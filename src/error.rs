use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::error;

#[derive(Debug, Error)]
pub enum GatewayError {
    // #[from] 会自动生成 impl From<reqwest::Error> for GatewayError！
    #[error("上游 AI 服务网络请求异常: {0}")]
    UpstreamNetwork(#[from] reqwest::Error),

    #[error("上游 AI 服务返回错误状态码: {0}")]
    UpstreamApiError(StatusCode, Value),

    #[error("请求参数缺失或非法: {0}")]
    BadRequest(String),
}

// 实现 Axum 的 IntoResponse Trait
// 只要实现了它，Handler 函数就可以直接返回 Result<Json<Value>, GatewayError>！
impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        match self {
            GatewayError::UpstreamNetwork(err) => {
                error!("❌ 网关转发网络故障: {}", err);
                let body = Json(json!({
                    "error": {
                        "message": format!("Bad Gateway: {}", err),
                        "type": "gateway_network_error"
                    }
                }));
                (StatusCode::BAD_GATEWAY, body).into_response()
            }
            GatewayError::UpstreamApiError(status, resp_body) => {
                // 原样透传上游大模型的报错状态码和 JSON 结构
                (status, Json(resp_body)).into_response()
            }
            GatewayError::BadRequest(msg) => {
                let body = Json(json!({
                    "error": {
                        "message": msg,
                        "type": "invalid_request_error"
                    }
                }));
                (StatusCode::BAD_REQUEST, body).into_response()
            }
        }
    }
}