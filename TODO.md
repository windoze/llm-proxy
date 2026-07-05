# LLM Proxy 任务列表

> 本文件是 [PLAN.md](./PLAN.md) 的可执行任务分解，供 coding agent 逐条执行。
>
> **约定**
> - 任务按执行顺序排列，编号形如 `M1-01`。
> - 标题中的 `[TODO]` 是状态标记，执行完成后由 agent 更新为 `[DONE]`（或 `[BLOCKED]` 并注明原因）。
> - 每个里程碑最后有一个 `-RV` review 任务，确认该里程碑实现正确且未偏离 [DESIGN.md](./DESIGN.md) 目标。
> - 参考文档：`DESIGN.md`（设计与约束）、`PLAN.md`（里程碑）。文中 `DESIGN §x` 指 DESIGN.md 章节。
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

### M1-05 `[TODO]` 实现 DeepSeek profile (`provider/deepseek.rs`)
实现 `CapabilityProfile`，严格按 DESIGN §5：
- `param_blocklist`: `["temperature","top_p","presence_penalty","frequency_penalty","logprobs","top_logprobs"]`
- `normalize_reasoning_effort`: `low|medium -> high`, `xhigh -> max`, 默认 `high`
- `reasoning_echo_policy`: `OnlyWithToolCall`
- `supports_multiple_choices`: `false`
- `thinking_model`: `model == "deepseek-reasoner"`（`deepseek-chat` 为 false）
- `base_url`: `https://api.deepseek.com`
在代码注释标注 DESIGN §5 的"官方文档版本不一致"警告，说明以 `thinking_mode` 新页为准。

### M1-06 `[TODO]` OpenAI Chat/DeepSeek 请求解析 (`protocol/openai_chat/decode.rs`)
实现 `chat_request_to_ir(body:&Value, profile:&dyn CapabilityProfile) -> Result<IrRequest>`：
- messages 中 `role:system` → `IrRequest.system`
- `tool_calls`（assistant）→ `ContentBlock::ToolUse`；`role:tool` 消息 → `ToolResult`（配 `tool_call_id`）
- `reasoning_content` → `ContentBlock::Thinking{source:DeepSeek, echo_policy: profile 决定}`
- `tools`/`tool_choice`/`max_tokens`/`temperature` 等映射到 IR
覆盖 DESIGN §4.1(DeepSeek 条件回传)、§6.3(工具挂载)。

### M1-07 `[TODO]` OpenAI Chat/DeepSeek 响应解析 (`protocol/openai_chat/decode.rs`)
实现 `chat_response_to_ir(body:&Value) -> Result<IrResponse>`（非流式）：
`choices[0].message.content` → Text block；`reasoning_content` → Thinking；`tool_calls` → ToolUse；
`finish_reason` → `StopReason`（`stop→EndTurn, length→MaxTokens, tool_calls→ToolUse`）；
`usage` → `Usage`（含 `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens` → cache_read/miss）。

### M1-08 `[TODO]` M1 单元测试
在 `protocol/openai_chat/` 加 `#[cfg(test)]`：准备 DeepSeek 响应 JSON 样本
（含 `reasoning_content` + `tool_calls` 组合），断言解析出的 IR 结构正确，
echo_policy 在有/无 tool_calls 场景符合 §4.1。用 `insta` 做快照。

### M1-RV `[TODO]` 【Review】M1 IR 与解析
确认：IR 类型覆盖 DESIGN §2.1/§4.2 全部块类型；DeepSeek profile 规则与 §5 逐条一致；
解析测试通过；`reasoning_content` echo_policy 逻辑正确。检查是否有硬编码应进 profile 的东西。记录偏差。

---

## M2 — 链 3：Chat/DeepSeek → Anthropic（服务 Claude Code）✅ 可用里程碑①

### M2-01 `[TODO]` Anthropic 请求解析 (`protocol/anthropic/decode.rs`)
实现 `anthropic_request_to_ir(body:&Value) -> Result<IrRequest>`（解析 Claude Code 发来的请求）：
- 顶层 `system`（string 或 block 数组）→ `IrRequest.system`
- `messages[].content` 的 block（`text`/`image`/`tool_use`/`tool_result`/`thinking`）→ IR ContentBlock
- `tools`（`input_schema`）、`tool_choice`（`auto/any/tool` → IR `Auto/Required/Tool`）、`max_tokens`
- `thinking` block（带 signature）→ `Thinking{source:Anthropic, opaque:signature, echo_policy:Always}`

