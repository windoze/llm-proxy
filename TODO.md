# LLM Proxy 任务列表

> 本文件是 [PLAN.md](./PLAN.md) 的可执行任务分解，供 coding agent 逐条执行。
>
> **约定**
> - 任务按执行顺序排列，编号形如 `M1-01`。
> - 标题中的 `[TODO]` 是状态标记，执行完成后由 agent 更新为 `[DONE]`（或 `[BLOCKED]` 并注明原因）。
> - 每个里程碑最后有一个 `-RV` review 任务，确认该里程碑实现正确且未偏离 [DESIGN.md](./DESIGN.md) 目标。
> - 参考文档：`DESIGN.md`（设计与约束）、`PLAN.md`（里程碑）、`TESTING.md`（测试与真实世界联调）。文中 `DESIGN §x` 指 DESIGN.md 章节。
> - **真实世界联调**：`.envrc`（已 gitignore）预置了 DeepSeek / Responses / Anthropic 三组真实后端凭据，供 `-RV` 里程碑做真实客户端联调。接法与安全铁律见 `TESTING.md`。凭据禁止写进任何入库文件。
>
> **全局铁律**
> - **无状态**：任何任务不得引入会话状态存储（唯一例外见 M4-05，默认关闭）。
> - **保真优先**：reasoning 往返、tool-call 流式重组、ID 映射三块必须有测试锁死。
> - **profile 可扩展**：新增 OpenAI 兼容后端应只需加 profile，不改核心逻辑。

---

## M0 — 项目骨架

### [DONE] M0-01 添加项目依赖
在 `Cargo.toml` 的 `[dependencies]` 加入并锁定合适版本：
- `tokio`（features: `["full"]`）、`axum`、`tower`、`tower-http`（features: `["trace"]`）
- `reqwest`（features: `["json", "stream"]`，默认 rustls）
- `serde`（features: `["derive"]`）、`serde_json`
- `eventsource-stream`(SSE 解析)、`bytes`、`futures`/`futures-util`、`async-stream`
- `tracing`、`tracing-subscriber`（features: `["env-filter"]`）
- `thiserror`、`anyhow`
- dev-dependencies: `tokio-test`、`insta`(快照测试)、`wiremock`(mock 后端)
运行 `cargo build` 确认可编译。注意 edition 已是 2024。

完成记录：
- 2026-07-06：已添加并锁定所需 dependencies/dev-dependencies，`Cargo.lock` 已更新。
- `reqwest` 使用当前版本的 `rustls` feature（关闭 default-features）以保持 rustls-only TLS 行为。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo build`、`cargo test --all --all-targets` 均通过。

### [DONE] M0-02 建立目录结构与模块骨架
按 PLAN.md 的结构创建空模块（每个 `mod.rs` 先放 `//!` 文档注释 + 占位）：
```
src/main.rs
src/config.rs
src/error.rs
src/ir/{mod.rs,message.rs,request.rs,event.rs}
src/protocol/{mod.rs,anthropic/mod.rs,responses/mod.rs,openai_chat/mod.rs}
src/provider/{mod.rs}
src/stream/{mod.rs}
```
在 `main.rs` 用 `mod` 声明所有顶层模块。确保 `cargo build` 通过（允许 dead_code warning）。

完成记录：
- 2026-07-06：已按 PLAN.md/M0-02 创建 `src/config.rs`、`src/error.rs`、`src/ir/*`、`src/protocol/*`、`src/provider/mod.rs`、`src/stream/mod.rs` 模块骨架，并在 `src/main.rs` 声明顶层模块。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo build --quiet`、`cargo test --all --all-targets --quiet` 均通过。

### [DONE] M0-03 定义统一错误类型
在 `src/error.rs` 定义 `ProxyError`（`thiserror`），至少涵盖：
`UpstreamHttp`(reqwest error)、`Deserialize`(serde)、`UnsupportedFeature{feature,protocol}`、
`ProtocolMapping(String)`、`Config(String)`、`Upstream4xx{status,body}`。
实现 `axum::response::IntoResponse`，把错误映射为合理的 HTTP 状态码 + JSON body（结构 M7 再细化，此处给最简版）。
定义 `type Result<T> = std::result::Result<T, ProxyError>;`。

完成记录：
- 2026-07-06：已在 `src/error.rs` 定义统一 `ProxyError` 与 `Result<T>` 别名，覆盖上游 HTTP、JSON 解析、不支持特性、协议映射、配置错误与上游 4xx 响应。
- 已实现 `axum::response::IntoResponse`，返回合理 HTTP 状态码与最简 JSON error body；新增单元测试覆盖配置错误响应体与上游 4xx 状态透传。
- `src/main.rs` 将 `error` 模块公开，以便当前骨架阶段的共享错误 API 不触发 dead-code warning。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo build --quiet`、`cargo test --all --all-targets --quiet` 均通过。

### [DONE] M0-04 启动 axum 服务与 /health
在 `main.rs`：初始化 `tracing_subscriber`（读 `RUST_LOG`），构建 `axum::Router`，
监听地址从环境变量 `LLM_PROXY_ADDR`（默认 `127.0.0.1:8080`）读取。
加 `GET /health` 返回 `200 {"status":"ok"}`。用 `tower_http::trace::TraceLayer` 记录请求。

完成记录：
- 2026-07-06：已将 `src/main.rs` 改为 Tokio/Axum 服务入口，按 `RUST_LOG` 初始化 tracing，按 `LLM_PROXY_ADDR`（默认 `127.0.0.1:8080`）绑定监听地址。
- 已构建带 `TraceLayer::new_for_http()` 的 Router，并添加 `GET /health`，返回 `200 {"status":"ok"}`；新增单元测试覆盖该路由响应。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo build --quiet`、`cargo test --all --all-targets --quiet` 均通过。

### [DONE] M0-05 实现流式透传路由（passthrough）
加一条临时路由（如 `POST /passthrough`），用 `reqwest` 向配置的上游 URL 转发请求体，
用 `reqwest::Response::bytes_stream()` 把响应体作为 `axum::body::Body` 流式返回，
透传 `content-type`。目的：验证 axum + reqwest 流式链路字节无损。上游 URL 暂从环境变量读。

完成记录：
- 2026-07-06：已添加 `POST /passthrough` 临时路由，从 `LLM_PROXY_UPSTREAM_URL` 读取上游 URL，使用共享 `reqwest::Client` 将请求体转发到上游。
- 已将上游响应的 status 与 `content-type` 透传给客户端，并用 `bytes_stream()` + `Body::from_stream()` 流式返回响应体；同时透传客户端请求的 `content-type` 到上游。
- 新增单元测试覆盖请求体转发、响应字节无损、`content-type` 透传，以及缺少上游 URL 时的配置错误响应。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo build --quiet`、`cargo test --all --all-targets --quiet` 均通过。

### [DONE] M0-RV 【Review】M0 骨架
确认：`cargo build` + `cargo clippy` 无 error；`/health` 可访问；passthrough 能流式转发一个真实
SSE 响应且字节无损（可用 `curl` 对比）。确认目录结构与 PLAN.md 一致。记录偏差。

完成记录：
- 2026-07-06：已复核 M0 依赖、目录骨架、统一错误类型、Axum 服务入口、`/health` 与临时 `POST /passthrough` 路由；当前 `src/` 结构与 PLAN.md 的 M0 骨架要求一致。
- 已用本地 chunked SSE 上游 + `curl` 验证运行中的 `/health` 与 passthrough：`/health` 返回 `{"status":"ok"}`，passthrough 保留 `text/event-stream` 且 SSE 响应字节与上游输出一致。
- 偏差记录：M0 骨架未发现与 PLAN.md/TODO.md 要求不一致的偏差。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo build --quiet`、`cargo test --all --all-targets --quiet`、live `curl` `/health` 与 SSE passthrough 字节对比均通过。

---

## M1 — IR 数据结构 + OpenAI Chat 解析

### [DONE] M1-01 定义 IR 内容块 (`ir/message.rs`)
定义核心类型（`serde` 可序列化，字段用 `Option` 表达可选）：
- `enum Role { System, User, Assistant, Tool }`
- `enum ContentBlock { Text{text}, Image(ImageSource), ToolUse{id,name,input:serde_json::Value},
  ToolResult{tool_use_id,content:Vec<ContentBlock>,is_error:bool}, Thinking(Thinking) }`
- `struct Thinking { text:Option<String>, opaque:Option<Vec<u8>>, source:Provider, echo_policy:EchoPolicy }`
- `enum EchoPolicy { Always, OnlyWithToolCall, Never }`
- `enum Provider { Anthropic, Responses, OpenAiChat, DeepSeek }`
- `enum ImageSource { Url(String), Base64{media_type:String,data:String} }`
- `struct Message { role:Role, content:Vec<ContentBlock> }`
细节见 DESIGN §2.1、§4.2。为所有类型 derive `Debug, Clone, PartialEq`。

完成记录：
- 2026-07-06：已在 `src/ir/message.rs` 定义 `Role`、`ContentBlock`、`Thinking`、`EchoPolicy`、`Provider`、`ImageSource` 与 `Message`，覆盖 DESIGN §2.1/§4.2 要求的 text/image/tool_use/tool_result/thinking 内容块模型。
- 所有类型已 derive `Debug, Clone, PartialEq` 并支持 serde 序列化/反序列化；因后续 M1 任务才会接入解析/编码，已对该 staged IR 模块添加局部 dead-code allowance，避免当前阶段 lint 噪音。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M1-02 定义 IR 统一请求 (`ir/request.rs`)
- `struct IrRequest { model:String, system:Option<Vec<ContentBlock>>, messages:Vec<Message>,
  tools:Vec<ToolDef>, tool_choice:ToolChoice, max_tokens:Option<u32>, temperature:Option<f32>,
  top_p:Option<f32>, top_k:Option<u32>, stop:Vec<String>, stream:bool,
  extra:serde_json::Map<String,Value> }`（`extra` 装 provider 特有直通参数）
- `struct ToolDef { name:String, description:Option<String>, input_schema:serde_json::Value }`
- `enum ToolChoice { Auto, None, Required, Tool(String) }`
- `struct IrResponse { id:String, model:String, content:Vec<ContentBlock>, stop_reason:StopReason, usage:Usage }`
- `enum StopReason { EndTurn, MaxTokens, StopSequence, ToolUse, Other(String) }`
- `struct Usage { input_tokens:u32, output_tokens:u32, cache_read:Option<u32>, cache_write:Option<u32> }`
映射依据见 DESIGN §6.5/§6.6。

完成记录：
- 2026-07-06：已在 `src/ir/request.rs` 定义 `IrRequest`、`ToolDef`、`ToolChoice`、`IrResponse`、`StopReason` 与 `Usage`，覆盖统一请求、非流式响应、停止原因与 token/cache usage 结构。
- 所有新增 IR 类型已 derive `Debug, Clone, PartialEq, Serialize, Deserialize`，并为 staged IR 模块添加局部 dead-code allowance，等待后续 M1 解析/编码任务接入。
- 新增单元测试覆盖 request/response serde 形状、provider `extra` 直通参数、工具选择、停止原因与 usage 字段。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M1-03 定义流式 IR event (`ir/event.rs`)
`enum IrEvent`，至少含：
`MessageStart{id,model}`、`BlockStart{index:usize, block:BlockKind}`、
`TextDelta{index,text}`、`ThinkingDelta{index,text}`、`ToolUseDelta{index,partial_json:String}`、
`BlockStop{index}`、`MessageDelta{stop_reason:Option<StopReason>, usage:Option<Usage>}`、`MessageStop`。
`enum BlockKind { Text, Thinking, ToolUse{id,name} }`。
这是流式转换的中间语言，encoder/decoder 只与它交互（DESIGN §6.1）。

完成记录：
- 2026-07-06：已在 `src/ir/event.rs` 定义 provider-neutral `IrEvent` 与 `BlockKind`，覆盖 message/block lifecycle、text/thinking/tool-use delta、stop reason 与 usage 更新事件。
- 所有新增事件类型已 derive `Debug, Clone, PartialEq, Serialize, Deserialize`，并沿用 staged IR 模块的局部 dead-code allowance，等待后续流式 decoder/encoder 任务接入。
- 新增单元测试覆盖 tool-use block start 与 message delta 的 serde 形状。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M1-04 定义 provider capability profile trait (`provider/mod.rs`)
定义 `trait CapabilityProfile`，方法覆盖 DESIGN §5 的能力：
- `fn param_blocklist(&self, model:&str) -> &[&str]`（静默 drop 的参数名）
- `fn normalize_reasoning_effort(&self, effort:&str) -> &str`
- `fn reasoning_echo_policy(&self, model:&str) -> EchoPolicy`
- `fn supports_multiple_choices(&self) -> bool`（`n>1`）
- `fn base_url(&self) -> &str` / `fn map_model_name(&self, requested:&str) -> String`
- `fn thinking_model(&self, model:&str) -> bool`
提供一个 `GenericOpenAi` 默认实现（无 blocklist、echo=Never）。

完成记录：
- 2026-07-06：已在 `src/provider/mod.rs` 定义 `CapabilityProfile`，覆盖参数 blocklist、reasoning effort 归一、reasoning echo policy、多 choice 支持、base URL、模型名映射与 thinking-model 检测能力。
- 已实现 `GenericOpenAi` 默认 profile：无 blocklist、reasoning effort 原样保留、`EchoPolicy::Never`、支持 `n>1`、默认 OpenAI Chat base URL、模型名原样映射、默认无 thinking model；同时支持自定义 base URL。
- 新增单元测试覆盖 `GenericOpenAi` 中性默认行为与自定义 base URL。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M1-05 实现 DeepSeek profile (`provider/deepseek.rs`)
实现 `CapabilityProfile`，严格按 DESIGN §5：
- `param_blocklist`: `["temperature","top_p","presence_penalty","frequency_penalty","logprobs","top_logprobs"]`
- `normalize_reasoning_effort`: `low|medium -> high`, `xhigh -> max`, 默认 `high`
- `reasoning_echo_policy`: `OnlyWithToolCall`
- `supports_multiple_choices`: `false`
- `thinking_model`: `model == "deepseek-reasoner"`（`deepseek-chat` 为 false）
- `base_url`: `https://api.deepseek.com`
在代码注释标注 DESIGN §5 的"官方文档版本不一致"警告，说明以 `thinking_mode` 新页为准。

