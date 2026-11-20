pub mod openai;
pub mod gemini;

use crate::error::GatewayError;
use crate::ir::{TokenUsage, UnifiedRequest, UnifiedStreamEvent};
use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use serde_json::Value;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 厂商标识名（用于日志与审计）
    fn name(&self) -> &'static str;

    /// 1. 请求翻译：UnifiedRequest -> 厂商原生 HTTP 请求
    fn build_request(
        &self,
        client: &Client,
        req: &UnifiedRequest,
    ) -> Result<RequestBuilder, GatewayError>;

    /// 2. 非流式响应翻译：厂商原生 JSON -> 标准 OpenAI JSON + TokenUsage
    fn parse_response(&self, raw_json: Value, model: &str) -> Result<(Value, TokenUsage), GatewayError>;

    /// 3. 流式帧翻译：单行流文本 -> 一组网关统一流式事件
    fn parse_stream_line(&self, line: &str) -> Vec<UnifiedStreamEvent>;
}