use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use serde_json::{json, Value};
use crate::error::GatewayError;
use crate::ir::{TokenUsage, UnifiedRequest, UnifiedStreamEvent};
use super::LlmProvider;

pub struct AnthropicProvider {
    pub api_key: String,
    pub base_url: String, // 默认 https://api.anthropic.com/v1/messages
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    /// 请求翻译：OpenAI IR -> Anthropic Messages API
    fn build_request(&self, client: &Client, req: &UnifiedRequest) -> Result<RequestBuilder, GatewayError> {
        let mut body = json!({
            "model": req.model,
            "messages": req.messages,
            "stream": req.stream,
            // Anthropic 强制要求 max_tokens，若客户端未传则给默认值
            "max_tokens": req.max_tokens.unwrap_or(4096),
        });

        // Anthropic 的 system 提示词必须放在顶层字段
        if let Some(sys) = &req.system {
            body["system"] = json!(sys);
        }
        if let Some(temp) = req.temperature {
            body["temperature"] = json!(temp);
        }

        let builder = client
            .post(&self.base_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body);

        Ok(builder)
    }

    /// 非流式响应翻译：Anthropic JSON -> OpenAI JSON
    fn parse_response(&self, raw: Value, model: &str) -> Result<(Value, u64), GatewayError> {
        let content = raw["content"][0]["text"].as_str().unwrap_or("");
        let input_tokens = raw["usage"]["input_tokens"].as_u64().unwrap_or(0);
        let output_tokens = raw["usage"]["output_tokens"].as_u64().unwrap_or(0);
        let total = input_tokens + output_tokens;

        let openai_resp = json!({
            "id": raw["id"].as_str().unwrap_or("chatcmpl-anthropic"),
            "object": "chat.completion",
            "model": model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": input_tokens,
                "completion_tokens": output_tokens,
                "total_tokens": total
            }
        });

        Ok((openai_resp, total))
    }

    /// 流式翻译：Anthropic SSE -> UnifiedStreamEvent
    fn parse_stream_line(&self, line: &str) -> Option<UnifiedStreamEvent> {
        let data = line.strip_prefix("data: ")?;
        let json: Value = serde_json::from_str(data).ok()?;
        let event_type = json["type"].as_str()?;

        match event_type {
            // 文本增量
            "content_block_delta" => {
                let text = json["delta"]["text"].as_str()?;
                Some(UnifiedStreamEvent::ContentDelta(text.to_string()))
            }
            // 输入 Token 在 message_start 中返回
            "message_start" => {
                let input = json["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0);
                Some(UnifiedStreamEvent::Usage(TokenUsage {
                    prompt_tokens: input,
                    ..Default::default()
                }))
            }
            // 输出 Token 在 message_delta 中返回
            "message_delta" => {
                let output = json["usage"]["output_tokens"].as_u64().unwrap_or(0);
                Some(UnifiedStreamEvent::Usage(TokenUsage {
                    completion_tokens: output,
                    ..Default::default()
                }))
            }
            "message_stop" => Some(UnifiedStreamEvent::Done),
            _ => None,
        }
    }
}