完成记录：
- 2026-07-06：已新增 `src/provider/deepseek.rs` 并在 `src/provider/mod.rs` 暴露 `provider::deepseek` 模块。
- 已实现 DeepSeek `CapabilityProfile`：按 DESIGN §5 静默 drop 不支持参数、归一 `reasoning_effort`、使用 `EchoPolicy::OnlyWithToolCall`、禁用 `n>1`、仅 `deepseek-reasoner` 启用 thinking、base URL 为 `https://api.deepseek.com`，模型名保持原样。
- 已在代码注释中标注 DeepSeek 官方文档版本不一致，并说明以 `thinking_mode` 新页规则为准；新增单元测试覆盖每条 profile 规则。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M1-06 OpenAI Chat/DeepSeek 请求解析 (`protocol/openai_chat/decode.rs`)
实现 `chat_request_to_ir(body:&Value, profile:&dyn CapabilityProfile) -> Result<IrRequest>`：
- messages 中 `role:system` → `IrRequest.system`
- `tool_calls`（assistant）→ `ContentBlock::ToolUse`；`role:tool` 消息 → `ToolResult`（配 `tool_call_id`）
- `reasoning_content` → `ContentBlock::Thinking{source:DeepSeek, echo_policy: profile 决定}`
- `tools`/`tool_choice`/`max_tokens`/`temperature` 等映射到 IR
覆盖 DESIGN §4.1(DeepSeek 条件回传)、§6.3(工具挂载)。

完成记录：
- 2026-07-06：已新增 `src/protocol/openai_chat/decode.rs` 并在 `protocol/openai_chat` 暴露 decoder。
- 已实现 `chat_request_to_ir`：支持 system/developer hoist、user/assistant/tool 消息解析、assistant `tool_calls` → `ToolUse`、`role:tool` → `ToolResult`、DeepSeek `reasoning_content` → `Thinking{source:DeepSeek, echo_policy: profile.reasoning_echo_policy(...)}`。
- 已解析 `tools`、`tool_choice`、`max_tokens`/`max_completion_tokens`、`temperature`、`top_p`、`top_k`、`stop`、`stream` 与 provider `extra`；DeepSeek profile blocklist 参数会静默 drop，`reasoning_effort` 会按 profile 归一化，`n>1` 在不支持多 choice 的 profile 下返回不支持特性错误。
- 新增单元测试覆盖 DeepSeek reasoning + tool_calls + tool result 映射、OpenAI 常规采样参数保留、DeepSeek `n>1` 拒绝、非法 tool arguments JSON 报错。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M1-07 OpenAI Chat/DeepSeek 响应解析 (`protocol/openai_chat/decode.rs`)
实现 `chat_response_to_ir(body:&Value) -> Result<IrResponse>`（非流式）：
`choices[0].message.content` → Text block；`reasoning_content` → Thinking；`tool_calls` → ToolUse；
`finish_reason` → `StopReason`（`stop→EndTurn, length→MaxTokens, tool_calls→ToolUse`）；
`usage` → `Usage`（含 `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens` → cache_read/miss）。

完成记录：
- 2026-07-06：已实现 `chat_response_to_ir`，解析非流式 OpenAI Chat/DeepSeek 响应的首个 choice，输出 `IrResponse`。
- 已覆盖 assistant `content` → Text、DeepSeek `reasoning_content` → `Thinking{source:DeepSeek, echo_policy:OnlyWithToolCall}`、`tool_calls` → `ToolUse`，以及 `finish_reason` 到 `StopReason` 的 `stop`/`length`/`tool_calls` 映射。
- 已解析 `usage.prompt_tokens`/`completion_tokens` 到 `Usage.input_tokens`/`output_tokens`，并将 DeepSeek `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens` 映射到 `cache_read`/`cache_write`。
- 新增单元测试覆盖 DeepSeek reasoning + tool_calls + cache usage 响应，以及普通 text 响应的 `stop`/`length` 停止原因和无 cache usage 场景。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M1-08 M1 单元测试
在 `protocol/openai_chat/` 加 `#[cfg(test)]`：准备 DeepSeek 响应 JSON 样本
（含 `reasoning_content` + `tool_calls` 组合），断言解析出的 IR 结构正确，
echo_policy 在有/无 tool_calls 场景符合 §4.1。用 `insta` 做快照。

完成记录：
- 2026-07-06：已在 `src/protocol/openai_chat/decode.rs` 增加 DeepSeek 非流式响应测试样本，覆盖 `reasoning_content` + `tool_calls` 组合以及无 tool_calls 场景。
- 已断言解析出的 `IrResponse` 结构、`ToolUse`、cache usage 与 `EchoPolicy::OnlyWithToolCall`，锁定 DESIGN §4.1 中 DeepSeek 有工具调用必须回传、无工具调用可丢弃的条件性 reasoning 语义。
- 已用 `insta::assert_snapshot!` 生成并提交两个 JSON 快照：带 tool_calls 和不带 tool_calls 的 DeepSeek 响应 IR。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`INSTA_UPDATE=always cargo test --all --all-targets`、`cargo test --all --all-targets` 均通过。

### [DONE] M1-RV 【Review】M1 IR 与解析
确认：IR 类型覆盖 DESIGN §2.1/§4.2 全部块类型；DeepSeek profile 规则与 §5 逐条一致；
解析测试通过；`reasoning_content` echo_policy 逻辑正确。检查是否有硬编码应进 profile 的东西。记录偏差。

完成记录：
- 2026-07-06：已复核 M1 IR：`ContentBlock` 覆盖 `text` / `image` / `tool_use` / `tool_result` / `thinking`，`Thinking` 包含 `text`、`opaque`、`source`、`echo_policy`，与 DESIGN §2.1/§4.2 一致。
- 已复核 DeepSeek `CapabilityProfile`：参数 blocklist、`reasoning_effort` 归一、`OnlyWithToolCall` echo policy、禁用 `n>1`、`deepseek-reasoner` thinking 判定与 base URL 均与 DESIGN §5 一致。
- 已复核 OpenAI Chat/DeepSeek decoder 测试与快照：请求/响应解析覆盖 `reasoning_content`、tool_calls/tool_result、cache usage、DeepSeek 参数 drop 与 `n>1` 拒绝；DeepSeek `reasoning_content` 使用 `EchoPolicy::OnlyWithToolCall`，符合 DESIGN §4.1 的有 tool_calls 必须回传、无 tool_calls 可丢弃语义。
- 偏差记录：未发现 M1 当前阶段与 DESIGN/TODO 要求不一致的偏差；未发现需要从 M1 decoder 迁移到 profile 的额外硬编码。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

---

## M2 — 链 3：Chat/DeepSeek → Anthropic（服务 Claude Code）✅ 可用里程碑①

### [DONE] M2-01 Anthropic 请求解析 (`protocol/anthropic/decode.rs`)
实现 `anthropic_request_to_ir(body:&Value) -> Result<IrRequest>`（解析 Claude Code 发来的请求）：
- 顶层 `system`（string 或 block 数组）→ `IrRequest.system`
- `messages[].content` 的 block（`text`/`image`/`tool_use`/`tool_result`/`thinking`）→ IR ContentBlock
- `tools`（`input_schema`）、`tool_choice`（`auto/any/tool` → IR `Auto/Required/Tool`）、`max_tokens`
- `thinking` block（带 signature）→ `Thinking{source:Anthropic, opaque:signature, echo_policy:Always}`

