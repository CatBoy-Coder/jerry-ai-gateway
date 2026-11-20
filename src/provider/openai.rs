use super::LlmProvider;
use crate::error::GatewayError;
use crate::ir::{TokenUsage, UnifiedRequest, UnifiedStreamEvent};
use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use serde_json::{json, Value};

pub struct OpenAiProvider {
    pub api_key: String,
    pub upstream_url: String,
}

impl OpenAiProvider {
    pub fn new(api_key: String, upstream_url: String) -> Self {
        Self {
            api_key,
            upstream_url,
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai-compatible"
    }

    fn build_request(
        &self,
        client: &Client,
        req: &UnifiedRequest,
    ) -> Result<RequestBuilder, GatewayError> {
        // 将独立的 system 字段还原为 OpenAI messages 的第一条
        let mut messages = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(json!({ "role": "system", "content": sys }));
        }
        for m in &req.messages {
            messages.push(json!({ "role": m.role, "content": m.content }));
        }

        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "stream": req.stream,
        });

        if let Some(max_tokens) = req.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(temp) = req.temperature {
            body["temperature"] = json!(temp);
        }
        // 流式模式下自动注入 include_usage，确保审计闭环
        if req.stream {
            body["stream_options"] = json!({ "include_usage": true });
        }

        let builder = client
            .post(&self.upstream_url)
            .bearer_auth(&self.api_key)
            .json(&body);

        Ok(builder)
    }

    fn parse_response(
        &self,
        raw_json: Value,
        _model: &str,
    ) -> Result<(Value, TokenUsage), GatewayError> {
        let usage = TokenUsage {
            prompt_tokens: raw_json
                .pointer("/usage/prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            completion_tokens: raw_json
                .pointer("/usage/completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            total_tokens: raw_json
                .pointer("/usage/total_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        };
        Ok((raw_json, usage))
    }

    fn parse_stream_line(&self, line: &str) -> Vec<UnifiedStreamEvent> {
        let mut events = Vec::new();
        let Some(data) = line.strip_prefix("data: ") else {
            return events;
        };

        if data.trim() == "[DONE]" {
            events.push(UnifiedStreamEvent::Done);
            return events;
        }

        let Ok(json) = serde_json::from_str::<Value>(data) else {
            return events;
        };

        // 提取文本增量
        if let Some(content) = json.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                events.push(UnifiedStreamEvent::ContentDelta(content.to_string()));
            }
        }

        // 提取 Token 审计数据
        if let Some(usage) = json.get("usage") {
            if !usage.is_null() {
                events.push(UnifiedStreamEvent::Usage(TokenUsage {
                    prompt_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
                    completion_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
                    total_tokens: usage["total_tokens"].as_u64().unwrap_or(0),
                }));
            }
        }

        events
    }
}