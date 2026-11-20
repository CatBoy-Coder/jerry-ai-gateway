mod config;
mod error;
mod ir;
mod middleware;
mod provider;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use config::{AppConfig, ProviderConfig};
use error::GatewayError;
use futures_util::StreamExt;
use governor::{Quota, RateLimiter};
use ir::{format_openai_sse_chunk, TokenUsage, UnifiedRequest, UnifiedStreamEvent};
use provider::{gemini::GeminiProvider, openai::OpenAiProvider, LlmProvider};
use reqwest::Client;
use serde_json::{json, Value};
use std::num::NonZeroU32;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{error, info, warn};

/// 上游首字节响应超时时间（超时则触发自动降级）
const UPSTREAM_TTFB_TIMEOUT_SECS: u64 = 15;

/// 运行时通道实例（配置 + 协议适配器）
#[derive(Clone)]
pub struct ProviderEntry {
    pub config: ProviderConfig,
    pub adapter: Arc<dyn LlmProvider>,
}

pub struct AppState {
    pub http_client: Client,
    pub config: AppConfig,
    pub rate_limiter: governor::DefaultDirectRateLimiter,
    /// 按 .env 从上至下顺序加载的通道列表
    pub providers: Vec<ProviderEntry>,
}

impl AppState {
    /// 根据请求的模型名称，动态构建本次请求的降级候选链路
    pub fn build_fallback_chain(
        &self,
        requested_model: &str,
    ) -> Result<Vec<(String, ProviderEntry)>, GatewayError> {
        let mut chain: Vec<(String, ProviderEntry)> = Vec::new();
        let mut used_ids: Vec<String> = Vec::new();

        // 1. 若客户端指定了具体模型（非 auto），优先匹配对应前缀或默认模型的通道放第一位
        if requested_model != "auto" {
            let matched = self.providers.iter().find(|entry| {
                entry.config.default_model == requested_model
                    || entry
                    .config
                    .model_prefix
                    .as_deref()
                    .map(|p| requested_model.starts_with(p))
                    .unwrap_or(false)
            });

            if let Some(entry) = matched {
                used_ids.push(entry.config.id.clone());
                chain.push((requested_model.to_string(), entry.clone()));
            } else if let Some(primary) = self.providers.first() {
                // 未匹配到特定前缀时，交给顺位 1 的默认通道尝试
                used_ids.push(primary.config.id.clone());
                chain.push((requested_model.to_string(), primary.clone()));
            }
        }

        // 2. 严格按 .env 从上至下的顺序追加剩余通道作为降级备胎
        for entry in &self.providers {
            if !used_ids.contains(&entry.config.id) {
                used_ids.push(entry.config.id.clone());
                chain.push((entry.config.default_model.clone(), entry.clone()));
            }
        }

        if chain.is_empty() {
            return Err(GatewayError::InternalError(
                "网关未配置任何可用的上游模型通道".to_string(),
            ));
        }

        Ok(chain)
    }
}

/// 根据协议类型实例化对应的适配器
fn build_adapter(cfg: &ProviderConfig) -> Arc<dyn LlmProvider> {
    match cfg.protocol.as_str() {
        "gemini" => Arc::new(GeminiProvider::new(
            cfg.api_key.clone(),
            cfg.base_url.clone(),
        )),
        _ => Arc::new(OpenAiProvider::new(
            cfg.api_key.clone(),
            cfg.base_url.clone(),
        )),
    }
}