完成记录：
- 2026-07-06：已新增 `src/protocol/anthropic/decode.rs` 并在 `protocol::anthropic` 暴露 decoder。
- 已实现 `anthropic_request_to_ir`：解析顶层 `system` 字符串/内容块数组、`messages` 中 `text`/`image`/`tool_use`/`tool_result`/`thinking` 内容块、`tools.input_schema`、`tool_choice` 的 `auto`/`none`/`any`/`tool`、`max_tokens`、采样参数、`stop_sequences`、`stream` 与 provider `extra`。
- Anthropic `thinking` block 会保存可读 thinking 文本，并把原始 `signature` 字符串字节放入 `Thinking.opaque`，设置 `source=Anthropic`、`echo_policy=Always`。
- 新增单元测试覆盖块数组系统提示、文本系统提示、工具定义/选择、tool_use/tool_result、Anthropic thinking signature 保真，以及未知内容块拒绝。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-02 Anthropic 非流式响应编码 (`protocol/anthropic/encode.rs`)
实现 `ir_response_to_anthropic(resp:&IrResponse) -> Value`：
IR content → Anthropic `content` block 数组；`ToolUse` → `tool_use` block；
`Thinking` → `thinking` block（signature 从 opaque）；`StopReason` → `end_turn/max_tokens/stop_sequence/tool_use`；
`Usage` → `{input_tokens,output_tokens,cache_read_input_tokens?}`。输出符合 Anthropic Messages API 响应结构。

完成记录：
- 2026-07-06：已新增 `src/protocol/anthropic/encode.rs` 并在 `protocol::anthropic` 暴露 encoder。
- 已实现 `ir_response_to_anthropic`，输出 Anthropic Messages API 非流式响应结构（`type=message`、`role=assistant`、`content`、`stop_reason`、`stop_sequence`、`usage`）。
- 已覆盖 IR `Text`/`Image`/`ToolUse`/`ToolResult`/`Thinking` 内容块到 Anthropic content block 的编码；`Thinking.opaque` 会按 Anthropic signature 原样恢复为 UTF-8 字符串。
- 已将 `StopReason` 映射为 `end_turn`/`max_tokens`/`stop_sequence`/`tool_use`，并保留 `Other` 自定义停止原因；`Usage.cache_read` 映射到 `cache_read_input_tokens`，`Usage.cache_write` 映射到 Anthropic `cache_creation_input_tokens`。
- 新增单元测试覆盖 thinking+tool_use 响应、所有 stop reason 映射、usage cache 字段，以及 image/tool_result 嵌套内容块编码。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-03 通用 SSE 解析基础设施 (`stream/sse.rs`)
封装基于 `eventsource-stream` 的辅助：把 `reqwest` bytes_stream 解析为 `(event_type, data)` 迭代，
处理 OpenAI Chat 的 `data: {...}` + `data: [DONE]` 终止。为下游状态机提供干净输入。

完成记录：
- 2026-07-06：已新增 `src/stream/sse.rs` 并从 `src/stream/mod.rs` 暴露，提供 `SseEvent { event_type, data }` 与 boxed `SseEventStream`，把 `reqwest` bytes stream 通过 `eventsource-stream` 解析为下游状态机可消费的干净 SSE 输入。
- 已实现 OpenAI Chat 专用解析入口，正常产出 `data: {...}` 事件，并将 `data: [DONE]` 视为流结束而非错误；上游传输错误映射为 `ProxyError::UpstreamHttp`，SSE/UTF-8 解析错误映射为 `ProxyError::ProtocolMapping`。
- 新增单元测试覆盖具名事件与默认 `message` 事件解析、多行 `data:` 合并、OpenAI `[DONE]` 终止、非法 UTF-8 解析错误。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-04 Chat SSE → IR event 状态机 (`stream/chat_decoder.rs`) 🔒
实现把 OpenAI Chat 流式 chunk 转成 `IrEvent` 流的**有状态**解析器：
- 首个 chunk → `MessageStart`
- `delta.content` → 维护 text block（首次发 `BlockStart{Text}`）+ `TextDelta`
- `delta.reasoning_content` → thinking block + `ThinkingDelta`
- `delta.tool_calls[i]` → 按 tool index 维护 ToolUse block，`function.arguments` 碎片 → `ToolUseDelta{partial_json}`
- `finish_reason` → 关闭所有开启的 block（`BlockStop`）+ `MessageDelta{stop_reason}` + `MessageStop`
**tool-call 流式重组是重点**（DESIGN §6.2），需处理碎片无边界问题。加单元测试覆盖多 tool、reasoning+content 混合。

完成记录：
- 2026-07-06：已新增 `src/stream/chat_decoder.rs` 并从 `src/stream/mod.rs` 暴露，提供 `ChatStreamDecoder` 与 `chat_sse_to_ir_events`，把 OpenAI Chat/DeepSeek SSE chunk 转为 provider-neutral `IrEvent` 流。
- 已实现有状态 block lifecycle：首个 chunk 发 `MessageStart`，`reasoning_content`/`content` 首次出现时分别创建 thinking/text block 并发 delta；`finish_reason` 会关闭所有开启 block、发 `MessageDelta`，并在流结束时发 `MessageStop`，同时支持 usage-only 尾 chunk 保留 token usage。
- 已按 tool index 维护多 tool call block，保留 `function.arguments` 碎片原始边界；当 arguments 早于 `id`/`function.name` 到达时会先缓冲，待 metadata 完整后再创建 ToolUse block 并按原顺序回放，避免无边界碎片丢失或错位。
- 新增单元测试覆盖 reasoning+content 混合、多 tool 碎片交错、metadata 晚到的 tool arguments 缓冲、usage 尾 chunk 以及缺少 `finish_reason` 的协议错误。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-05 IR event → Anthropic SSE 编码 (`protocol/anthropic/stream.rs`) 🔒
实现 `IrEvent` 流 → Anthropic SSE 事件流：
`MessageStart→message_start`；`BlockStart→content_block_start`（按 index 与 type）；
`TextDelta→content_block_delta{text_delta}`；`ThinkingDelta→content_block_delta{thinking_delta}`；
`ToolUseDelta→content_block_delta{input_json_delta{partial_json}}`；`BlockStop→content_block_stop`；
`MessageDelta→message_delta{stop_reason,usage}`；`MessageStop→message_stop`。
维护正确的 block index 序列（DESIGN §6.1）。

完成记录：
- 2026-07-06：已新增 `src/protocol/anthropic/stream.rs` 并从 `protocol::anthropic` 暴露，提供 `AnthropicStreamEncoder` 与 `ir_events_to_anthropic_sse`，将 provider-neutral `IrEvent` 编码为 Anthropic Messages API SSE bytes。
- 已覆盖 `message_start`、`content_block_start`、`content_block_delta`（text/thinking/input_json）、`content_block_stop`、`message_delta` 与 `message_stop` 的事件名和 JSON payload；thinking block start 会输出空 `signature` 字段，tool-use start 会输出空 `input` 对象。
- 编码器会校验 message lifecycle、block index 递增、delta/stop 只能作用于已开启 block、终止后不可继续输出，避免 Anthropic SSE index 序列错位。
- 新增单元测试覆盖 thinking/text/tool-use lifecycle、usage/cache 字段映射、SSE frame 格式、非连续 block index 拒绝与未开启 block delta 拒绝。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-06 tool ID 映射与配对 (`protocol/mod.rs` 或 `ir/`) 🔒
实现 `tool_call_id`(Chat) ↔ `tool_use_id`(Anthropic) 的映射与"调用→结果"配对链保真
（DESIGN §6.2）。在请求方向：Anthropic 的 `tool_result.tool_use_id` 要对回 Chat 的 `tool_call_id`。
确保多轮对话中 ID 不错位。加测试。

完成记录：
- 2026-07-06：已新增 `src/protocol/tool_ids.rs` 并从 `protocol::tool_ids` 暴露请求级工具 ID 映射/校验工具。
- 已实现 `ToolIdMap`，支持 Chat `tool_call_id` ↔ Anthropic `tool_use_id` 双向映射、identity 映射、冲突检测与确定性 pair 输出；当前 Chat↔Anthropic 无状态链路使用同 ID 保真。
- 已实现 `tool_id_map_from_request` / `validate_tool_result_pairs`，按 IR 请求历史扫描 assistant `ToolUse` 与 user/tool `ToolResult`，拒绝未知结果 ID、重复结果、重复 tool-use ID、错误 role 上的工具块和未完成配对，覆盖多 tool、多轮、结果顺序不同的配对场景。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-07 DeepSeek 消息交替规整 (`protocol/openai_chat/encode.rs`)
IR → Chat 请求方向：实现 DeepSeek 严格 user/assistant 交替约束处理——合并连续同 role 消息
（尤其 Anthropic 把多个 tool_result 放一个 user 消息、或拆成连续 user 的情况），DESIGN §6.4。
应用 `param_blocklist` 静默 drop、`n>1` 拒绝。

完成记录：
- 2026-07-06：已新增 `src/protocol/openai_chat/encode.rs` 并从 `protocol::openai_chat` 暴露 encoder，提供 `ir_request_to_chat` 将统一 IR 请求编码为 OpenAI Chat/DeepSeek 兼容请求。
- 已在编码前合并连续同 role IR 消息，支持 system 注入、user text/image 内容、assistant text/reasoning/tool_calls、Anthropic-style `ToolResult` 到 Chat `role:"tool"` 消息的拆分，并接入 M2-06 工具 ID 配对校验以保持 tool_call_id/tool_use_id 不错位。
- 已按 profile 应用 `param_blocklist` 静默丢弃 DeepSeek 不支持参数，保留/归一 `reasoning_effort`，并在 DeepSeek profile 下拒绝 `n > 1`；新增单元测试覆盖严格交替规整、多 tool_result、参数丢弃、reasoning 回传策略与多 choice 拒绝。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-08 装配链 3 端到端路由
加 `POST /v1/messages`（Anthropic 端点）：解析请求→IR→按 profile 构造 Chat 请求→调 DeepSeek→
流式或非流式响应经上述编码器返回。鉴权头翻译（`x-api-key`+`anthropic-version` ↔ `Bearer`，DESIGN §6.6）。
`max_tokens` 默认值处理（Anthropic 必填 → 给 Chat 合理默认）。system prompt hoist。

