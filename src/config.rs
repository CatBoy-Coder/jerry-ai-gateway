use std::collections::HashSet;
use std::env;

/// 单个上游模型通道的动态配置
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// 通道标识（如 DASHSCOPE, DEEPSEEK, GEMINI, MOONSHOT）
    pub id: String,
    /// 协议类型：openai（默认）或 gemini
    pub protocol: String,
    pub api_key: String,
    pub base_url: String,
    /// 该通道作为默认或降级备胎时使用的模型名
    pub default_model: String,
    /// 模型名称路由前缀（如 qwen-, deepseek-, gemini-）
    pub model_prefix: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub port: u16,
    pub gateway_api_key: String,
    pub rate_limit_per_sec: u64,
    /// 按优先级从高到低排列的上游通道列表
    pub providers: Vec<ProviderConfig>,
}

impl AppConfig {
    pub fn from_env() -> Self {
        // 1. 强制用 .env 覆盖当前进程环境变量（避免本地开发时被系统残留变量干扰）
        let _ = dotenvy::dotenv_override();

        let port = env::var("GATEWAY_PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse::<u16>()
            .expect("GATEWAY_PORT 必须是合法的端口号");

        let gateway_api_key = env::var("GATEWAY_API_KEY")
            .unwrap_or_else(|_| "sk-jerry-gateway-local-dev-key".to_string());

        let rate_limit_per_sec = env::var("RATE_LIMIT_PER_SEC")
            .unwrap_or_else(|_| "2".to_string())
            .parse::<u64>()
            .unwrap_or(2);

        // 2. 收集有序的通道 ID 列表
        let ordered_ids = discover_ordered_provider_ids();

        // 3. 逐个解析通道配置
        let mut providers = Vec::new();
        for id in ordered_ids {
            if let Some(cfg) = load_provider_config(&id) {
                providers.push(cfg);
            }
        }

        if providers.is_empty() {
            panic!("❌ 启动失败：未在 .env 或环境变量中发现任何有效的上游 *_API_KEY 配置！");
        }

        Self {
            port,
            gateway_api_key,
            rate_limit_per_sec,
            providers,
        }
    }
}

/// 按约定发现通道顺序：
/// 优先读取 FALLBACK_ORDER -> 其次按 .env 文件从上至下顺序 -> 最后兜底扫描系统环境变量
fn discover_ordered_provider_ids() -> Vec<String> {
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();

    // 规则 A：若显式配置了 FALLBACK_ORDER（适合 Docker/K8s 无 .env 场景）
    if let Ok(order_str) = env::var("FALLBACK_ORDER") {
        for raw in order_str.split(',') {
            let id = raw.trim().to_uppercase();
            if !id.is_empty() && id != "GATEWAY" && seen.insert(id.clone()) {
                ordered.push(id);
            }
        }
    }

    // 规则 B：按 .env 文件从上至下的物理行序扫描 *_API_KEY
    if let Ok(iter) = dotenvy::from_path_iter(".env") {
        for item in iter.flatten() {
            let (key, val) = item;
            if val.trim().is_empty() {
                continue;
            }
            if let Some(prefix) = key.strip_suffix("_API_KEY") {
                let id = prefix.to_uppercase();
                if id != "GATEWAY" && seen.insert(id.clone()) {
                    ordered.push(id);
                }
            }
        }
    }

    // 规则 C：兜底扫描系统环境变量中其余的 *_API_KEY
    for (key, val) in env::vars() {
        if val.trim().is_empty() {
            continue;
        }
        if let Some(prefix) = key.strip_suffix("_API_KEY") {
            let id = prefix.to_uppercase();
            if id != "GATEWAY" && seen.insert(id.clone()) {
                ordered.push(id);
            }
        }
    }

    ordered
}

/// 根据通道 ID 加载具体配置（内置常见厂商的合理默认值，自定义厂商按规范读取）
fn load_provider_config(id: &str) -> Option<ProviderConfig> {
    let api_key = env::var(format!("{}_API_KEY", id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let protocol = env::var(format!("{}_PROTOCOL", id))
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if id == "GEMINI" {
                "gemini".to_string()
            } else {
                "openai".to_string()
            }
        });

    let base_url = env::var(format!("{}_BASE_URL", id))
        .or_else(|_| {
            // 兼容旧版配置名（UPSTREAM_URL / DEEPSEEK_URL）
            if id == "DASHSCOPE" {
                env::var("UPSTREAM_URL")
            } else {
                env::var(format!("{}_URL", id))
            }
        })
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| match id {
            "DASHSCOPE" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions".to_string()),
            "DEEPSEEK" => Some("https://api.deepseek.com/chat/completions".to_string()),
            "GEMINI" => Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
            _ => None,
        });

    let Some(base_url) = base_url else {
        eprintln!("⚠️ 忽略通道 {}: 已配置 {}_API_KEY 但缺少 {}_BASE_URL", id, id, id);
        return None;
    };

    let default_model = env::var(format!("{}_DEFAULT_MODEL", id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| match id {
            "DASHSCOPE" => Some("qwen-plus".to_string()),
            "DEEPSEEK" => Some("deepseek-chat".to_string()),
            "GEMINI" => Some("gemini-2.5-flash".to_string()),
            _ => None,
        });

    let Some(default_model) = default_model else {
        eprintln!("⚠️ 忽略通道 {}: 缺少 {}_DEFAULT_MODEL", id, id);
        return None;
    };

    let model_prefix = env::var(format!("{}_MODEL_PREFIX", id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| match id {
            "DASHSCOPE" => Some("qwen-".to_string()),
            "DEEPSEEK" => Some("deepseek-".to_string()),
            "GEMINI" => Some("gemini-".to_string()),
            _ => None,
        });

    Some(ProviderConfig {
        id: id.to_string(),
        protocol,
        api_key,
        base_url,
        default_model,
        model_prefix,
    })
}