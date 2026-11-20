use crate::error::GatewayError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 网关内部统一请求模型（与任何外部厂商协议解耦）
#[derive(Debug, Clone)]
pub struct UnifiedRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<UnifiedMessage>,
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedMessage {
    pub role: String,
    pub content: String,
}

/// 统一的 Token 审计统计
#[derive(Debug, Default, Clone)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// 统一的流式事件（屏蔽不同厂商的 SSE / NDJSON 格式差异）
#[derive(Debug)]
pub enum UnifiedStreamEvent {
    ContentDelta(String),
    Usage(TokenUsage),
    Done,
}

impl UnifiedRequest {
    /// 入站翻译：将客户端发来的 OpenAI 格式 JSON 解析为网关内部 IR
    pub fn from_openai_json(payload: &Value) -> Result<Self, GatewayError> {
        let model = payload
            .get("model")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::BadRequest("缺少必填字段: model".to_string()))?
            .to_string();

        let stream = payload
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let max_tokens = payload
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let temperature = payload
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);

        let raw_messages = payload
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| GatewayError::BadRequest("缺少必填字段: messages".to_string()))?;

        let mut system_parts = Vec::new();
        let mut messages = Vec::new();

        for msg in raw_messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");

            if role == "system" {
                system_parts.push(content.to_string());
            } else {
                messages.push(UnifiedMessage {
                    role: role.to_string(),
                    content: content.to_string(),
                });
            }
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n"))
        };

        Ok(Self {
            model,
            system,
            messages,
            stream,
            max_tokens,
            temperature,
        })
    }
}

/// 出站翻译：将网关统一的文本增量事件包装为标准 OpenAI SSE 帧
pub fn format_openai_sse_chunk(model: &str, content: &str) -> String {
    let chunk = json!({
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{
            "index": 0,
            "delta": { "content": content },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\n", chunk)
}