### M2-02 `[TODO]` Anthropic 非流式响应编码 (`protocol/anthropic/encode.rs`)
实现 `ir_response_to_anthropic(resp:&IrResponse) -> Value`：
IR content → Anthropic `content` block 数组；`ToolUse` → `tool_use` block；
`Thinking` → `thinking` block（signature 从 opaque）；`StopReason` → `end_turn/max_tokens/stop_sequence/tool_use`；
`Usage` → `{input_tokens,output_tokens,cache_read_input_tokens?}`。输出符合 Anthropic Messages API 响应结构。

### M2-03 `[TODO]` 通用 SSE 解析基础设施 (`stream/sse.rs`)
封装基于 `eventsource-stream` 的辅助：把 `reqwest` bytes_stream 解析为 `(event_type, data)` 迭代，
处理 OpenAI Chat 的 `data: {...}` + `data: [DONE]` 终止。为下游状态机提供干净输入。

### M2-04 `[TODO]` Chat SSE → IR event 状态机 (`stream/chat_decoder.rs`) 🔒
实现把 OpenAI Chat 流式 chunk 转成 `IrEvent` 流的**有状态**解析器：
- 首个 chunk → `MessageStart`
- `delta.content` → 维护 text block（首次发 `BlockStart{Text}`）+ `TextDelta`
- `delta.reasoning_content` → thinking block + `ThinkingDelta`
- `delta.tool_calls[i]` → 按 tool index 维护 ToolUse block，`function.arguments` 碎片 → `ToolUseDelta{partial_json}`
- `finish_reason` → 关闭所有开启的 block（`BlockStop`）+ `MessageDelta{stop_reason}` + `MessageStop`
**tool-call 流式重组是重点**（DESIGN §6.2），需处理碎片无边界问题。加单元测试覆盖多 tool、reasoning+content 混合。

### M2-05 `[TODO]` IR event → Anthropic SSE 编码 (`protocol/anthropic/stream.rs`) 🔒
实现 `IrEvent` 流 → Anthropic SSE 事件流：
`MessageStart→message_start`；`BlockStart→content_block_start`（按 index 与 type）；
`TextDelta→content_block_delta{text_delta}`；`ThinkingDelta→content_block_delta{thinking_delta}`；
`ToolUseDelta→content_block_delta{input_json_delta{partial_json}}`；`BlockStop→content_block_stop`；
`MessageDelta→message_delta{stop_reason,usage}`；`MessageStop→message_stop`。
维护正确的 block index 序列（DESIGN §6.1）。

### M2-06 `[TODO]` tool ID 映射与配对 (`protocol/mod.rs` 或 `ir/`) 🔒
实现 `tool_call_id`(Chat) ↔ `tool_use_id`(Anthropic) 的映射与"调用→结果"配对链保真
（DESIGN §6.2）。在请求方向：Anthropic 的 `tool_result.tool_use_id` 要对回 Chat 的 `tool_call_id`。
确保多轮对话中 ID 不错位。加测试。

### M2-07 `[TODO]` DeepSeek 消息交替规整 (`protocol/openai_chat/encode.rs`)
IR → Chat 请求方向：实现 DeepSeek 严格 user/assistant 交替约束处理——合并连续同 role 消息
（尤其 Anthropic 把多个 tool_result 放一个 user 消息、或拆成连续 user 的情况），DESIGN §6.4。
应用 `param_blocklist` 静默 drop、`n>1` 拒绝。

### M2-08 `[TODO]` 装配链 3 端到端路由
加 `POST /v1/messages`（Anthropic 端点）：解析请求→IR→按 profile 构造 Chat 请求→调 DeepSeek→
流式或非流式响应经上述编码器返回。鉴权头翻译（`x-api-key`+`anthropic-version` ↔ `Bearer`，DESIGN §6.6）。
`max_tokens` 默认值处理（Anthropic 必填 → 给 Chat 合理默认）。system prompt hoist。

### M2-09 `[TODO]` 链 3 集成测试
用 `wiremock` mock DeepSeek 后端，录制的 Claude Code 请求样本打到 `/v1/messages`，
用 `insta` 快照比对流式输出的 Anthropic SSE 序列。覆盖：纯文本、reasoning、带 tool-use 的多轮。

### M2-RV `[TODO]` 【Review】M2 链 3 + 真实联调
确认：**真实 Claude Code 指向本网关 + DeepSeek 后端，完成一次带工具调用的多轮对话**（PLAN M2 验收）。
核对流式 block index/start/stop 正确、tool ID 无错位、reasoning 正确呈现。检查是否偏离 DESIGN。记录偏差与遗留问题。