完成记录：
- 2026-07-06：已在 Axum router 装配 `POST /v1/messages`，将 Anthropic Messages 请求解析为 IR，应用 `max_tokens` 默认值后按 DeepSeek profile 编码为 Chat Completions 请求，并调用配置的 Chat-compatible 上游。
- 已实现后端 Bearer 鉴权装配：优先使用 `DEEPSEEK_API_KEY`，测试/本地可用 `LLM_PROXY_CHAT_COMPLETIONS_URL` 指向 mock 或兼容上游；`LLM_PROXY_ANTHROPIC_DEFAULT_MAX_TOKENS` 可覆盖默认输出上限。
- 已将非流式 Chat 响应解析为 IR 后编码为 Anthropic message JSON；流式 Chat SSE 经 `parse_openai_chat_sse` → `chat_sse_to_ir_events` → `ir_events_to_anthropic_sse` 转为 Anthropic SSE 返回。
- 新增 route 测试覆盖 system prompt hoist、默认 `max_tokens`、后端 Authorization Bearer 翻译、非流式响应编码、流式 SSE 转换与缺少后端 API key 的配置错误。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-09 链 3 集成测试
用 `wiremock` mock DeepSeek 后端，录制的 Claude Code 请求样本打到 `/v1/messages`，
用 `insta` 快照比对流式输出的 Anthropic SSE 序列。覆盖：纯文本、reasoning、带 tool-use 的多轮。

完成记录：
- 2026-07-06：已为 `POST /v1/messages` 增加 `wiremock` DeepSeek mock 集成测试样本，覆盖 Claude Code-style 纯文本、DeepSeek reasoning、以及带历史 tool_use/tool_result 的多轮 tool-use 请求。
- 新增 `insta` 快照比对完整 Anthropic SSE 事件序列，锁定 `message_start`、content block start/delta/stop、`message_delta` usage/stop_reason、`message_stop`，并对上游 Chat 请求 JSON 做 recorded-request 断言。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M2-RV 【Review】M2 链 3 + 真实联调
确认：**真实 Claude Code 指向本网关 + DeepSeek 后端，完成一次带工具调用的多轮对话**（PLAN M2 验收）。
核对流式 block index/start/stop 正确、tool ID 无错位、reasoning 正确呈现。检查是否偏离 DESIGN。记录偏差与遗留问题。