/// 判断上游 HTTP 状态码是否适合触发自动降级
fn should_fallback(status: reqwest::StatusCode) -> bool {
    let code = status.as_u16();
    // 401(Key失效) / 402(欠费) / 403(额度耗尽) / 404(端点或模型不存在) / 429(限流) / 5xx(服务端异常)
    matches!(code, 401 | 402 | 403 | 404 | 429) || status.is_server_error()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = AppConfig::from_env();
    let bind_addr = format!("0.0.0.0:{}", config.port);

    let quota = Quota::per_second(NonZeroU32::new(config.rate_limit_per_sec as u32).unwrap());
    let rate_limiter = RateLimiter::direct(quota);

    let providers: Vec<ProviderEntry> = config
        .providers
        .iter()
        .map(|cfg| ProviderEntry {
            config: cfg.clone(),
            adapter: build_adapter(cfg),
        })
        .collect();

    let chain_desc: Vec<String> = providers
        .iter()
        .map(|p| format!("{}({})", p.config.id, p.config.default_model))
        .collect();

    info!(
        "🛡️ 动态降级链路已就绪 (按 .env 顺序): {}",
        chain_desc.join(" -> ")
    );

    let shared_state = Arc::new(AppState {
        http_client: Client::new(),
        config,
        rate_limiter,
        providers,
    });

    let app = Router::new()
        .route("/health", get(health_check))
        .route(
            "/v1/chat/completions",
            post(proxy_chat_completions)
                .route_layer(axum::middleware::from_fn_with_state(
                    shared_state.clone(),
                    middleware::rate_limit_middleware,
                ))
                .route_layer(axum::middleware::from_fn_with_state(
                    shared_state.clone(),
                    middleware::auth_middleware,
                )),
        )
        .with_state(shared_state);

    info!(
        "🚀 jerry-ai-gateway v0.5.0 启动成功！监听地址: http://{}",
        bind_addr
    );

    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health_check() -> impl IntoResponse {
    Json(json!({
        "status": "UP",
        "gateway": "jerry-ai-gateway",
        "version": "0.5.0"
    }))
}

async fn proxy_chat_completions(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> Result<Response, GatewayError> {
    let start_time = Instant::now();

    // 1. 入站翻译：解析为网关独立的 UnifiedRequest
    let base_req = UnifiedRequest::from_openai_json(&payload)?;

    // 2. 构建动态降级候选链路
    let candidates = state.build_fallback_chain(&base_req.model)?;
    let total_candidates = candidates.len();

    let mut active_response: Option<reqwest::Response> = None;
    let mut active_entry: Option<ProviderEntry> = None;
    let mut active_model = String::new();
    let mut last_error: Option<GatewayError> = None;

    // 3. 依次尝试候选通道，触发可重试异常时自动降级
    for (idx, (target_model, entry)) in candidates.into_iter().enumerate() {
        let mut req_for_candidate = base_req.clone();
        req_for_candidate.model = target_model.clone();

        info!(
            ">>> 发起推理请求 ({}/{}) | 通道: {} [{}] | 目标模型: {} | 流式: {}",
            idx + 1,
            total_candidates,
            entry.config.id,
            entry.adapter.name(),
            target_model,
            req_for_candidate.stream
        );

        let request_builder = entry
            .adapter
            .build_request(&state.http_client, &req_for_candidate)?;

        let send_result = tokio::time::timeout(
            Duration::from_secs(UPSTREAM_TTFB_TIMEOUT_SECS),
            request_builder.send(),
        )
            .await;

        match send_result {
            Ok(Ok(resp)) => {
                let status = resp.status();
                if status.is_success() {
                    active_response = Some(resp);
                    active_entry = Some(entry);
                    active_model = target_model;
                    break;
                }

                let axum_status = StatusCode::from_u16(status.as_u16())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let resp_body: Value = resp.json().await.unwrap_or_default();
                let err = GatewayError::UpstreamApiError(axum_status, resp_body);

                if should_fallback(status) && idx + 1 < total_candidates {
                    warn!(
                        "⚠️ 通道 {} (模型: {}) 返回 HTTP {}，触发自动降级至下一顺位！",
                        entry.config.id,
                        target_model,
                        status.as_u16()
                    );
                    last_error = Some(err);
                    continue;
                } else {
                    error!(
                        "❌ 通道 {} (模型: {}) 返回不可降级错误 HTTP {}，直接终止请求",
                        entry.config.id,
                        target_model,
                        status.as_u16()
                    );
                    return Err(err);
                }
            }
            Ok(Err(e)) => {
                warn!(
                    "⚠️ 通道 {} (模型: {}) 网络请求失败: {}，准备降级...",
                    entry.config.id, target_model, e
                );
                last_error = Some(GatewayError::from(e));
                continue;
            }
            Err(_) => {
                warn!(
                    "⚠️ 通道 {} (模型: {}) 首字节响应超过 {}s 超时，触发自动降级！",
                    entry.config.id, target_model, UPSTREAM_TTFB_TIMEOUT_SECS
                );
                last_error = Some(GatewayError::InternalError(format!(
                    "上游模型 {} 响应超时",
                    target_model
                )));
                continue;
            }
        }
    }

    let response = match active_response {
        Some(r) => r,
        None => {
            return Err(last_error.unwrap_or_else(|| {
                GatewayError::InternalError("所有上游模型通道均不可用".to_string())
            }));
        }
    };

    let entry = active_entry.unwrap();
    let provider_id = entry.config.id.clone();
    let provider = entry.adapter.clone();
    let status = response.status();

    // 4. 双轨响应处理（统一翻译回 OpenAI 协议）
    if base_req.stream {
        let mut upstream_stream = response.bytes_stream();
        let stream_model = active_model.clone();
        let stream_provider = provider.clone();
        let stream_provider_id = provider_id.clone();

        let stream_body = async_stream::stream! {
            let mut line_buf: Vec<u8> = Vec::new();
            let mut final_usage = TokenUsage::default();
            let mut sent_done = false;
            let stream_start = Instant::now();

            while let Some(chunk_result) = upstream_stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        line_buf.extend_from_slice(&bytes);

                        while let Some(pos) = line_buf.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = line_buf.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
                            if line.is_empty() {
                                continue;
                            }

                            for event in stream_provider.parse_stream_line(&line) {
                                match event {
                                    UnifiedStreamEvent::ContentDelta(text) => {
                                        let sse = format_openai_sse_chunk(&stream_model, &text);
                                        yield Ok::<_, reqwest::Error>(sse);
                                    }
                                    UnifiedStreamEvent::Usage(u) => {
                                        if u.prompt_tokens > 0 {
                                            final_usage.prompt_tokens = u.prompt_tokens;
                                        }
                                        if u.completion_tokens > 0 {
                                            final_usage.completion_tokens = u.completion_tokens;
                                        }
                                        final_usage.total_tokens = if u.total_tokens > 0 {
                                            u.total_tokens
                                        } else {
                                            final_usage.prompt_tokens + final_usage.completion_tokens
                                        };
                                    }
                                    UnifiedStreamEvent::Done => {
                                        if !sent_done {
                                            sent_done = true;
                                            yield Ok::<_, reqwest::Error>("data: [DONE]\n\n".to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => yield Err(e),
                }
            }

            if !sent_done {
                yield Ok::<_, reqwest::Error>("data: [DONE]\n\n".to_string());
            }

            let elapsed = stream_start.elapsed().as_millis();
            if final_usage.total_tokens > 0 {
                tracing::info!(
                    "<<< [流式审计入账] 通道: {} | 模型: {} | 耗时: {} ms | Token: {} (输入 {} / 输出 {}) 💰",
                    stream_provider_id,
                    stream_model,
                    elapsed,
                    final_usage.total_tokens,
                    final_usage.prompt_tokens,
                    final_usage.completion_tokens
                );
            } else {
                tracing::warn!("<<< [流式审计] 通道: {} 未提取到 Token 统计", stream_provider_id);
            }
        };

        let mut res = Response::new(Body::from_stream(stream_body));
        *res.status_mut() = status;
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        );

        info!(
            "<<< 流式通道已建立 | 通道: {} | 模型: {} | TTFB: {} ms",
            provider_id,
            active_model,
            start_time.elapsed().as_millis()
        );
        Ok(res)
    } else {
        let raw_body: Value = response.json().await?;
        let (openai_body, usage) = provider.parse_response(raw_body, &active_model)?;

        info!(
            "<<< 非流式推理完成 | 通道: {} | 模型: {} | 耗时: {} ms | Token: {} (输入 {} / 输出 {})",
            provider_id,
            active_model,
            start_time.elapsed().as_millis(),
            usage.total_tokens,
            usage.prompt_tokens,
            usage.completion_tokens
        );

        let mut res = Json(openai_body).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        Ok(res)
    }
}