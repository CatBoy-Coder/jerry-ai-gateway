use std::env;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub port: u16,
    pub dashscope_api_key: String,
    pub upstream_url: String,
    pub gateway_api_key: String,
    pub rate_limit_per_sec: u64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        // 加载 .env 文件（如果存在）
        let _ = dotenvy::dotenv();

        let port = env::var("GATEWAY_PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse::<u16>()
            .expect("GATEWAY_PORT 必须是合法的端口号");

        let dashscope_api_key = env::var("DASHSCOPE_API_KEY")
            .expect("❌ 启动失败：未找到环境变量 DASHSCOPE_API_KEY，请在 .env 文件中配置！");

        let upstream_url = env::var("UPSTREAM_URL").unwrap_or_else(|_| {
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions".to_string()
        });

        // 默认网关 Key
        let gateway_api_key = env::var("GATEWAY_API_KEY")
            .unwrap_or_else(|_| "sk-jerry-gateway-local-dev-key".to_string());

        // 默认限流：每秒最多 2 个请求
        let rate_limit_per_sec = env::var("RATE_LIMIT_PER_SEC")
            .unwrap_or_else(|_| "2".to_string())
            .parse::<u64>()
            .unwrap_or(2);

        Self {
            port,
            dashscope_api_key,
            upstream_url,
            gateway_api_key,
            rate_limit_per_sec,
        }
    }
}