完成记录：
- 2026-07-06：已完成 M2 链 3 review。使用真实 Claude Code CLI 2.1.200 指向本地网关 `http://127.0.0.1:18080`，网关使用 `.envrc` 中的 `DEEPSEEK_API_KEY` 调用 DeepSeek 后端，未将任何凭据写入入库文件。
- 真实联调使用隔离的临时 `CLAUDE_CONFIG_DIR` 与临时工作区，指定 `--model deepseek-chat`，仅开放 `Read` 工具；Claude Code 读取 `weather.txt` 后带回 tool_result，并完成第二轮回答，验证真实多轮 tool-use 链路可用。
- 联调输出显示 DeepSeek reasoning 以 Anthropic thinking block 形式正确呈现，`tool_use` id 与后续 `tool_result.tool_use_id` 一致，最终 `stop_reason=end_turn`，未观察到 tool ID 错位或流式 block lifecycle 错误。
- 偏差记录：未发现 M2 链 3 与 DESIGN/PLAN 要求不一致的偏差；M2 当前仍保持无状态实现，未引入会话存储。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`、真实 Claude Code → llm-proxy `/v1/messages` → DeepSeek tool-use 多轮联调均通过。

---

## M3 — 链 1：Chat/DeepSeek → Responses（服务 Codex）✅ 可用里程碑②

### [DONE] M3-01 Responses 请求解析 (`protocol/responses/decode.rs`)
实现 `responses_request_to_ir(body:&Value) -> Result<IrRequest>`（解析 Codex 发来的请求）：
- `input`（全量历史数组）中各 item：`message`(role+content)、`function_call`、`function_call_output`、
  `reasoning`(带 `encrypted_content`) → 对应 IR ContentBlock
- `instructions`/`developer` → `IrRequest.system`
- `tools`、`tool_choice`、`max_output_tokens`→`max_tokens`
- 记录 Codex 发来的 `reasoning` item（M5/M6 会用到 encrypted_content 还原，此处先透传保存进 Thinking.opaque）

完成记录：
- 2026-07-06：已新增 `src/protocol/responses/decode.rs` 并从 `protocol::responses` 暴露 decoder。
- 已实现 `responses_request_to_ir`：解析 Responses `input` 全量历史中的 `message`、`function_call`、`function_call_output` 与 `reasoning` item；`reasoning.encrypted_content` 会以原始字节保存到 `Thinking.opaque`，设置 `source=Responses`、`echo_policy=Always`。
- 已支持 `instructions`/`developer` 以及 input 中 `system`/`developer` message hoist 到 `IrRequest.system`，并解析 Responses tool、tool_choice、`max_output_tokens`、采样/stop/stream 参数与 provider `extra`。
- 新增单元测试覆盖 Codex-style message/content、reasoning 保真、function call/output、工具定义与选择、system/developer hoist、字符串 input，以及无效 function arguments / 缺少 `encrypted_content` 的错误路径。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M3-02 Responses 非流式响应编码 (`protocol/responses/encode.rs`)
实现 `ir_response_to_responses(resp:&IrResponse) -> Value`：
构造 `response` 对象 + `output` 数组（`message` item 含 `output_text`；`function_call` item；
`reasoning` item）；`status`、`usage`（`input_tokens`/`output_tokens`）；stop 映射。
符合 Responses API 响应结构。

完成记录：
- 2026-07-06：已新增 `src/protocol/responses/encode.rs` 并从 `protocol::responses` 暴露 encoder。
- 已实现 `ir_response_to_responses`，输出 Responses `response` 对象，包含 `object=response`、动态 `created_at`、`status`/`incomplete_details`、`output`、无状态 `store=false`、`previous_response_id=null`、并行工具调用标记与 usage。
- 已覆盖 IR `Text` → `message`/`output_text`、`Thinking` → `reasoning`（保留 `encrypted_content` 且避免 `status=null`）、`ToolUse` → `function_call`，并为完整 IR 覆盖支持 `ToolResult` → `function_call_output`。
- 已将 `StopReason` 映射到 Responses 状态：正常结束、stop sequence、tool use 为 `completed`，max tokens 与其他上游停止原因映射为 `incomplete` 并写入 `incomplete_details.reason`；usage 输出 `input_tokens`、`output_tokens`、cache read details 与 `total_tokens`。
- 新增单元测试覆盖 reasoning/text/function_call/usage 编码、连续文本合并、无 `encrypted_content` reasoning、stop/status 映射与 tool result 编码。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M3-03 IR event → Responses SSE 编码 (`protocol/responses/stream.rs`) 🔒
实现 `IrEvent` 流 → Responses SSE：
`response.created`/`response.in_progress`；`response.output_item.added`（新 block）；
`response.output_text.delta`；`response.function_call_arguments.delta`/`.done`（tool 参数碎片）；
`response.output_item.done`；`response.completed`（含 usage）。
处理 reasoning item 的流式事件（thinking → reasoning delta）。

完成记录：
- 2026-07-06：已新增 `src/protocol/responses/stream.rs` 并从 `protocol::responses` 暴露 Responses SSE 编码模块。
- 已实现 `IrEvent` → Responses SSE 的状态机编码：`MessageStart` 生成 `response.created`/`response.in_progress`，block start/stop 生成 `response.output_item.added`/`response.output_item.done`，text delta 生成 `response.output_text.delta`，tool 参数碎片生成 `response.function_call_arguments.delta`/`.done`，terminal event 生成包含 output 与 usage 的 `response.completed`。
- 已覆盖 thinking/reasoning 流式编码：`ThinkingDelta` 会编码为 Responses reasoning item 的 `response.reasoning_text.delta`，并在 block stop 时输出 reasoning text/content part/item done 事件。
- 编码器会校验 message lifecycle、block index 递增、delta 类型与已开启 block 匹配、terminal delta 与 message stop 顺序，避免 Responses SSE 序列错位。
- 新增单元测试覆盖 reasoning/text/tool-call lifecycle、SSE wrapper 多帧输出、非连续 block index、错误类型 delta、缺少 terminal delta 的拒绝路径。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M3-04 Responses tool ID 映射与配对 🔒
实现 `tool_call_id`(Chat) ↔ Responses `call_id` 映射；`function_call`/`function_call_output` 配对链
（DESIGN §6.2/§6.3）。确保 Codex 多轮 agent 循环 ID 不断裂。加测试。

完成记录：
- 2026-07-06：已将共享工具 ID 映射扩展为 Chat ↔ client 协议工具 ID 的无状态双向映射，并新增 Responses `call_id` 专用插入、查询与校验入口。
- 已在 Responses 请求 decoder 中即时校验 `function_call` / `function_call_output` 配对链，拒绝孤立 output、重复 output、未回答 function_call 等会导致 Codex 多轮 agent 循环 ID 断裂的输入。
- 新增单元测试覆盖 Responses `call_id` 双向映射、多轮 agent loop ID 连续性、孤立 `function_call_output`、重复 output 与未回答 `function_call`。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M3-05 装配链 1 端到端路由
加 `POST /v1/responses`（Responses 端点）：解析→IR→Chat 请求→调 DeepSeek→编码返回（流式/非流式）。
`developer/system` 消息处理、`max_output_tokens` 映射、鉴权头翻译、profile 应用。

完成记录：
- 2026-07-06：已新增 `POST /v1/responses`，将 Codex/Responses 请求解析为 IR，再通过 DeepSeek profile 编码为 Chat Completions 请求并调用配置的 Chat 后端。
- 已覆盖非流式 Chat 响应 → Responses JSON 编码，以及 Chat SSE → IR event → Responses SSE 的流式返回；响应头设置为 `text/event-stream` 并禁用缓存。
- 已处理 Responses `instructions`/`developer` system hoist、`max_output_tokens` → Chat `max_tokens`、DeepSeek profile 参数过滤/`reasoning_effort` 归一，以及 Responses `Authorization: Bearer` 到上游 Bearer token 的鉴权头翻译。
- 新增路由测试覆盖非流式文本响应、流式 tool-use 多轮 ID 连续性与 bearer token fallback。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M3-06 抓取 Codex 真实 payload（解阻塞 M4）⚠️
搭一个临时的假 Responses 端点（或复用 M0 passthrough + dump），让真实 Codex 打过来，
dump 完整请求 payload。确认 DESIGN §4.4 未钉死项：Codex 客户端是否校验 reasoning item 的
`encrypted_content`/`id` 格式或长度（vs 纯透传）。把结论写进 DESIGN.md §7 的"仍未钉死"表。

完成记录：
- 2026-07-06：已使用隔离的临时 `CODEX_HOME`、占位 `OPENAI_API_KEY`、本地假 Responses 端点与真实 Codex CLI 0.142.5 抓取 `POST /v1/responses` 请求 payload；请求为 `stream=true`、`store=false`，每轮发送完整 `input` 历史，工具列表包含 `exec_command` 等 Codex CLI 工具。
- 已通过假 Responses SSE 返回包含 synthetic reasoning item + `exec_command` function_call 的响应，驱动 Codex 执行安全命令并发起第二轮请求；第二轮 payload 中包含上一轮 function_call/function_call_output 以及 reasoning item。
- 实测结论：Codex 客户端不校验 reasoning `encrypted_content` 格式，非 base64 内容可原样进入下一轮请求；响应侧非 `rs_` id 不会被客户端拒绝，但 Codex 下轮请求不回传 reasoning `id`/`status`，只回传 `type:"reasoning"`、`summary` 与 `encrypted_content`。已验证 32 KiB 与 256 KiB 级 `encrypted_content` 原样回传；绝对长度上限仍由 M4 长度保护任务防御性处理。
- 已将结论写入 `DESIGN.md` §4.4 与 §7；本任务仅修改文档/任务记录，未改编译产物，沿用上一轮绿色 `cargo fmt`/`cargo clippy`/`cargo test` 结果。

### [DONE] M3-07 链 1 集成测试
`wiremock` mock DeepSeek，录制的 Codex 请求打到 `/v1/responses`，`insta` 快照比对 Responses SSE 序列。
覆盖：文本、带 tool-use 多轮。

完成记录：
- 2026-07-06：已为 `POST /v1/responses` 增加 `wiremock` DeepSeek mock 集成测试，使用 Codex-style 录制请求样本覆盖纯文本流式响应与带历史 `function_call`/`function_call_output` 的多轮 tool-use 场景。
- 已新增 Responses SSE `insta` 快照，锁定 `response.created` / `response.in_progress`、文本 content part lifecycle、`response.output_text.delta`/`.done`、tool-call `response.function_call_arguments.delta`/`.done`、`response.output_item.done` 与 `response.completed` usage/output 序列；测试中仅归一动态 `created_at` 以保持快照稳定。
- 已断言 Codex Responses 请求到 Chat Completions 上游的映射：`instructions`/`developer` system hoist、`max_output_tokens` → `max_tokens`、DeepSeek `reasoning_effort` 归一、Responses `call_id` 与上游/下游 tool-call ID 连续性。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M3-RV 【Review】M3 链 1 + 真实联调
确认：**真实 Codex 指向本网关 + DeepSeek 后端，完成一次带工具调用的多轮对话**（PLAN M3 验收）。
核对 Responses SSE 事件序列、`call_id` 配对、M3-06 的 payload 结论已回填 DESIGN。记录偏差。

完成记录：
- 2026-07-06：已使用隔离临时 `CODEX_HOME`、占位 `OPENAI_API_KEY`、真实 Codex CLI 0.142.5、本地网关 `POST /v1/responses` 与真实 DeepSeek 后端完成链 1 真实联调；Codex 按要求调用 shell 工具执行 `printf m3-rv-tool-ok`，第二轮携带 tool output 后收到最终文本 `m3-rv-tool-ok`。
- Review 中发现真实 Codex 请求包含 `namespace` 工具与默认关闭的 `web_search` 工具；已修复 Responses decoder：将 `namespace.tools[]` 中的 function 工具展开为 Chat-compatible function tools，并忽略 `external_web_access=false` 的 disabled `web_search`，对 enabled web search 保持明确不支持错误。
- 已通过 capture proxy 核对真实 Responses SSE：首轮包含 `response.created` / `response.in_progress`、`response.function_call_arguments.delta` / `.done`、`response.output_item.done`、`response.completed`；第二轮包含文本 content part lifecycle、`response.output_text.delta` / `.done`、`response.completed`。
- 已核对 `call_id` 配对：首轮 DeepSeek tool call `call_00_pLHcGog6hCbXwnS2ZzRT7479` 在 Responses `function_call` 输出中出现，第二轮 Codex request 的 `function_call` 与 `function_call_output` 均携带同一 `call_id`。
- M3-06 payload 结论已确认存在于 DESIGN §4.4/§7：Codex 0.142.5 不校验 reasoning `encrypted_content` 格式，回传时只保留 `type`/`summary`/`encrypted_content`，不回传 reasoning `id`/`status`。
- 偏差记录：真实 Codex 0.142.5 会发送 `namespace` 与 disabled `web_search` tools；M3-RV 已按上述方式兼容，PLAN/DESIGN 的阶段级目标无需调整。
- 验证：修复前 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；修复后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；真实 Codex + DeepSeek 链 1 tool-use 多轮联调通过。

---

## M4 — Reasoning 保真机制（envelope + client-as-storage）

### [DONE] M4-01 定义 envelope 格式 (`reasoning/envelope.rs`)
定义无状态搬运不透明令牌的格式（DESIGN §4.3/§4.4）：
`struct Envelope { version:u8, source:Provider, payload:Vec<u8>, checksum }`。
`payload` 装原始 block 序列化结果（Anthropic thinking+signature，或 Responses reasoning item）。
提供 `wrap(source_block) -> String`（base64）和 `unwrap(&str) -> Result<SourceBlock>`。
加完整性校验（HMAC 或 CRC）防止对端篡改导致的静默错误。

完成记录：
- 2026-07-06：已新增 `src/reasoning/envelope.rs` 与 `src/reasoning/mod.rs`，并在模块树中接入 `reasoning`。
- 已定义 `SourceBlock { source, payload }` 与 `Envelope { version, source, payload, checksum }`；`payload` 保存原始 provider block 序列化字节，`wrap` 输出 base64 JSON envelope，`unwrap` 校验后还原 `SourceBlock`。
- 已使用 CRC32 覆盖 version、source 与 payload，防止客户端回传过程中篡改或损坏导致静默错误；新增单元测试覆盖 Responses reasoning item、Anthropic thinking block、payload/source 篡改、非法 base64 与不支持版本。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M4-02 伪装成合法 reasoning item (`reasoning/envelope.rs`) ⚠️
实现把 envelope 包装成 Responses 合法 reasoning item 结构：生成 `rs_` 前缀 id、`type:"reasoning"`，
envelope base64 放入 `encrypted_content`（DESIGN §4.4）。依据 M3-06 抓到的 Codex 校验行为调整
（若 Codex 校验 id/长度则严格伪装，若纯透传则宽松）。

完成记录：
- 2026-07-06：已在 `src/reasoning/envelope.rs` 实现 Responses reasoning item 包装/解包 helper：生成 `rs_` 前缀 id、`type:"reasoning"`、空 `summary`，并把 envelope base64 放入 `encrypted_content`。
- 解包逻辑复用 envelope checksum 校验；依据 M3-06 实测，当前 Codex 回传可缺省 `id`/`status`，因此解包时仅在 `id` 存在时校验 `rs_` 前缀，避免要求客户端回传不会保留的字段。
- 新增单元测试覆盖合法 item 结构、Codex 缺省 `id`/`status` 回传、非法 type/id 拒绝，以及 `encrypted_content` 篡改检测。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M4-03 Anthropic signature 侧对称实现 (`reasoning/envelope.rs`)
实现把 envelope 编码进 Anthropic `thinking` block 的 `signature` 字段（我们自签自验，DESIGN §4.3 链4）。
提供 `wrap_as_signature` / `unwrap_from_signature`。

完成记录：
- 2026-07-06：已在 `src/reasoning/envelope.rs` 实现 Anthropic signature 侧 envelope 对称搬运，提供 `wrap_as_signature` / `unwrap_from_signature`；signature 使用 `llm_proxy_sig_v1:` 前缀承载 base64 envelope，并复用 version/source/payload/checksum 自验逻辑。
- 新增单元测试覆盖 signature 包装结构、Responses reasoning payload 字节级往返、非本网关 signature 拒绝、空 payload 拒绝与 payload 篡改 checksum 检测。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过。

### [DONE] M4-04 reasoning item 字段保真处理 (`protocol/responses/`) 🔒
按 DESIGN §4.5 已知坑：转换 reasoning item 时**绝不丢 `encrypted_content`**，
正确处理 `status` 字段（API 拒绝 `status=null`，该省则省）。加针对性测试，防止 liteLLM/langchainjs 同款 400。

完成记录：
- 2026-07-06：已新增 Responses reasoning item 保真 helper，转换时要求保留 `encrypted_content`，并仅将 `status:null` 规范化为字段缺省，避免向 Responses API 发送会被拒绝的 null status。
- Responses decoder 现在会把规范化后的原始 reasoning item JSON 保存在 `Thinking.opaque`，encoder 在可用时原样恢复这些字段，避免重建时丢失 `encrypted_content`、`id`、`summary` 或 provider 扩展字段。
- 新增针对性回归测试覆盖 decode→encode 往返中 `encrypted_content` 字节保真、`status:null` 省略，以及已有合法 `status` 不被响应级状态覆盖。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M4-05 长度上限保护与降级接口（默认关闭）🔒
实现 envelope 超长检测；预留有状态降级 trait `ReasoningStore { put(id,block); get(id)->block }`，
默认 `NoopStore`（不启用）。仅当 envelope 超过阈值时才需 store（DESIGN §4.4 风险项）。
**不得默认引入状态**——这是无状态铁律的唯一例外，需显式配置开启。

完成记录：
- 2026-07-06：已在 `reasoning/envelope.rs` 中新增 `EnvelopeLimits`、`ReasoningStore` 与默认 `NoopStore`，默认 wrap/unwrap 路径保持无状态；超过阈值时若未显式配置 store 会返回协议映射错误。
- 新增 store-reference envelope 降级表示，仅当 inline envelope 超过配置阈值时调用 `ReasoningStore::put`；`unwrap_with_store` 会通过 `ReasoningStore::get` 恢复原始 block 并继续校验 provider source 与 checksum。
- 已为 raw envelope、Responses reasoning item 的 `encrypted_content`、Anthropic thinking `signature` 提供显式 store-aware API，并覆盖 under-limit 不用 store、默认禁用 store 拒绝、配置 store 后 round-trip、Responses/Anthropic 长度限制的单元测试。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M4-06 envelope round-trip 测试
单元测试：`wrap → 模拟客户端原样带回 → unwrap 还原原始 block`，字节级一致。
覆盖两侧（encrypted_content 侧、signature 侧）+ tool-use 场景 + 篡改检测（改一字节应校验失败）。

完成记录：
- 2026-07-06：已新增显式模拟客户端原样回传的 round-trip 单元测试，分别覆盖 Responses `encrypted_content` 侧与 Anthropic `signature` 侧。
- 新增用例包含 tool-use 相关 opaque payload，并断言 unwrap 后 `Provider` 与原始 payload bytes 字节级一致；已有篡改检测测试继续覆盖 encrypted_content/signature 改一字节后的 checksum 失败路径。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M4-RV 【Review】M4 reasoning 机制
确认：envelope round-trip 无损；伪装结构符合 M3-06 结论；无状态铁律未被破坏（降级接口默认关闭）；
字段保真测试覆盖已知 400 坑。检查 envelope 完整性校验有效。记录偏差。

完成记录：
- 2026-07-06：已复核 M4 reasoning envelope 机制：`wrap`/`unwrap`、Responses `encrypted_content` 包装、Anthropic `signature` 包装均保持原始 provider payload 字节级往返，包含 tool-use payload 场景。
- 已确认 Responses reasoning item 伪装结构符合 M3-06 实测结论：生成 `rs_` 前缀 id、`type:"reasoning"`、空 `summary`，并允许 Codex 回传时缺省 `id`/`status`，只要求 `encrypted_content` 原样带回。
- 已确认无状态铁律未被破坏：默认路径使用 `NoopStore`，超长 envelope 仅在显式配置 `ReasoningStore` 时降级为 store-reference；默认超长场景会明确报错而非静默引入状态。
- 已确认 Responses reasoning item 字段保真测试覆盖 DESIGN §4.5 已知 400 坑：`encrypted_content` 不丢，`status:null` 会省略，合法 status 与 provider 扩展字段保持不变。
- 已确认完整性校验覆盖 version/source/payload，payload 或 source 篡改会触发 checksum mismatch；未发现 M4 与 DESIGN/PLAN 要求不一致的偏差。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

---

## M5 — 链 4：Responses → Anthropic（服务 Claude Code，富↔富）

### [DONE] M5-01 Responses 后端客户端 (`provider/responses_backend.rs`)
实现向 Responses 后端发请求：强制 `store=false` + `include:["reasoning.encrypted_content"]`（DESIGN §7）。
处理鉴权、流式 bytes_stream。

完成记录：
- 2026-07-06：已新增 `src/provider/responses_backend.rs` 并从 `provider::responses_backend` 暴露 Responses 后端客户端。
- 已实现 `ResponsesBackendClient`：校验 Responses endpoint 与 API key，使用 `Authorization: Bearer` 调用上游，并返回未缓冲的 `reqwest::Response` 供后续链路通过 `bytes_stream()` 流式消费。
- 已在发送前强制请求体 `store=false`，并确保 `include` 包含 `reasoning.encrypted_content`；保留已有 include 项且避免重复，非法 include 形状会明确报错。
- 新增单元测试覆盖 store/include 强制、重复 include 去重、非法 include 拒绝、空 API key 拒绝、鉴权 JSON POST、流式响应读取与上游错误 body 透传。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M5-02 Responses 响应 → IR（reasoning 侧）(`protocol/responses/decode.rs`)
扩展 decoder：响应中的 reasoning item（`encrypted_content`）→ `Thinking{source:Responses, opaque:encrypted_content, echo_policy:Always}`。

完成记录：
- 2026-07-06：已在 `src/protocol/responses/decode.rs` 新增 `responses_response_to_ir`，将非流式 Responses response 的 `output` 解码为统一 `IrResponse`。
- 已实现 response `reasoning` item 解码：要求保留 `encrypted_content`，并映射为 `Thinking{source=Responses, opaque=encrypted_content bytes, echo_policy=Always}`；同时覆盖 message/function_call/function_call_output、status stop reason 与 usage/cache_read 映射。
- 新增单元测试覆盖 reasoning item → IR Thinking 的 raw opaque 字节映射，以及缺失 `encrypted_content` 的错误路径。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M5-03 IR Thinking → Anthropic thinking（envelope 编码）
在 Anthropic encoder 中：`Thinking{source:Responses}` → Anthropic `thinking` block，
`signature = envelope.wrap_as_signature(opaque)`（DESIGN §4.3 链4）。

完成记录：
- 2026-07-06：已更新 `src/protocol/anthropic/encode.rs`，将非流式 Anthropic response encoder 改为 fallible `Result<Value>`，避免 envelope/签名错误被 panic 或静默吞掉，并在 `/v1/messages` 非流式返回路径传播协议映射错误。
- 已实现 `Thinking{source=Responses}` → Anthropic `thinking` block：要求 `opaque` 中存在 Responses reasoning `encrypted_content` bytes，并通过 `SourceBlock{source=Responses,payload=opaque}` + `wrap_as_signature` 写入 Anthropic `signature`；`thinking` 文本保持原 IR 文本，缺省时输出空字符串。
- 已保留 Anthropic-origin thinking 的原有 signature 直通行为，并将无效 UTF-8 signature 改为明确协议映射错误；新增单元测试覆盖 Responses opaque 经 signature unwrap 后字节级一致，以及缺少 opaque 时拒绝。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M5-04 反向还原：Claude Code 带回的 thinking → Responses reasoning item
在 Anthropic decoder + Responses encoder 路径：Claude Code 回传的 `thinking` block 的 signature
→ `unwrap_from_signature` → 还原原始 Responses reasoning item，放回后端请求的 `input`（DESIGN §4.3）。

完成记录：
- 2026-07-06：已更新 Anthropic request decoder：对本网关生成的 `thinking.signature` 先识别 envelope 前缀，再用 `unwrap_from_signature` 校验并还原；`source=Responses` 的 payload 会恢复为 `Thinking{source=Responses, opaque=原 encrypted_content bytes, echo_policy=Always}`，非本网关真实 Anthropic signature 仍按 Anthropic-origin signature 保留。
- 已新增 Responses request encoder `ir_request_to_responses`，将 IR 历史编码为 Responses `input`：Responses-origin `Thinking` 会输出 `type:"reasoning"` item，优先保留完整 reasoning item JSON，否则用恢复出的 opaque bytes 写回 `encrypted_content`；同时覆盖 message、function_call、function_call_output、tools、tool_choice 与常用请求参数。
- 新增单元测试覆盖 Claude Code 带回 signature 的 unwrap、非 Responses envelope source 拒绝、Responses request `input` 中 reasoning item/encrypted_content 还原、保留 reasoning item 字段并省略 `status:null`、以及非 Responses-origin thinking 拒绝。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M5-05 富↔富流式 (Responses SSE ↔ Anthropic SSE)
复用 IR event 层：Responses SSE → IR event（新增 `stream/responses_decoder.rs`）→ Anthropic SSE。
两侧都是块结构，注意 index/类型对齐（DESIGN §6.1）。

完成记录：
- 2026-07-06：已新增 `src/stream/responses_decoder.rs` 并从 `stream::responses_decoder` 暴露 Responses SSE → IR event 状态机；支持 `response.created`/`response.in_progress`、message/reasoning/function_call output item lifecycle、文本/reasoning/tool 参数 delta、terminal usage/status 解码。
- 已扩展流式 IR，新增 `ThinkingMetadata{source,opaque}` 事件，用于在流式 reasoning block 结束前携带不透明推理载荷；Responses decoder 会从 `output_item.done` 或 terminal `response.output` 恢复 `encrypted_content` 并映射为 `source=Responses`。
- 已扩展 Anthropic SSE encoder：`ThinkingMetadata{source=Responses}` 会通过 envelope 编码为 Anthropic `signature_delta`，保证 Claude Code 后续带回的 thinking signature 可反向还原；同时让 Responses SSE encoder 能把同类 metadata 写回 reasoning `encrypted_content`，避免新增 IR 事件在另一侧被静默丢弃。
- 新增单元测试覆盖 Responses reasoning/text/function_call 流式解码、output_index 顺序校验、terminal encrypted_content 兜底恢复、缺失 encrypted_content 拒绝、Responses SSE → IR → Anthropic SSE signature_delta 端到端，以及 Responses SSE encoder 的 metadata 写回。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M5-06 装配链 4 + 集成测试
`/v1/messages` 支持路由到 Responses 后端。`wiremock` mock Responses 后端（含 encrypted_content reasoning item），
测试多轮 + tool-use，断言 reasoning signature 往返后端不报错。

完成记录：
- 2026-07-06：已将 `/v1/messages` 装配为可路由到 Responses 后端：Anthropic 请求先解析为 IR，再通过 `ir_request_to_responses` 编码为 Responses 请求，使用 `ResponsesBackendClient` 调用 `OPENAI_API_ENDPOINT`/`OPENAI_API_KEY` 配置的上游；非流式响应经 `responses_response_to_ir` → Anthropic message，流式响应经 `parse_reqwest_sse` → `responses_sse_to_ir_events` → Anthropic SSE。
- 路由选择在 M7 模型路由完成前采用临时安全默认：`deepseek-*` 模型继续走 Chat/DeepSeek，非 DeepSeek 模型在 Responses 后端配置存在时走 Responses；可用 `LLM_PROXY_ANTHROPIC_MESSAGES_BACKEND=chat|responses|auto` 显式覆盖。`TESTING.md` 已记录该临时联调方式。
- 新增 `wiremock` 集成测试覆盖 Responses 后端含 `encrypted_content` reasoning item 的多轮 tool-use：首轮 Responses reasoning 被包装为 Anthropic `thinking.signature`，次轮 Claude Code 回传 signature 后可还原为原始 `encrypted_content` 并随 `function_call_output` 发回 Responses 后端；另新增流式 Responses SSE → Anthropic SSE 路由测试，断言 `signature_delta` 可 unwrap 回原始 encrypted payload。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M5-RV 【Review】M5 链 4 + 真实联调
确认：**Claude Code 接 Responses 后端完成带 reasoning + tool-use 的多轮对话，签名往返无 400**（PLAN M5 验收）。
核对 encrypted_content 经 signature 往返无损、富↔富流式 index 正确。记录偏差。

完成记录：
- 2026-07-06：已复核 M5 链 4 装配：`/v1/messages` 的 Anthropic 请求可路由到 Responses 后端，强制 `store=false` 与 `include:["reasoning.encrypted_content"]`，非流式与流式路径均经 IR/IR event 桥接到 Anthropic 响应。
- 真实联调发现并修复两个直接阻塞项：Claude Code 的 Anthropic-only `output_config`/`thinking`/`context_management` 不再原样泄漏到 Responses 请求，`output_config.effort` 会映射为 Responses `reasoning.effort`；生成的 Responses `function_call_output` 请求项不再发送真实后端拒绝的 `is_error` 字段。
- 已用真实 Claude Code 2.1.200 指向本地网关、真实 Responses 后端 deployment `gpt-5.5`，完成两轮 tool-use 对话：首轮触发 Bash tool_use，工具结果回传后次轮正常得到最终文本，无 400。
- 已用真实 Responses 后端做三轮 raw M5 验证：第二轮返回 Anthropic `thinking.signature`（由 Responses `encrypted_content` envelope 生成），第三轮将该 signature 带回后端并成功完成，确认 signature → encrypted_content 往返被真实后端接受。
- 已核对富↔富流式 index 与 reasoning metadata 路径由 `responses_sse_to_ir_events`、`ir_events_to_anthropic_sse` 及路由集成测试锁定：非连续 output index 会拒绝，`signature_delta` 可 unwrap 回原始 encrypted payload。
- 偏差记录：M5 未发现行为偏差；真实联调使用当前 `.envrc` 后端实际可用的 `gpt-5.5` deployment alias，正式模型别名/路由仍按 M7-02 处理。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`、真实 Claude Code → `/v1/messages` → Responses backend tool-use、多轮 reasoning signature raw round-trip 均通过。

