use super::LlmProvider;
use crate::error::GatewayError;
use crate::ir::{TokenUsage, UnifiedRequest, UnifiedStreamEvent};
use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use serde_json::{json, Value};

pub struct GeminiProvider {
    pub api_key: String,
    /// 默认 https://generativelanguage.googleapis.com/v1beta
    pub base_url: String,
}

impl GeminiProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn name(&self) -> &'static str {
        "google-gemini"
    }

    /// 1. 请求翻译：UnifiedRequest -> Gemini generateContent / streamGenerateContent
    fn build_request(
        &self,
        client: &Client,
        req: &UnifiedRequest,
    ) -> Result<RequestBuilder, GatewayError> {
        // Gemini 的模型名拼在 URL 路径中，流式需追加 ?alt=sse
        let url = if req.stream {
            format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                self.base_url, req.model
            )
        } else {
            format!("{}/models/{}:generateContent", self.base_url, req.model)
        };

        // 角色映射：OpenAI 的 "assistant" 在 Gemini 中叫 "model"
        let contents: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let role = if m.role == "assistant" { "model" } else { "user" };
                json!({
                    "role": role,
                    "parts": [{ "text": m.content }]
                })
            })
            .collect();

        let mut body = json!({ "contents": contents });

        // 系统提示词映射为顶层 systemInstruction
        if let Some(sys) = &req.system {
            body["systemInstruction"] = json!({
                "parts": [{ "text": sys }]
            });
        }

        // 生成参数映射为 generationConfig
        let mut gen_config = serde_json::Map::new();
        // 关闭 Gemini 2.5 默认的内部推理思考，大幅降低 Token 消耗与首字节延迟
        gen_config.insert(
            "thinkingConfig".to_string(),
            json!({ "thinkingBudget": 0 }),
        );
        if let Some(max_tokens) = req.max_tokens {
            gen_config.insert("maxOutputTokens".to_string(), json!(max_tokens));
        }
        if let Some(temp) = req.temperature {
            gen_config.insert("temperature".to_string(), json!(temp));
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = Value::Object(gen_config);
        }

        let builder = client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body);

        Ok(builder)
    }

    /// 2. 非流式响应翻译：Gemini JSON -> 标准 OpenAI JSON + TokenUsage
    fn parse_response(
        &self,
        raw_json: Value,
        model: &str,
    ) -> Result<(Value, TokenUsage), GatewayError> {
        let content = raw_json
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let usage = extract_gemini_usage(&raw_json);

        let openai_resp = json!({
            "id": "chatcmpl-gemini",
            "object": "chat.completion",
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens
            }
        });

        Ok((openai_resp, usage))
    }

    /// 3. 流式帧翻译：Gemini SSE -> UnifiedStreamEvent
    fn parse_stream_line(&self, line: &str) -> Vec<UnifiedStreamEvent> {
        let mut events = Vec::new();
        let Some(data) = line.strip_prefix("data: ") else {
            return events;
        };

        let Ok(json) = serde_json::from_str::<Value>(data) else {
            return events;
        };

        // 提取文本增量
        if let Some(text) = json
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|v| v.as_str())
        {
            if !text.is_empty() {
                events.push(UnifiedStreamEvent::ContentDelta(text.to_string()));
            }
        }

        // 提取 Token 审计数据（Gemini 会在流末尾帧附带完整 usageMetadata）
        if json.get("usageMetadata").is_some() {
            let usage = extract_gemini_usage(&json);
            if usage.total_tokens > 0 {
                events.push(UnifiedStreamEvent::Usage(usage));
            }
        }

        events
    }
}

fn extract_gemini_usage(json: &Value) -> TokenUsage {
    let prompt = json
        .pointer("/usageMetadata/promptTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = json
        .pointer("/usageMetadata/candidatesTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = json
        .pointer("/usageMetadata/totalTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(prompt + completion);

    TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
    }
}