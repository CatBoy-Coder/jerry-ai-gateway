# 🚀 jerry-ai-gateway

一个基于 **Rust + Axum** 构建的高性能、轻量级企业级 AI 代理网关（当前版本：`v0.5.0`）。

对外统一暴露标准 **OpenAI Chat Completions 协议**（`POST /v1/chat/completions`），
对内采用 **独立中间层（Unified IR）+ 插件化协议适配器** 架构，支持跨协议转换、
**约定大于配置** 的动态通道发现、故障自动降级，以及零延迟流式 Token 审计。

---

## ✨ 核心特性

- **独立中间层与跨协议转换**
  内置强类型 `UnifiedRequest` / `UnifiedStreamEvent` 中间表示，屏蔽上游协议差异。
  客户端只需使用标准 OpenAI 协议，即可无缝调用阿里云百炼、DeepSeek 及原生 Google Gemini。
- **约定大于配置（`.env` 顺序即优先级）**
  启动时按 `.env` 文件**从上至下的物理行序**自动扫描 `*_API_KEY`，动态注册通道并排定降级顺位。
  接入任意 OpenAI 兼容厂商（Kimi / 智谱 / 硅基流动 / OpenRouter），仅需在 `.env` 追加 4 行配置，**无需修改 Rust 代码**。
- **高可用自动降级（Fallback）**
  支持 `"model": "auto"` 按 `.env` 顺位自动调度，也支持指定模型并在故障时自动兜底。
  遇到 **15s 首字节超时**、网络异常或 **HTTP `401 / 402 / 403 / 404 / 429 / 5xx`** 时无缝切换下一顺位；
  客户端自身参数错误（`400`）直接返回，不浪费备用额度。
- **零延迟流式透传与 Token 审计**
  基于 `async-stream` 实现旁路拦截与字节级按行切分（防止 TCP 分包截断中文或 JSON），
  自动注入 `stream_options`，在不阻塞打字机流的前提下精准记录 TTFB、总耗时及输入/输出 Token。
- **成本优化**
  针对 Gemini 2.5 系列默认关闭内部推理思考（`thinkingBudget: 0`），大幅降低隐形 Token 消耗与首字节延迟。
- **安全治理底座**
  内置 Axum 鉴权中间件（`Bearer Token`）与 `governor` 无锁令牌桶限流器，保护上游密钥与账单安全。

---

## 🏗️ 项目目录结构

```text
src/
├── main.rs
├── config.rs
├── error.rs
├── middleware.rs
├── ir.rs
└── provider/
    ├── mod.rs
    ├── openai.rs
    └── gemini.rs
```

| 文件路径 | 核心职责 |
| :--- | :--- |
| `src/main.rs` | 启动入口、动态路由与故障降级引擎、流式按行翻译与审计 |
| `src/config.rs` | 约定式配置加载器（按 `.env` 行序 / `FALLBACK_ORDER` 发现通道） |
| `src/error.rs` | 统一错误枚举与 HTTP 状态码映射 |
| `src/middleware.rs` | Bearer Token 鉴权与全局令牌桶限流中间件 |
| `src/ir.rs` | 网关独立中间层（`UnifiedRequest` / `TokenUsage` / SSE 封装） |
| `src/provider/mod.rs` | `LlmProvider` Trait 协议契约定义 |
| `src/provider/openai.rs` | OpenAI 兼容协议适配器（百炼、DeepSeek、Kimi 等通用） |
| `src/provider/gemini.rs` | Google Gemini 原生协议双向翻译适配器 |

---

## ⚙️ 环境配置（约定大于配置）

在项目根目录复制 `.env.example` 并重命名为 `.env`：

