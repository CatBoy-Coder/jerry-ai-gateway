use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use std::sync::Arc;
use tracing::warn;

use crate::AppState;

// =========================================================================
// 1. 统一鉴权中间件
// =========================================================================
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<impl IntoResponse, impl IntoResponse> {
    // 尝试从请求头中提取 Authorization: Bearer <key>
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));

    match auth_header {
        Some(token) if token == state.config.gateway_api_key => {
            // 鉴权通过，放行到下一个中间件或路由处理器
            Ok(next.run(req).await)
        }
        _ => {
            // 鉴权失败，直接短路拦截
            warn!("❌ 拦截到非法访问请求，Token 无效或缺失");
            let err_body = Json(json!({
                "error": {
                    "message": "Unauthorized: 必须提供有效的 Gateway API Key",
                    "type": "authentication_error"
                }
            }));
            Err((StatusCode::UNAUTHORIZED, err_body))
        }
    }
}

// =========================================================================
// 2. 并发限流中间件 (令牌桶)
// =========================================================================
pub async fn rate_limit_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<impl IntoResponse, impl IntoResponse> {
    // 尝试从全局限流器中获取令牌 (check() 是非阻塞的)
    if state.rate_limiter.check().is_err() {
        warn!("🚦 触发网关全局限流，拒绝请求");
        let err_body = Json(json!({
            "error": {
                "message": "Too Many Requests: 网关触发并发限流，请稍后重试",
                "type": "rate_limit_exceeded"
            }
        }));
        return Err((StatusCode::TOO_MANY_REQUESTS, err_body));
    }

    Ok(next.run(req).await)
}