---

## M6 — 链 2：Anthropic → Responses（服务 Codex，富↔富）

### [DONE] M6-01 Anthropic 后端客户端 (`provider/anthropic_backend.rs`)
实现向真 Anthropic 后端发请求（`x-api-key` + `anthropic-version`），流式 bytes_stream。

完成记录：
- 2026-07-06：已新增 `src/provider/anthropic_backend.rs` 并从 `provider::anthropic_backend` 暴露 Anthropic 后端客户端。
- 已实现 `AnthropicBackendClient`：校验 Anthropic endpoint、API key 与 `anthropic-version`，使用 `x-api-key` + `anthropic-version` 头向上游发送已编码的 Anthropic Messages JSON 请求，并返回未缓冲的 `reqwest::Response` 供后续链路通过 `bytes_stream()` 流式消费。
- 已保持 Anthropic 请求体字段原样不改写，并对非对象请求体、非法 endpoint、空 API key、空 version 与上游错误 body 添加明确错误路径。
- 新增单元测试覆盖请求体保真、配置校验、鉴权/版本请求头、流式响应读取与上游错误 body 透传。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M6-02 Anthropic 响应 → IR（thinking 侧）
扩展 Anthropic decoder：响应 `thinking` block（含真 signature）→ `Thinking{source:Anthropic, opaque:signature, echo_policy:Always}`。
复用 `stream/anthropic_decoder.rs`（Anthropic SSE → IR event，若 M2/M5 未建则此处建）。

