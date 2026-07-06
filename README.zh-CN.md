# llm-proxy

[English](./README.md) | [中文](./README.zh-CN.md)

`llm-proxy` 是一个面向编码 agent 的无状态 LLM API 网关。它对外暴露 Claude Code 和 Codex 使用的两套客户端 API，然后将每个请求路由到配置好的上游后端，同时保留工具调用、流式事件、用量统计和推理载荷。

当前项目暴露的接口：

| 客户端 | 前端端点 | 兼容的上游后端 |
|---|---|---|
| Claude Code | `POST /v1/messages` | OpenAI Chat / DeepSeek，或 OpenAI Responses |
| Codex | `POST /v1/responses` | OpenAI Chat / DeepSeek，或 Anthropic Messages |

协议映射的设计参见 [`DESIGN.md`](./DESIGN.md)，本地、集成及真实客户端测试流程参见 [`TESTING.md`](./TESTING.md)。

## 快速开始

环境要求：

- 支持 edition 2024 的 Rust 工具链
- 若要调用任何真实后端，需具备网络访问能力和上游凭据

运行默认的 DeepSeek 兼容配置：

```bash
export DEEPSEEK_API_KEY="<你的 DeepSeek API key>"
cargo run
```

服务器默认监听 `127.0.0.1:8080`。可通过 `LLM_PROXY_ADDR` 覆盖：

```bash
LLM_PROXY_ADDR=127.0.0.1:18080 cargo run
```

健康检查：

```bash
curl -s http://127.0.0.1:8080/health
```

## 配置

`llm-proxy` 在设置了 `LLM_PROXY_CONFIG` 时从该路径加载配置。配置文件可以是 TOML、YAML 或 YML 格式。在支持的情况下，环境变量会覆盖配置文件中的值。

`config.toml` 示例：

```toml
listen_addr = "127.0.0.1:8080"
anthropic_default_max_tokens = 4096

[switches]
anthropic_cache_injection = true
reasoning_store = false
observability_dump = false

[backend_request]
max_retries = 2
initial_backoff_ms = 250
max_backoff_ms = 5000
timeout_ms = 120000
concurrency_limit = 16

[[backends]]
name = "deepseek"
type = "chat"
base_url = "https://api.deepseek.com"
profile = "deep_seek"

[[backends]]
name = "responses"
type = "responses"
endpoint = "https://responses.example/v1/responses"
profile = "generic_openai"

[[backends]]
name = "anthropic"
type = "anthropic"
base_url = "https://anthropic.example"
profile = "anthropic"
default_model = "claude-opus-compatible"

[model_aliases."claude-code-deepseek"]
backend = "deepseek"
model = "deepseek-chat"

[model_aliases."codex-deepseek"]
backend = "deepseek"
model = "deepseek-chat"

[model_aliases."claude-code-rich"]
backend = "responses"
model = "gpt-5.5"

[model_aliases."codex-rich"]
backend = "anthropic"
model = "claude-opus-compatible"
```

### 后端故障切换（failover）

一个模型别名可以配置多个后端目标：下标 0 为首选，其余为故障切换候选。当当前活跃后端在一个滑动时间窗口内累计足够多的失败（HTTP 429、5xx，或连接/超时错误）时，别名会为**后续**请求切换到下一个目标。切换是单向的（不会回退到更靠前的首选目标），并受最小切换间隔限制——即便失败次数已达阈值，只要距上次切换的时间未到该间隔，也不会切换。触发阈值的那一次请求仍会照常返回错误，只有之后的请求才会使用新目标。

```toml
[model_aliases."codex-default"]
targets = [
  { backend = "responses", model = "gpt-5.1" },
  { backend = "anthropic", model = "claude-sonnet-4-5" },
]

[model_aliases."codex-default".failover]
window_ms = 60000              # 统计失败的滑动窗口
failure_threshold = 3          # 窗口内达到该失败次数即满足切换条件
min_switch_interval_ms = 30000 # 两次切换之间的最小间隔
```