---

## M3 — 链 1：Chat/DeepSeek → Responses（服务 Codex）✅ 可用里程碑②

### M3-01 `[TODO]` Responses 请求解析 (`protocol/responses/decode.rs`)
实现 `responses_request_to_ir(body:&Value) -> Result<IrRequest>`（解析 Codex 发来的请求）：
- `input`（全量历史数组）中各 item：`message`(role+content)、`function_call`、`function_call_output`、
  `reasoning`(带 `encrypted_content`) → 对应 IR ContentBlock
- `instructions`/`developer` → `IrRequest.system`
- `tools`、`tool_choice`、`max_output_tokens`→`max_tokens`
- 记录 Codex 发来的 `reasoning` item（M5/M6 会用到 encrypted_content 还原，此处先透传保存进 Thinking.opaque）

### M3-02 `[TODO]` Responses 非流式响应编码 (`protocol/responses/encode.rs`)
实现 `ir_response_to_responses(resp:&IrResponse) -> Value`：
构造 `response` 对象 + `output` 数组（`message` item 含 `output_text`；`function_call` item；
`reasoning` item）；`status`、`usage`（`input_tokens`/`output_tokens`）；stop 映射。
符合 Responses API 响应结构。

### M3-03 `[TODO]` IR event → Responses SSE 编码 (`protocol/responses/stream.rs`) 🔒
实现 `IrEvent` 流 → Responses SSE：
`response.created`/`response.in_progress`；`response.output_item.added`（新 block）；
`response.output_text.delta`；`response.function_call_arguments.delta`/`.done`（tool 参数碎片）；
`response.output_item.done`；`response.completed`（含 usage）。
处理 reasoning item 的流式事件（thinking → reasoning delta）。

### M3-04 `[TODO]` Responses tool ID 映射与配对 🔒
实现 `tool_call_id`(Chat) ↔ Responses `call_id` 映射；`function_call`/`function_call_output` 配对链
（DESIGN §6.2/§6.3）。确保 Codex 多轮 agent 循环 ID 不断裂。加测试。

### M3-05 `[TODO]` 装配链 1 端到端路由
加 `POST /v1/responses`（Responses 端点）：解析→IR→Chat 请求→调 DeepSeek→编码返回（流式/非流式）。
`developer/system` 消息处理、`max_output_tokens` 映射、鉴权头翻译、profile 应用。

### M3-06 `[TODO]` 抓取 Codex 真实 payload（解阻塞 M4）⚠️
搭一个临时的假 Responses 端点（或复用 M0 passthrough + dump），让真实 Codex 打过来，
dump 完整请求 payload。确认 DESIGN §4.4 未钉死项：Codex 客户端是否校验 reasoning item 的
`encrypted_content`/`id` 格式或长度（vs 纯透传）。把结论写进 DESIGN.md §7 的"仍未钉死"表。

### M3-07 `[TODO]` 链 1 集成测试
`wiremock` mock DeepSeek，录制的 Codex 请求打到 `/v1/responses`，`insta` 快照比对 Responses SSE 序列。
覆盖：文本、带 tool-use 多轮。

### M3-RV `[TODO]` 【Review】M3 链 1 + 真实联调
确认：**真实 Codex 指向本网关 + DeepSeek 后端，完成一次带工具调用的多轮对话**（PLAN M3 验收）。
核对 Responses SSE 事件序列、`call_id` 配对、M3-06 的 payload 结论已回填 DESIGN。记录偏差。

---

## M4 — Reasoning 保真机制（envelope + client-as-storage）

### M4-01 `[TODO]` 定义 envelope 格式 (`reasoning/envelope.rs`)
定义无状态搬运不透明令牌的格式（DESIGN §4.3/§4.4）：
`struct Envelope { version:u8, source:Provider, payload:Vec<u8>, checksum }`。
`payload` 装原始 block 序列化结果（Anthropic thinking+signature，或 Responses reasoning item）。
提供 `wrap(source_block) -> String`（base64）和 `unwrap(&str) -> Result<SourceBlock>`。
加完整性校验（HMAC 或 CRC）防止对端篡改导致的静默错误。