完成记录：
- 2026-07-06：已扩展 `protocol::anthropic::decode`，新增 `anthropic_response_to_ir`，可解析 Anthropic Messages 非流式响应并将 `thinking` block 的真实 `signature` 保存在 `Thinking{source:Anthropic, opaque:signature, echo_policy:Always}`。
- 已新增 `src/stream/anthropic_decoder.rs` 并从 `stream::anthropic_decoder` 暴露 Anthropic SSE → IR event 状态机，覆盖 `thinking_delta` 与 `signature_delta`，将签名作为 `IrEvent::ThinkingMetadata{source:Anthropic}` 保真输出，并在缺少签名时明确报错。
- 新增单元测试覆盖非流式 thinking signature 映射、缺失 signature 拒绝、流式 thinking/signature 事件解码、缺失 `signature_delta` 拒绝，以及默认 SSE `message` 事件类型解析。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M6-03 IR Thinking → Responses reasoning item（envelope 编码）
在 Responses encoder：`Thinking{source:Anthropic}` → reasoning item，
`encrypted_content = envelope.wrap`（含 thinking+真 signature），伪装成合法结构（M4-02）。

完成记录：
- 2026-07-06：已扩展 `protocol::responses::encode` 的非流式 Responses 响应编码路径，将 `Thinking{source:Anthropic}` 编码为 M4-02 Responses-compatible reasoning item。
- `encrypted_content` 现在由 envelope 包装序列化后的 Anthropic `thinking` block（含可读 thinking 文本与真实 `signature`）生成；输出 item 使用 `rs_llm_proxy...` id、`type:"reasoning"`、响应状态与 summary，并拒绝缺少真实签名的 Anthropic thinking。
- 新增单元测试覆盖 envelope payload 解包后还原 Anthropic thinking+signature，以及缺少签名时明确报错。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M6-04 反向还原：Codex 带回的 reasoning item → Anthropic thinking block
Responses decoder + Anthropic encoder 路径：Codex 回传 reasoning item 的 encrypted_content
→ `envelope.unwrap` → 还原**带原始签名**的 thinking block，发回 Anthropic 后端（后端验自己的签名，DESIGN §4.3 链2）。

完成记录：
- 2026-07-06：已扩展 `protocol::responses::decode`，当 Codex 回传的 Responses reasoning item 携带本网关生成的 Anthropic envelope 时，会校验并 unwrap `encrypted_content`，还原为 `Thinking{source=Anthropic, opaque=原始 signature, echo_policy=Always}`；普通 Responses reasoning item 继续按 M4 保真路径保存完整原始 item。
- 已在 `protocol::anthropic::encode` 新增 Anthropic backend request encoder `ir_request_to_anthropic`，将恢复出的 Anthropic-origin thinking 编码回 `thinking` block 并直通原始 `signature`，同时拒绝把 Responses-origin thinking 伪装发送给真实 Anthropic 后端。
- 新增单元测试覆盖 Codex 省略 `id/status` 后的 envelope 还原、gateway envelope 篡改拒绝、Anthropic request 中原始 thinking signature 编码，以及错误 source 的拒绝路径。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M6-05 富↔富流式 (Anthropic SSE ↔ Responses SSE)
Anthropic SSE → IR event → Responses SSE，index/类型对齐。

完成记录：
- 2026-07-06：已完成 Anthropic SSE → IR event → Responses SSE 富↔富流式桥接验证，覆盖 thinking/text/tool_use block 的顺序、Responses `output_index` 与 IR block index 对齐，以及 Responses SSE item 类型输出。
- 已修复 streaming Anthropic thinking metadata 的 envelope 编码：`ThinkingMetadata{source=Anthropic}` 不再只包装 signature 字节，而是在 reasoning block 结束时把完整 Anthropic `thinking` block（thinking text + 原始 signature）写入 Responses `encrypted_content` envelope，保证 Codex 后续回传可还原真实 Anthropic signature。
- 新增 targeted 单元测试覆盖 Responses SSE encoder 的 Anthropic reasoning envelope，以及完整 Anthropic SSE → IR event → Responses SSE 桥接，断言 tool call arguments、usage、block index 与 envelope payload 保真。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M6-06 可选：Anthropic 后端 cache_control 注入
纯函数从消息结构算 cache 断点，注入 Anthropic 请求省钱（DESIGN §3.1）。无状态。可作为可配置开关。

完成记录：
- 2026-07-06：已新增 `provider::anthropic_cache` 无状态 `cache_control` 注入模块，按 Anthropic prompt 顺序从 `tools` → `system` → `messages` 收集可缓存块，并为最近的最多 4 个可缓存断点注入 `{"type":"ephemeral"}`。
- 注入逻辑会跳过不可直接缓存的 `thinking` block 与空 text block，保留既有 `cache_control`，对字符串形式的 system/message content 做等价 text block 转换，并在已有断点超过 Anthropic 上限时明确报错。
- 已为 `AnthropicBackendClient` 增加默认关闭的 `AnthropicCacheControlInjection` 开关；M6-07 装配 `/v1/responses` → Anthropic 后端时可显式启用，不引入任何会话状态。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M6-07 装配链 2 + 集成测试
`/v1/responses` 支持路由到 Anthropic 后端。`wiremock` mock Anthropic（含 thinking+signature），
测试 Codex 多轮 + tool-use，断言 reasoning 往返后端验签通过。

完成记录：
- 2026-07-06：已将 `/v1/responses` 装配为可路由到 Anthropic Messages 后端：Responses 请求先解析为 IR，再通过 `ir_request_to_anthropic` 编码为 Anthropic 请求，使用 `AnthropicBackendClient` 调用 `ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN` 配置的上游；非流式响应经 `anthropic_response_to_ir` → Responses response，流式响应经 `parse_reqwest_sse` → `anthropic_sse_to_ir_events` → Responses SSE。
- 路由选择在 M7 模型路由完成前采用临时安全默认：`deepseek-*` 模型继续走 Chat/DeepSeek，非 DeepSeek 模型在 Anthropic 后端配置存在时走 Anthropic；可用 `LLM_PROXY_RESPONSES_BACKEND=chat|anthropic|auto` 显式覆盖。Anthropic 后端请求启用 M6-06 的无状态 cache-control 注入，并支持 `ANTHROPIC_DEFAULT_OPUS_MODEL` 作为临时模型覆盖。
- 修复了 Responses/Codex 历史编码到 Anthropic 后端时的相邻同角色消息拆分问题：`ir_request_to_anthropic` 现在会按 Anthropic 目标 role 合并相邻消息，使 reasoning item + function_call 同处 assistant turn、tool_result + 后续用户文本同处 user turn，保证多轮 tool-use 与 thinking signature 回传边界正确。
- 新增 route-level wiremock 集成测试覆盖 `/v1/responses` → Anthropic 的非流式多轮 tool-use reasoning signature 往返，以及 Anthropic SSE → Responses SSE 的 thinking/signature envelope 与 tool-use 流式输出。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M6-RV 【Review】M6 链 2 + 全链路
确认：**Codex 接 Anthropic 后端完成带 reasoning + tool-use 的多轮对话**（PLAN M6 验收）。
确认 **4 条链全部可用**。核对 signature 经 encrypted_content 往返无损。记录偏差。

完成记录：
- 2026-07-06：已完成 M6 链 2 review。使用隔离临时 `CODEX_HOME` 与临时工作区，将真实 Codex CLI 0.142.5 指向本地网关 `POST /v1/responses`，网关调用 `.envrc` 中配置的真实 Anthropic-compatible 后端；Codex 成功通过 shell tool 执行 `printf m6-rv-tool-ok`，第二轮携带工具输出后最终返回 `m6-rv-tool-ok`。
- Review 中发现并修复三个直接阻塞真实 Codex/Anthropic 后端的问题：Codex 0.142.5 当前请求会携带 Responses `custom` 工具（`apply_patch`）与 `tool_search` 工具，现已分别适配为 Anthropic-compatible 工具声明；真实后端使用 token-shaped `ANTHROPIC_AUTH_TOKEN` 时需要 `Authorization: Bearer`，现保留 `sk-ant-*` 官方 key 的 `x-api-key` 路径并为 token credential 使用 bearer；该后端启用 thinking 需要透传 `output_config.effort`，现已允许 `output_config` 进入 Anthropic 后端请求。
- 已用真实 Anthropic-compatible 后端做 Codex-protocol reasoning + tool-use 往返验证：首轮 Responses 请求启用 `thinking:{type:"adaptive"}` + `output_config:{effort:"high"}` 并要求调用 `lookup_weather`，返回 output 类型为 `reasoning,message,function_call`；第二轮将第一轮 reasoning item 的 `encrypted_content`、function_call 与 function_call_output 带回，后端接受还原后的原始 Anthropic thinking signature 并完成响应，确认 signature 经 encrypted_content 往返无损且无 400。
- 已核对全链路状态：M2-RV（Claude Code → DeepSeek）、M3-RV（Codex → DeepSeek）、M5-RV（Claude Code → Responses）均已完成真实联调记录；本次 M6-RV 补齐 Codex → Anthropic 后端后，4 条链均可用。
- 偏差/环境记录：`.envrc` 中当前 `ANTHROPIC_DEFAULT_OPUS_MODEL` 带有字面 `[1m]` 后缀，真实联调使用进程内 sanitized override（去除该后缀）启动网关；未修改或提交 `.envrc`，正式模型别名/配置仍按 M7-01/M7-02 处理。
- 验证：`cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`、真实 Codex CLI → `/v1/responses` → Anthropic backend tool-use 多轮联调、真实 Anthropic-compatible backend reasoning encrypted_content/signature raw round-trip 均通过。

