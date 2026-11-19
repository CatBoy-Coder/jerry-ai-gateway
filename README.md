```markdown
# 🚀 jerry-ai-gateway

一个基于 **Rust** 构建的高性能、轻量级企业级 AI 代理网关（兼容 OpenAI 协议）。专为大语言模型（LLM）API 的统一接入、安全治理与流量观测而设计，默认适配阿里云百炼（DashScope）兼容模式接口。

## ✨ 核心特性

- **双轨代理透传**：原生支持标准 JSON 非流式推理与 **SSE (Server-Sent Events)** 零缓冲流式打字机输出。
- **统一鉴权拦截**：基于 Axum 中间件实现 `Bearer Token` 校验，保护上游大模型凭证不直接暴露给前端或下游业务方。
- **并发令牌桶限流**：集成 `governor` 高性能无锁限流器，支持按 QPS 精准控制全局流量，防止恶意刷量导致账单失控。
- **零延迟流式 Token 审计**：利用 `async-stream` 构建透明旁路拦截器，在零阻塞透传 SSE 字节流的同时，自动注入 `stream_options` 并实时解析记录每次对话的 TTFB（首字节耗时）、总耗时及 Token 消耗。
- **强类型配置与统一错误处理**：基于环境变量驱动配置，配合 `thiserror` 提供规范的 HTTP 状态码与结构化错误响应。

## 🛠️ 技术栈

- **Web 框架 / 异步运行时**：`Axum` + `Tokio`
- **HTTP 客户端 / 流处理**：`Reqwest` (`stream`) + `async-stream` + `futures-util`
- **并发限流**：`Governor`
- **序列化 / 日志 / 配置**：`Serde` + `Tracing` + `dotenvy`

---

## ⚙️ 环境配置（`.env` 文件说明）

项目启动时会自动读取根目录下的 `.env` 文件（若使用系统环境变量亦可直接覆盖）。请在项目根目录新建 `.env` 文件并参考以下模板配置：

```env
# 【必填】上游大模型 API Key（如阿里云百炼 DashScope API Key）
DASHSCOPE_API_KEY=sk-your-real-dashscope-api-key

# 【可选】网关监听端口，默认 3000
GATEWAY_PORT=3000

# 【可选】上游推理接口地址，默认指向阿里云百炼 OpenAI 兼容端点
UPSTREAM_URL=[https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions](https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions)

# 【可选】客户端访问本网关的鉴权 Key，默认 sk-jerry-gateway-local-dev-key
GATEWAY_API_KEY=sk-jerry-gateway-local-dev-key

# 【可选】全局限流阈值（每秒允许的最大请求数），默认 2
RATE_LIMIT_PER_SEC=2

```

### ⚠️ `.env` 文件核心注意事项

1. **严禁提交至代码仓库**：`.env` 文件包含真实的上游计费 API Key，**务必确保 `.env` 已加入 `.gitignore**`，切勿推送到 GitHub/GitLab 等公开仓库。建议仅提交脱敏后的 `.env.example` 作为配置模板。
2. **必填项校验**：`DASHSCOPE_API_KEY` 为强校验项，未配置或拼写错误会导致程序启动时直接 panic 退出。
3. **生产环境务必修改默认 Key**：`GATEWAY_API_KEY` 在未配置时会回退到开发默认值 `sk-jerry-gateway-local-dev-key`，部署至测试或生产环境前请务必替换为高强度随机密钥。
4. **注意文件编码与格式**：在 Windows 下编辑 `.env` 时，请保存为 **UTF-8（无 BOM）** 格式，且等号 `=` 两侧不要留多余空格。
5. **配置变更需重启**：当前配置在网关启动时加载至内存，修改 `.env` 后需重启服务生效。

---

## 🚀 快速启动

```bash
# 1. 克隆项目并进入目录
git clone <your-repo-url>
cd jerry-ai-gateway

# 2. 创建并填写 .env 文件（参考上方说明）

# 3. 编译并运行
cargo run

```

服务启动后，可通过健康检查接口验证状态（免鉴权、免限流）：

```bash
curl [http://127.0.0.1:3000/health](http://127.0.0.1:3000/health)

```

---

## 📡 接口调用示例

核心推理接口：`POST /v1/chat/completions`（需携带网关鉴权头）

### 使用 `curl` 测试流式输出（推荐 Git Bash / Linux / macOS）

```bash
curl -N -X POST "[http://127.0.0.1:3000/v1/chat/completions](http://127.0.0.1:3000/v1/chat/completions)" \
  -H "Authorization: Bearer sk-jerry-gateway-local-dev-key" \
  -H "Content-Type: application/json; charset=utf-8" \
  -d '{
    "model": "qwen-plus",
    "stream": true,
    "messages": [
      {"role": "user", "content": "你好，介绍一下你自己。"}
    ]
  }'

```

---

## 💡 使用与调试注意事项

1. **请求头 `Content-Type` 切勿填错**
* 无论是否开启流式输出（`"stream": true`），客户端发给网关的请求体都是 JSON，因此请求头必须为 `Content-Type: application/json`（或 `application/json; charset=utf-8`）。
* `text/event-stream` 是**网关返回的响应头**，不要误填到请求头中，否则会触发 `415 Unsupported Media Type`。


2. **全局限流触发 `429 Too Many Requests**`
* 默认限流为每秒 2 次请求（全局共享令牌桶）。如果本地压测或快速连点调试触发 `429` 拦截属于正常现象，可在 `.env` 中调大 `RATE_LIMIT_PER_SEC`。


3. **流式 Token 审计参数自动注入**
* 当请求包含 `"stream": true` 时，网关会自动在转发给上游的报文里注入 `"stream_options": {"include_usage": true}`，以确保在流式输出结束时能够捕获 Token 消耗量并在服务端控制台打印审计日志。


4. **Windows 终端调试避坑指南**
* **PowerShell**：直接用 `curl.exe -d "{...}"` 传递 JSON 和中文极易触发引号转义错误或编码乱码。建议改用 ApiPost/Postman，或使用 Git Bash。
* **Git Bash**：若直接粘贴含中文的命令行参数报 `invalid unicode code point`，请将 JSON 写入 UTF-8 编码的 `request.json` 文件，并通过 `curl -N ... -d @request.json` 发送。
* **API 调试工具（ApiPost / Postman）**：部分版本默认会对响应做缓冲或单行渲染，若想直观查看逐字输出的打字机节奏，建议使用 `curl -N`（禁用缓冲）进行验证。