`failover` 块可选；省略时默认为 `window_ms = 60000`、`failure_threshold = 3`、`min_switch_interval_ms = 30000`。旧的单目标写法（`backend = "..."` + `model = "..."`）仍然支持，等价于只有一个 `targets` 条目且不带 failover。

不要提交真实凭据。请将密钥保存在本地环境变量或未纳入版本控制的本地配置中。对应上面的示例：

```bash
export LLM_PROXY_CONFIG="$PWD/config.toml"
export DEEPSEEK_API_KEY="<deepseek key>"
export OPENAI_API_KEY="<responses backend key>"
export ANTHROPIC_AUTH_TOKEN="<anthropic backend key>"
cargo run
```

当变量名存在重叠时，请在与代理进程不同的 shell 中运行客户端命令。`ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` 对代理进程而言可能表示「Anthropic 后端凭据」，但对 Claude Code 进程而言表示「Claude Code 应调用的 base URL」；同理，`OPENAI_API_KEY` 对代理而言可能是 Responses 后端 key，对 Codex 而言则可能只是本地占位符。

### 代理鉴权

设置顶层 `api_key`（或 `LLM_PROXY_API_KEY` 环境变量）即可要求下游客户端进行鉴权。配置后，访问 `/v1/messages`、`/v1/responses`、`/passthrough` 的请求必须通过 `Authorization: Bearer <key>` 或 `x-api-key: <key>` 之一携带该 key，否则代理返回 `401`；`/health` 保持开放。未设置 `api_key` 时代理接受匿名请求（默认，向后兼容）。

```toml
api_key = "your-proxy-secret"   # 或设置 LLM_PROXY_API_KEY
```

代理**从不**将下游客户端的凭据转发给后端：每个后端只使用它自己配置的 `api_key`（或相应的 `*_API_KEY` 环境变量）。未配置 `api_key` 的 `chat` 后端在发请求时**不带** `Authorization` header——这既避免把客户端/代理 key 泄露给后端，也支持无需 key 的后端（例如缺省配置的 Ollama）。（`responses` 与 `anthropic` 后端仍要求各自配置 `api_key`。）

### 后端附加 Header 与 Query 参数

每个后端都可配置可选的 `additional_headers`（别名 `headers`）与 `additional_query_params`（别名 `query` / `query_params`）映射。它们会被附加到代理发往该后端的每个请求上——适用于要求自定义 header 的网关，或要求 `api-version` query 参数的 Azure AI Foundry / Azure OpenAI。

```toml
[[backends]]
name = "azure"
type = "responses"
endpoint = "https://your-resource.openai.azure.com/openai/responses"
api_key = "<azure key>"
profile = "generic_openai"
additional_query_params = { "api-version" = "2024-02-01" }
additional_headers = { "x-custom-gateway" = "value" }
```

### 重要环境变量

| 变量 | 用途 |
|---|---|
| `LLM_PROXY_CONFIG` | TOML/YAML 配置文件路径 |
| `LLM_PROXY_ADDR` | 监听地址，默认 `127.0.0.1:8080` |
| `LLM_PROXY_API_KEY` | 可选的代理级 API key，要求下游客户端携带 |
| `DEEPSEEK_API_KEY` | Chat/DeepSeek 后端 API key |
| `LLM_PROXY_CHAT_COMPLETIONS_URL` | 覆盖 Chat Completions 端点 |
| `OPENAI_API_ENDPOINT` / `OPENAI_API_KEY` | Responses 兼容后端的端点和 key |
| `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` | Anthropic 兼容后端的 base URL 和凭据 |
| `ANTHROPIC_VERSION` | Anthropic 后端版本头，默认 `2023-06-01` |
| `ANTHROPIC_DEFAULT_OPUS_MODEL` | Anthropic 后端路由的默认上游模型 |
| `LLM_PROXY_OBSERVABILITY_DUMP` | 用于调试的脱敏 JSON dump 日志 |
| `LLM_PROXY_BACKEND_MAX_RETRIES` | 可重试的上游失败的重试次数 |
| `LLM_PROXY_BACKEND_TIMEOUT_MS` | 每次尝试的上游超时时间 |
| `LLM_PROXY_BACKEND_CONCURRENCY_LIMIT` | 全局上游并发上限 |