```env
# === 基础网关配置 ===
GATEWAY_PORT=3000
GATEWAY_API_KEY=sk-jerry-gateway-local-dev-key
RATE_LIMIT_PER_SEC=2

# 【可选】无 .env 文件部署（如 Docker/K8s）时显式指定顺位
# FALLBACK_ORDER=DASHSCOPE,DEEPSEEK,GEMINI

# === 上游模型通道（按从上至下顺序排定默认与降级链路） ===

# [顺位 1 - 默认主力] 阿里云百炼
DASHSCOPE_API_KEY=sk-your-dashscope-api-key
DASHSCOPE_BASE_URL=[https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions](https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions)
DASHSCOPE_DEFAULT_MODEL=qwen-plus
DASHSCOPE_MODEL_PREFIX=qwen-

# [顺位 2 - 第一备胎] DeepSeek
DEEPSEEK_API_KEY=sk-your-deepseek-api-key
DEEPSEEK_BASE_URL=[https://api.deepseek.com/chat/completions](https://api.deepseek.com/chat/completions)
DEEPSEEK_DEFAULT_MODEL=deepseek-chat
DEEPSEEK_MODEL_PREFIX=deepseek-

# [顺位 3 - 终极兜底] Google Gemini（跨协议转换）
GEMINI_API_KEY=AIza-your-gemini-api-key
GEMINI_BASE_URL=[https://generativelanguage.googleapis.com/v1beta](https://generativelanguage.googleapis.com/v1beta)
GEMINI_DEFAULT_MODEL=gemini-2.5-flash
GEMINI_MODEL_PREFIX=gemini-
GEMINI_PROTOCOL=gemini
```

### 🔌 约定规则说明

网关会自动识别所有 `<通道名>_API_KEY`（自动排除 `GATEWAY_API_KEY`），并读取以下配套变量：

| 配置项 | 是否必填 | 说明 |
| :--- | :--- | :--- |
| `<名称>_API_KEY` | 必填 | 上游厂商密钥，出现顺序即默认降级顺位 |
| `<名称>_BASE_URL` | 必填 | 上游接口地址（已内置百炼 / DeepSeek / Gemini 默认值） |
| `<名称>_DEFAULT_MODEL` | 必填 | `auto` 模式或降级兜底时使用的默认模型名 |
| `<名称>_MODEL_PREFIX` | 可选 | 模型名称前缀（如 `qwen-`），用于精准路由 |
| `<名称>_PROTOCOL` | 可选 | 协议类型，默认 `openai`，跨协议支持 `gemini` |

- **顺位优先级**：`FALLBACK_ORDER`（若配置） > `.env` 从上至下物理行序 > 系统环境变量。
- **零代码扩展新厂商**：例如接入 Kimi（Moonshot），只需在 `.env` 任意顺位插入以下 4 行并重启：

```env
MOONSHOT_API_KEY=sk-your-moonshot-api-key
MOONSHOT_BASE_URL=[https://api.moonshot.cn/v1/chat/completions](https://api.moonshot.cn/v1/chat/completions)
MOONSHOT_DEFAULT_MODEL=moonshot-v1-8k
MOONSHOT_MODEL_PREFIX=moonshot-
```

### ⚠️ `.env` 注意事项

- **严禁提交至仓库**：确保 `.env` 已加入 `.gitignore`，仅提交脱敏后的 `.env.example`。
- **至少配置一个通道**：未检测到任何有效的上游 `*_API_KEY` 时，网关启动会报错退出。
- **本地覆盖优先级**：内部使用 `dotenvy::dotenv_override()`，`.env` 会覆盖系统同名环境变量；修改后需重启服务。
- **生产密钥安全**：上线前请务必替换默认的 `GATEWAY_API_KEY`。
- **文件编码**：Windows 下请保存为 **UTF-8（无 BOM）**，且 `=` 两侧不要留多余空格。

---

## 🚀 快速启动

```bash
# 1. 克隆项目
git clone <your-repo-url>
cd jerry-ai-gateway

# 2. 创建并配置 .env
cp .env.example .env

# 3. 编译并运行
cargo run
```

启动成功后，控制台会打印动态识别出的降级链路：

```text
INFO jerry_ai_gateway: 🛡️ 动态降级链路