### M4-02 `[TODO]` 伪装成合法 reasoning item (`reasoning/envelope.rs`) ⚠️
实现把 envelope 包装成 Responses 合法 reasoning item 结构：生成 `rs_` 前缀 id、`type:"reasoning"`，
envelope base64 放入 `encrypted_content`（DESIGN §4.4）。依据 M3-06 抓到的 Codex 校验行为调整
（若 Codex 校验 id/长度则严格伪装，若纯透传则宽松）。

### M4-03 `[TODO]` Anthropic signature 侧对称实现 (`reasoning/envelope.rs`)
实现把 envelope 编码进 Anthropic `thinking` block 的 `signature` 字段（我们自签自验，DESIGN §4.3 链4）。
提供 `wrap_as_signature` / `unwrap_from_signature`。

### M4-04 `[TODO]` reasoning item 字段保真处理 (`protocol/responses/`) 🔒
按 DESIGN §4.5 已知坑：转换 reasoning item 时**绝不丢 `encrypted_content`**，
正确处理 `status` 字段（API 拒绝 `status=null`，该省则省）。加针对性测试，防止 liteLLM/langchainjs 同款 400。

### M4-05 `[TODO]` 长度上限保护与降级接口（默认关闭）🔒
实现 envelope 超长检测；预留有状态降级 trait `ReasoningStore { put(id,block); get(id)->block }`，
默认 `NoopStore`（不启用）。仅当 envelope 超过阈值时才需 store（DESIGN §4.4 风险项）。
**不得默认引入状态**——这是无状态铁律的唯一例外，需显式配置开启。

### M4-06 `[TODO]` envelope round-trip 测试
单元测试：`wrap → 模拟客户端原样带回 → unwrap 还原原始 block`，字节级一致。
覆盖两侧（encrypted_content 侧、signature 侧）+ tool-use 场景 + 篡改检测（改一字节应校验失败）。

### M4-RV `[TODO]` 【Review】M4 reasoning 机制
确认：envelope round-trip 无损；伪装结构符合 M3-06 结论；无状态铁律未被破坏（降级接口默认关闭）；
字段保真测试覆盖已知 400 坑。检查 envelope 完整性校验有效。记录偏差。

---

## M5 — 链 4：Responses → Anthropic（服务 Claude Code，富↔富）

### M5-01 `[TODO]` Responses 后端客户端 (`provider/responses_backend.rs`)
实现向 Responses 后端发请求：强制 `store=false` + `include:["reasoning.encrypted_content"]`（DESIGN §7）。
处理鉴权、流式 bytes_stream。

### M5-02 `[TODO]` Responses 响应 → IR（reasoning 侧）(`protocol/responses/decode.rs`)
扩展 decoder：响应中的 reasoning item（`encrypted_content`）→ `Thinking{source:Responses, opaque:encrypted_content, echo_policy:Always}`。

### M5-03 `[TODO]` IR Thinking → Anthropic thinking（envelope 编码）
在 Anthropic encoder 中：`Thinking{source:Responses}` → Anthropic `thinking` block，
`signature = envelope.wrap_as_signature(opaque)`（DESIGN §4.3 链4）。

### M5-04 `[TODO]` 反向还原：Claude Code 带回的 thinking → Responses reasoning item
在 Anthropic decoder + Responses encoder 路径：Claude Code 回传的 `thinking` block 的 signature
→ `unwrap_from_signature` → 还原原始 Responses reasoning item，放回后端请求的 `input`（DESIGN §4.3）。

### M5-05 `[TODO]` 富↔富流式 (Responses SSE ↔ Anthropic SSE)
复用 IR event 层：Responses SSE → IR event（新增 `stream/responses_decoder.rs`）→ Anthropic SSE。
两侧都是块结构，注意 index/类型对齐（DESIGN §6.1）。

### M5-06 `[TODO]` 装配链 4 + 集成测试
`/v1/messages` 支持路由到 Responses 后端。`wiremock` mock Responses 后端（含 encrypted_content reasoning item），
测试多轮 + tool-use，断言 reasoning signature 往返后端不报错。

### M5-RV `[TODO]` 【Review】M5 链 4 + 真实联调
确认：**Claude Code 接 Responses 后端完成带 reasoning + tool-use 的多轮对话，签名往返无 400**（PLAN M5 验收）。
核对 encrypted_content 经 signature 往返无损、富↔富流式 index 正确。记录偏差。

---

## M6 — 链 2：Anthropic → Responses（服务 Codex，富↔富）

### M6-01 `[TODO]` Anthropic 后端客户端 (`provider/anthropic_backend.rs`)
实现向真 Anthropic 后端发请求（`x-api-key` + `anthropic-version`），流式 bytes_stream。