`LLM_PROXY_BACKENDS` 和 `LLM_PROXY_MODEL_ALIASES` 可用 JSON 值替换已配置的后端列表和模型别名映射。

## 将客户端指向网关

### Claude Code

Claude Code 使用 Anthropic Messages 协议。将其指向代理的 base URL；若代理进程自身已配置上游凭据，则可使用任意本地占位符 token：

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:8080"
export ANTHROPIC_AUTH_TOKEN="local-placeholder"
claude -p "Use one sentence to say hello through llm-proxy."
```

请使用代理能够路由的模型名，例如 `deepseek-chat` 或配置好的别名（如 `claude-code-rich`）。

### Codex

Codex 使用 OpenAI Responses 协议。将其 Responses base URL 配置为代理地址，在代理进程已配置上游凭据时将 API key 保持为本地占位符，并在测试时使用隔离的 `CODEX_HOME`：

```bash
export CODEX_HOME="$(mktemp -d)"
export OPENAI_API_KEY="local-placeholder"
# 将你所用 Codex 版本的 provider/base-url 设置配置为：
#   http://127.0.0.1:8080/v1
codex exec "Use one sentence to say hello through llm-proxy."
```

Codex CLI 的确切 provider 语法请查阅你所安装 Codex 版本的帮助输出；要指向的代理端点是 `/v1` base URL 下的 `/v1/responses`。

## 支持的后端与 profile

| 后端类型 | Profile | 说明 |
|---|---|---|
| `chat` | `deep_seek`、`generic_openai` | OpenAI Chat 兼容后端。DeepSeek 会应用其文档中的参数黑名单、`reasoning_effort` 归一化以及 `n > 1` 拒绝规则。 |
| `responses` | `generic_openai` | OpenAI Responses 兼容后端。请求强制 `store=false`，并在调用后端时包含 `reasoning.encrypted_content`。 |
| `anthropic` | `anthropic` | Anthropic Messages 兼容后端。支持可选的无状态 cache-control 注入以及 Anthropic thinking 签名。 |

模型路由优先使用精确匹配的 `model_aliases`。在没有别名的情况下，`deepseek-*` 模型路由到 Chat 后端；非 DeepSeek 模型在配置了相应后端时，会路由到与前端端点兼容的 rich 后端。

## 部署说明

构建 release 二进制：

```bash
cargo build --release
```

使用本地配置和环境变量中保存的密钥运行：

```bash
export LLM_PROXY_CONFIG=/etc/llm-proxy/config.toml
export DEEPSEEK_API_KEY="<deepseek key>"
export OPENAI_API_KEY="<responses key>"
export ANTHROPIC_AUTH_TOKEN="<anthropic key>"
RUST_LOG=llm_proxy=info ./target/release/llm-proxy
```

如果启用 `LLM_PROXY_OBSERVABILITY_DUMP`，请求和响应 JSON 会在记录时对凭据、`encrypted_content` 和签名进行脱敏。在评估隐私影响之前，请勿在共享的生产日志中启用 body dump。

## 测试

不需要真实网络凭据的本地校验：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --all-targets
```

真实的 Claude Code / Codex 端到端测试有意标记为 `#[ignore]`，因为它们需要已安装的 CLI、网络访问以及未纳入版本控制的凭据。确切的真实环境测试流程参见 [`TESTING.md`](./TESTING.md)。

## 已知限制

- 该网关有意保持无状态。需要服务端会话存储的请求（例如不受支持的 `previous_response_id` 状态）会被拒绝，而不会被模拟。
- 推理载荷通过 envelope 保留。可选的推理存储回退机制用于处理超大 envelope，但默认关闭。
- OpenAI Chat / DeepSeek 后端相比 Anthropic 或 Responses 是保真度更低的目标；不受支持的特性会依据 profile 规则被丢弃，或以协议错误被拒绝。
- 真实 CLI 的配置项可能随 Claude Code 和 Codex 版本变化。测试时请使用临时配置目录，将各客户端专属的设置隔离开来。
