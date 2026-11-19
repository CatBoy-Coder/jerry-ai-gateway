mod config;
mod error;
mod middleware;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use config::AppConfig;
use error::GatewayError;
use governor::{Quota, RateLimiter};
use reqwest::Client;
use serde_json::{json, Value};
use std::{sync::Arc, time::Instant};
use std::num::NonZeroU32;
use axum::response::Response;
use tracing::info;
use futures_util::StreamExt;

pub struct AppState {
    pub http_client: Client,
    pub config: AppConfig,
    // 全局限流器实例：由于基于内部原子操作，无需套 Mutex
    pub rate_limiter: governor::DefaultDirectRateLimiter,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // 1. 从环境变量/.env 加载强类型配置
    let config = AppConfig::from_env();
    let bind_addr = format!("0.0.0.0:{}", config.port);

    // 配置限流额度（基于配置文件，每秒补充 N 个 Token，桶最大容量也是 N）
    let quota = Quota::per_second(NonZeroU32::new(config.rate_limit_per_sec as u32).unwrap());
    let rate_limiter = RateLimiter::direct(quota);

    let shared_state = Arc::new(AppState {
        http_client: Client::new(),
        config,
        rate_limiter,
    });

    // 注意：Axum 中间件的执行顺序是自底向上（倒序）的！
    // 所以先通过 auth_middleware，再通过 rate_limit_middleware
    let app = Router::new()
        .route("/health", get(health_check))
        // 只有 /v1/chat/completions 需要被鉴权和限流保护
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

    info!("🚀 jerry-ai-gateway v0.2.0 启动成功！监听地址: http://{}", bind_addr);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health_check() -> impl IntoResponse {
    Json(json!({
        "status": "UP",
        "gateway": "jerry-ai-gateway",
        "version": "0.2.0"
    }))
}

// 将返回值类型改为 Result<Response, GatewayError>，以便统一下发 JSON 或 Stream
async fn proxy_chat_completions(
    State(state): State<Arc<AppState>>,
    Json(mut payload): Json<Value>,
) -> Result<Response, GatewayError> {
    let start_time = Instant::now();

    // 1. 提取核心参数
    let model_name = payload
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| GatewayError::BadRequest("缺少必填字段: model".to_string()))?
        .to_string();

    // 嗅探客户端是否请求了流式输出 (默认为 false)
    let is_stream = payload
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 核心补丁：如果前端请求的是流式，网关强行注入 include_usage 参数
    if is_stream {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "stream_options".to_string(),
                json!({"include_usage": true})
            );
        }
    }

    info!(">>> 收到推理请求 | 目标模型: {} | 流式: {}", model_name, is_stream);

    // 3. 向上游发起请求
    let response = state
        .http_client
        .post(&state.config.upstream_url)
        .bearer_auth(&state.config.dashscope_api_key)
        .json(&payload)
        .send()
        .await?;

    let status = response.status();

    // 如果上游报错，直接原样抛出
    if !status.is_success() {
        let axum_status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let resp_body: Value = response.json().await.unwrap_or_default();
        return Err(GatewayError::UpstreamApiError(axum_status, resp_body));
    }

    // 🌟 4. 双轨分发：流式 (SSE) vs 非流式 (JSON)
    if is_stream {
        // 拿到上游的原始字节流
        let mut upstream_stream = response.bytes_stream();

        // 🌟 修复核心：克隆一份所有权给即将"远行"的后台异步流
        let stream_model_name = model_name.clone();

        // 🌟 核心魔法：使用 async_stream 开启一个透明的旁路拦截器
        let stream_body = async_stream::stream! {
            let mut final_usage = None;
            let stream_start = Instant::now();

            // 逐个截获流经此处的字节块 (Chunk)
            while let Some(chunk_result) = upstream_stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        // 1. 旁路嗅探：尝试把字节转成字符串，看看有没有 Token 消耗数据
                        if let Ok(text) = std::str::from_utf8(&bytes) {
                            for line in text.lines() {
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if data != "[DONE]" {
                                        // 嗅探 JSON 里的 usage 字段 (百炼大模型通常在最后几个 Chunk 附带完整 usage)
                                        if let Ok(json) = serde_json::from_str::<Value>(data) {
                                            if let Some(usage) = json.get("usage") {
                                                if !usage.is_null() {
                                                    final_usage = Some(usage.clone());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // 2. 🌟 绝对零延迟：嗅探完立刻把原始字节原封不动地发给下游客户端！
                        yield Ok::<_, reqwest::Error>(bytes);
                    }
                    Err(e) => yield Err(e),
                }
            }

            // --- 🌟 水管彻底关闭，流式传输结束，触发结账 ---
            let elapsed = stream_start.elapsed().as_millis();
            if let Some(usage) = final_usage {
                let total_tokens = usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                // 🌟 修改点：这里使用被 Move 进来的克隆体 stream_model_name
                tracing::info!(
                    "<<< [流式审计入账] 模型: {} | 持续输出耗时: {} ms | 共消耗 Token: {} 💰",
                    stream_model_name, elapsed, total_tokens
                );
                // 💡 未来可在此处使用 tokio::spawn 开启独立的异步任务，把 total_tokens 写入 MySQL/Redis 进行扣费，绝不阻塞！
            } else {
                tracing::warn!("<<< [流式审计] 未能从流中提取到 Token 消耗数据 (可能是上游异常中断)");
            }
        };

        // 将包装了“透明水表”的流，塞回给 Axum
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

        info!("<<< 流式通道已建立并挂载审计水表 | 模型: {} | TTFB: {} ms", model_name, start_time.elapsed().as_millis());
        Ok(res)
    } else {
        // 【非流式处理】(保持之前的逻辑)
        let resp_body: Value = response.json().await?;
        let total_tokens = resp_body
            .pointer("/usage/total_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        info!("<<< 非流式推理完成 | 模型: {} | 耗时: {} ms | 消耗 Token: {}",
              model_name, start_time.elapsed().as_millis(), total_tokens);

        // 手动构造带有 UTF-8 的 JSON 响应
        let mut res = Json(resp_body).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );

        Ok(res)
    }
}