### M6-02 `[TODO]` Anthropic 响应 → IR（thinking 侧）
扩展 Anthropic decoder：响应 `thinking` block（含真 signature）→ `Thinking{source:Anthropic, opaque:signature, echo_policy:Always}`。
复用 `stream/anthropic_decoder.rs`（Anthropic SSE → IR event，若 M2/M5 未建则此处建）。

### M6-03 `[TODO]` IR Thinking → Responses reasoning item（envelope 编码）
在 Responses encoder：`Thinking{source:Anthropic}` → reasoning item，
`encrypted_content = envelope.wrap`（含 thinking+真 signature），伪装成合法结构（M4-02）。

### M6-04 `[TODO]` 反向还原：Codex 带回的 reasoning item → Anthropic thinking block
Responses decoder + Anthropic encoder 路径：Codex 回传 reasoning item 的 encrypted_content
→ `envelope.unwrap` → 还原**带原始签名**的 thinking block，发回 Anthropic 后端（后端验自己的签名，DESIGN §4.3 链2）。

### M6-05 `[TODO]` 富↔富流式 (Anthropic SSE ↔ Responses SSE)
Anthropic SSE → IR event → Responses SSE，index/类型对齐。

### M6-06 `[TODO]` 可选：Anthropic 后端 cache_control 注入
纯函数从消息结构算 cache 断点，注入 Anthropic 请求省钱（DESIGN §3.1）。无状态。可作为可配置开关。

### M6-07 `[TODO]` 装配链 2 + 集成测试
`/v1/responses` 支持路由到 Anthropic 后端。`wiremock` mock Anthropic（含 thinking+signature），
测试 Codex 多轮 + tool-use，断言 reasoning 往返后端验签通过。

### M6-RV `[TODO]` 【Review】M6 链 2 + 全链路
确认：**Codex 接 Anthropic 后端完成带 reasoning + tool-use 的多轮对话**（PLAN M6 验收）。
确认 **4 条链全部可用**。核对 signature 经 encrypted_content 往返无损。记录偏差。

---

## M7 — 加固与运维

### M7-01 `[TODO]` 配置系统 (`config.rs`)
实现配置加载（文件 TOML/YAML + 环境变量覆盖）：后端列表（类型/base_url/凭据/profile）、
模型别名映射（client model 名 → 后端 + 改名，DESIGN §6.6）、监听地址、开关（cache 注入、reasoning store）。
用 `serde` 反序列化为强类型 `Config`。启动时校验。

### M7-02 `[TODO]` 模型路由 (`provider/router.rs`)
根据请求的 model 名 + 端点类型，用配置选择后端与 profile，并改写发往后端的 model 名。
无匹配时返回清晰错误。

### M7-03 `[TODO]` 错误映射完善 (`error.rs`)
各协议错误 JSON 结构、错误类型分类、状态码、`Retry-After`/限流头翻译（DESIGN §6.6）。
后端 4xx/5xx 映射为对应前端协议的错误格式（Anthropic error / Responses error 结构不同）。

### M7-04 `[TODO]` 不支持特性表 (`protocol/capability.rs`)
集中管理每个 `IR→协议` 方向的特性支持决策：drop / emulate / 400（DESIGN §6.5）。
例如：Responses json_schema → Anthropic 用 tool 模拟；不支持的参数明确 drop 或拒绝。

### M7-05 `[TODO]` Observability
`tracing` 结构化日志：每请求记录链路、后端、耗时、token 用量。加可选的请求/响应 dump（调试开关，脱敏凭据）。

### M7-06 `[TODO]` 限流与重试
对后端请求的重试与指数退避（尊重 `Retry-After`）。可配置并发/超时。

### M7-07 `[TODO]` 端到端回归测试套件
4 条链各录制若干真实会话（文本/reasoning/tool-use/多轮），做快照回归。整理成 `cargo test` 可跑的套件。

### M7-08 `[TODO]` README 与部署文档
写 `README.md`：配置示例、如何把 Claude Code / Codex 指向本网关、支持的后端与 profile、已知限制。

### M7-RV `[TODO]` 【Review】M7 加固 + 项目验收
确认：多后端配置化可用、错误可读、有基本可观测性、回归套件通过。
最终核对整个项目未偏离 DESIGN.md 的无状态铁律与保真目标。列出所有遗留 `[BLOCKED]` 项与后续建议。