---

## M7 — 加固与运维

### [DONE] M7-01 配置系统 (`config.rs`)
实现配置加载（文件 TOML/YAML + 环境变量覆盖）：后端列表（类型/base_url/凭据/profile）、
模型别名映射（client model 名 → 后端 + 改名，DESIGN §6.6）、监听地址、开关（cache 注入、reasoning store）。
用 `serde` 反序列化为强类型 `Config`。启动时校验。

完成记录：
- 2026-07-06：已在 `src/config.rs` 实现强类型 `Config`，支持从 `LLM_PROXY_CONFIG` 指向的 TOML/YAML 文件加载 listen 地址、后端列表、模型别名、临时路由覆盖以及 cache 注入/reasoning store 开关。
- 已实现环境变量覆盖：保留既有 `LLM_PROXY_ADDR`、`LLM_PROXY_UPSTREAM_URL`、`DEEPSEEK_API_KEY`、`LLM_PROXY_CHAT_COMPLETIONS_URL`、`OPENAI_API_ENDPOINT`/`OPENAI_API_KEY`、`ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN`、`ANTHROPIC_VERSION`、`ANTHROPIC_DEFAULT_OPUS_MODEL`、`LLM_PROXY_ANTHROPIC_DEFAULT_MAX_TOKENS`、`LLM_PROXY_ANTHROPIC_MESSAGES_BACKEND`、`LLM_PROXY_RESPONSES_BACKEND` 行为，并新增 `LLM_PROXY_BACKENDS`、`LLM_PROXY_MODEL_ALIASES`、`LLM_PROXY_ANTHROPIC_CACHE_INJECTION`、`LLM_PROXY_REASONING_STORE` 结构化覆盖。
- 启动路径现在先加载并校验配置，再构造 `AppState`；现有 env-only 启动方式继续可用，Anthropic cache-control 注入通过配置开关控制且默认保持当前启用行为。
- 新增配置单元测试覆盖 TOML/YAML 解析、文件扩展名识别、env 覆盖优先级、legacy 后端创建、backend JSON 覆盖、模型别名校验、重复后端与缺失 profile 拒绝。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M7-02 模型路由 (`provider/router.rs`)
根据请求的 model 名 + 端点类型，用配置选择后端与 profile，并改写发往后端的 model 名。
无匹配时返回清晰错误。

完成记录：
- 2026-07-06：已新增 `src/provider/router.rs`，实现 `ModelRouter`，按前端端点（Anthropic Messages / OpenAI Responses）与请求 model 选择允许的后端协议、profile 与上游 model 名；精确 `model_aliases` 优先，legacy route override 仅在无精确别名时生效。
- 已将 `/v1/messages` 与 `/v1/responses` 路由改为通过 `ModelRouter` 统一解析后端，编码上游请求前改写 `IrRequest.model`，并从选中的 backend 读取 Chat/Responses/Anthropic URL、凭据、Anthropic version、默认 model 与 max token 设置。
- 已保留 env-only 旧行为：无显式 chat backend 时仍注入默认 DeepSeek Chat backend，可继续使用客户端 `x-api-key`/`Authorization` 作为上游 token；非 DeepSeek 模型在配置了 rich backend 时继续按端点优先路由到 Responses 或 Anthropic。
- 新增路由单元测试覆盖精确别名改名、端点不兼容后端拒绝、Responses/Anthropic 隐式 rich backend 选择、legacy override 缺失后端错误，以及默认 DeepSeek 兼容路径；现有 route-level 集成测试已改为从 `Config` 构造状态。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M7-03 错误映射完善 (`error.rs`)
各协议错误 JSON 结构、错误类型分类、状态码、`Retry-After`/限流头翻译（DESIGN §6.6）。
后端 4xx/5xx 映射为对应前端协议的错误格式（Anthropic error / Responses error 结构不同）。

完成记录：
- 2026-07-06：已在 `src/error.rs` 增加协议感知错误格式化，保留 generic 旧格式，同时为 Anthropic Messages 返回 `{"type":"error","error":{...}}`，为 OpenAI Responses 返回 OpenAI-style `{"error":{"message","type","param","code"}}`。
- 已将错误分类统一映射到 invalid request、auth、permission、not found、rate limit、server 等类型，并让 `/v1/messages` 与 `/v1/responses` 在本地解析/配置/协议错误和 JSON extractor 失败时返回对应前端协议的错误结构。
- 已把上游非成功响应从仅命名为 4xx 的错误扩展为覆盖 4xx/5xx 的 `UpstreamStatus`，保留上游状态码和响应体，并抽取上游 JSON error message 生成前端可读错误。
- 已保存并翻译 `Retry-After` 与 OpenAI/Anthropic rate-limit headers：OpenAI-style `x-ratelimit-*` 可翻译为 Anthropic-style `anthropic-ratelimit-*`，Anthropic-style headers 也可翻译到 Responses 前端。
- 新增单元测试和路由测试覆盖 Anthropic/Responses 错误 JSON、上游 429/503 状态映射、`Retry-After` 与限流头翻译。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M7-04 不支持特性表 (`protocol/capability.rs`)
集中管理每个 `IR→协议` 方向的特性支持决策：drop / emulate / 400（DESIGN §6.5）。
例如：Responses json_schema → Anthropic 用 tool 模拟；不支持的参数明确 drop 或拒绝。

完成记录：
- 2026-07-06：已新增 `src/protocol/capability.rs`，集中定义 `IR→OpenAI Chat`、`IR→Anthropic Messages`、`IR→OpenAI Responses` 的 extra feature 决策表，统一表达 pass-through / drop / emulate / reject 行为。
- 已将 Chat、Anthropic、Responses 请求 encoder 改为调用能力表：Responses `text.format` 可转换为 Chat `response_format`，Chat `response_format` 可转换为 Responses `text.format`，Anthropic 对 Responses/Chat 结构化输出使用合成 tool 强制模拟；无状态无法保真的 `previous_response_id`、启用型 `store`/`background` 等会明确 400 拒绝或按表 drop。
- 已补充单元测试覆盖能力表分类、extra 过滤、Responses json_schema → Anthropic tool 模拟、Responses json_schema → Chat `response_format`、Chat `response_format` → Responses `text.format`、以及结构化输出与用户 tools 冲突时的拒绝路径。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M7-05 Observability
`tracing` 结构化日志：每请求记录链路、后端、耗时、token 用量。加可选的请求/响应 dump（调试开关，脱敏凭据）。

完成记录：
- 2026-07-06：已新增 `src/observability.rs`，为 `/v1/messages` 与 `/v1/responses` 增加请求级结构化 observability context，记录 request id、前端端点、转换链路、后端名称/类型、客户端 model、上游 model、是否流式、耗时与 token/cache usage。
- 非流式路径在解析上游响应为 IR 后记录 usage；流式路径在 IR event 层观察 `MessageDelta.usage` 与 `MessageStop`，在流完成时记录 token 用量，并对流式错误/缺少 terminal event 记录 warning。
- 新增 `LLM_PROXY_OBSERVABILITY_DUMP` / `switches.observability_dump` 调试开关；开启后记录前端请求、上游请求、上游响应与前端响应 JSON dump，并对鉴权头、API key/token/secret/password、`encrypted_content` 与 `signature` 做递归脱敏。`TESTING.md` 已补充启用方式。
- 新增单元测试覆盖 JSON/header dump 脱敏与配置开关解析。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 均通过。

### [DONE] M7-06 限流与重试
对后端请求的重试与指数退避（尊重 `Retry-After`）。可配置并发/超时。

完成记录：
- 2026-07-06：已新增 `provider::backend_request` 统一后端 HTTP 请求控制层，支持对 408/425/429/5xx 与连接/超时类传输错误按指数退避重试，并解析 `Retry-After` 的秒数与 HTTP-date 格式。
- 已新增全局 `backend_request` 配置与环境变量覆盖：`LLM_PROXY_BACKEND_MAX_RETRIES`、`LLM_PROXY_BACKEND_INITIAL_BACKOFF_MS`、`LLM_PROXY_BACKEND_MAX_BACKOFF_MS`、`LLM_PROXY_BACKEND_TIMEOUT_MS`、`LLM_PROXY_BACKEND_CONCURRENCY_LIMIT`；默认保持无重试、无超时、无限并发以兼容既有行为。
- 已将该控制层接入 Chat、Responses、Anthropic 三类后端请求；并发限制通过 permit 包装 `BackendResponse`，流式响应会一直持有 permit 直到 body stream 结束或被丢弃，避免只限制到响应头。
- 新增单元/路由测试覆盖 retryable status 重试、`Retry-After`、非 retryable 400 不重试、per-attempt timeout、流式响应持有并发 permit、配置文件/env 覆盖，以及 `/v1/messages` Chat backend 路由重试装配。
- 验证：变更前基线 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets` 通过；变更后 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`cargo test --all --all-targets` 均通过。

### M7-07 `[TODO]` 端到端回归测试套件
4 条链各录制若干真实会话（文本/reasoning/tool-use/多轮），做快照回归。整理成 `cargo test` 可跑的套件。

### M7-08 `[TODO]` README 与部署文档
写 `README.md`：配置示例、如何把 Claude Code / Codex 指向本网关、支持的后端与 profile、已知限制。
指向 `TESTING.md`（测试与真实世界联调）。

### M7-09 `[TODO]` GitHub CI pipeline
在 `.github/workflows/ci.yml` 建 GitHub Actions pipeline，push / PR 到 `main` 时触发：
- 步骤：`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all --all-targets`。
- 用稳定版 Rust（edition 2024 需足够新的工具链），并缓存 cargo registry / target 加速。
- **只跑不依赖网络的测试**：绝不加 `--ignored`。用真实 `codex` / `claude` CLI + 真实后端凭据的
  端到端测试（`TESTING.md` §5）都标了 `#[ignore]`，CI 环境无这些 CLI / 无 `.envrc` 凭据，
  必须默认跳过，避免失败与凭据泄露。
参考 `TESTING.md` §1（CI 跑的测试范围）与 §5.4（e2e 测试为何默认忽略）。

### M7-RV `[TODO]` 【Review】M7 加固 + 项目验收
确认：多后端配置化可用、错误可读、有基本可观测性、回归套件通过。
最终核对整个项目未偏离 DESIGN.md 的无状态铁律与保真目标。列出所有遗留 `[BLOCKED]` 项与后续建议。
