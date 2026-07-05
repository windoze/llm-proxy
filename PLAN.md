# LLM Proxy 落地计划

> 本文档是 [DESIGN.md](./DESIGN.md) 的实施计划。按里程碑推进，每个里程碑结束都有可验证的产出。
> 原则：**先无状态、先贫→富链（1/3）、reasoning 保真单独测试**。

---

## 总体推进策略

```
M0 骨架    →  M1 IR + Chat 解析  →  M2 链3(Chat→Anthropic)  →  M3 链1(Chat→Responses)
                                          ↓ 服务 Claude Code        ↓ 服务 Codex
                                     可用里程碑①                可用里程碑②
M4 reasoning 保真机制  →  M5 链4(Responses→Anthropic)  →  M6 链2(Anthropic→Responses)
                                          ↓                          ↓
                                     富↔富，硬骨头            全部 4 条链打通
M7 加固（错误/鉴权/限流/配置/observability）
```

关键顺序理由：
- 链 1、3 的后端是 Chat/DeepSeek，reasoning 是纯文本无签名，**复杂度最低、价值最高**（先让两个 agent 接上 DeepSeek）。
- 链 2、4 涉及不透明令牌往返（client-as-storage + envelope），是硬骨头，放后面。
- reasoning 保真机制（M4）在做链 4 之前统一建好，供 2/4 共用。

---

## M0 — 项目骨架

**目标**：一个能跑起来、能把请求原样透传（passthrough）的 axum 服务。

- [ ] 引入依赖：`axum`、`tokio`、`tower`、`reqwest`（`stream` feature）、`serde`/`serde_json`、
      `eventsource-stream`、`bytes`、`tracing`、`thiserror`、`anyhow`。
- [ ] 目录结构：
  ```
  src/
    main.rs            # 启动 axum，加载 config
    config.rs          # 后端路由/别名/鉴权配置（见 M7）
    ir/                # 规范中间表示
      mod.rs
      message.rs       # Message / ContentBlock / Thinking / ToolUse / ToolResult
      request.rs       # 统一请求参数
      event.rs         # 流式 IR event
    protocol/
      anthropic/       # decoder(→IR) + encoder(IR→) + streaming
      responses/
      openai_chat/     # 含 DeepSeek profile
    provider/          # 后端 HTTP 客户端 + capability profile
    stream/            # 通用 SSE/NDJSON 解析与状态机基础设施
    error.rs
  ```
- [ ] 一个 `/health` 端点 + 一条透传路由，验证 axum + reqwest 流式打通。

**验收**：`cargo run` 起服务，透传一个真实后端的流式响应，字节无损。

---

## M1 — IR 数据结构 + OpenAI Chat 解析

**目标**：定义 IR，实现 `OpenAI Chat → IR`（decoder），含 DeepSeek profile。

- [ ] `ir::message`：`ContentBlock` 枚举（`Text` / `Image` / `ToolUse` / `ToolResult` / `Thinking`）。
      `Thinking { text, opaque, source, echo_policy }`（见 DESIGN §4.2）。
- [ ] `ir::request`：统一请求（messages、system、tools、tool_choice、max_tokens、temperature、
      stream、以及 provider 特有参数的 passthrough 袋子）。
- [ ] `ir::event`：流式 IR event（`MessageStart` / `BlockStart{index,type}` / `TextDelta` /
      `ToolUseDelta{partial_json}` / `ThinkingDelta` / `BlockStop` / `MessageDelta{stop_reason,usage}` / `MessageStop`）。
- [ ] `protocol::openai_chat::decode`：Chat 请求/响应 → IR（非流式先做）。
- [ ] `provider` 的 **capability profile** trait + `deepseek` 实现（DESIGN §5）：
  - `param_blocklist` 静默 drop
  - `reasoning_effort` 归一
  - `reasoning_content` → IR Thinking（`source=deepseek`，`echo_policy=OnlyWithToolCall`）
  - usage 额外字段、`n>1` 拒绝
- [ ] 单元测试：DeepSeek 响应样本（含 `reasoning_content` + tool_calls）→ IR 正确。

**验收**：DeepSeek/Chat 的非流式响应能正确解析成 IR，profile 规则生效。

---

## M2 — 链 3：Chat/DeepSeek → Anthropic（服务 Claude Code）✅ 可用里程碑①

**目标**：Claude Code 指向本网关，用 DeepSeek 跑通一轮 + 一轮 tool-use。

- [ ] `protocol::anthropic::encode`：IR → Anthropic 响应（非流式）。
- [ ] `protocol::anthropic::decode`：Anthropic 请求 → IR（Claude Code 发来的）。
- [ ] **贫→富流式状态机**（`stream/`）：Chat SSE → IR event → Anthropic SSE。
  - 推断内容块边界，补发 `content_block_start/stop`，维护 block index。
  - **tool-call 流式重组**（DESIGN §6.2）🔒：Chat 的 `arguments` 碎片 → Anthropic `input_json_delta`。
- [ ] **ID 映射**：`tool_call_id` ↔ `tool_use_id`，配对链保真。
- [ ] 映射：system prompt hoist、`max_tokens` 默认值、stop_reason、usage、鉴权头翻译。
- [ ] DeepSeek 严格交替处理（连续 user 合并，DESIGN §6.4）。
- [ ] 集成测试：录制的 Claude Code 请求 → 本网关 → DeepSeek，比对流式输出结构。

**验收**：**真实 Claude Code 接本网关 + DeepSeek 后端，完成一次带工具调用的多轮对话。**

---

## M3 — 链 1：Chat/DeepSeek → Responses（服务 Codex）✅ 可用里程碑②

**目标**：Codex 指向本网关，用 DeepSeek 跑通一轮 + 一轮 tool-use。

- [ ] `protocol::responses::encode`：IR → Responses 响应（非流式）。
- [ ] `protocol::responses::decode`：Responses 请求 → IR（Codex 发来的，含 `input` 全量历史）。
- [ ] 贫→富流式状态机：Chat SSE → IR event → Responses SSE
      （`response.output_text.delta` / `response.function_call_arguments.delta`/`.done` 等）。
- [ ] **ID 映射**：`tool_call_id` ↔ Responses `call_id`；`function_call` / `function_call_output` 配对。
- [ ] 映射：`developer/system` 消息、`max_output_tokens`、stop_reason、usage。
- [ ] 集成测试：录制的 Codex 请求 → 本网关 → DeepSeek。

**验收**：**真实 Codex 接本网关 + DeepSeek 后端，完成一次带工具调用的多轮对话。**

> ⚠️ 本里程碑期间用假 Responses 端点抓一次 Codex 真实 payload，确认 §M4 需要的
> reasoning item 校验行为（DESIGN §4.4 未钉死项）。

---

## M4 — Reasoning 保真机制（envelope + client-as-storage）

**目标**：建立供链 2/4 共用的不透明令牌无状态搬运机制。

- [ ] `reasoning/envelope.rs`：定义 envelope 格式
  - 版本号 + 完整性校验（如 HMAC/CRC）+ 后端来源标记 + 原始载荷。
  - 提供 `wrap(source_block) -> opaque_bytes` / `unwrap(opaque_bytes) -> source_block`。
- [ ] **伪装成合法 reasoning item 结构**（DESIGN §4.4）：生成 `rs_` 风格 id、`type:reasoning`，
      载荷放 `encrypted_content`（base64）。
- [ ] Anthropic `signature` 侧对称实现：envelope 编码进 signature 字段。
- [ ] 长度上限保护 🔒：超限时降级路径（预留有状态 `id→block` 映射接口，默认关闭）。
- [ ] reasoning item 字段保真测试 🔒（DESIGN §4.5）：`encrypted_content` 不丢、`status` 正确。

**验收**：envelope round-trip 单元测试（wrap→模拟客户端带回→unwrap 还原原始 block，含 tool-use 场景）。

---

## M5 — 链 4：Responses → Anthropic（服务 Claude Code，富↔富）

**目标**：Claude Code 接本网关 + Responses 后端（如真 OpenAI o 系列 / GPT-5.1）。

- [ ] Responses 后端客户端：请求带 `include:["reasoning.encrypted_content"]`，`store=false`。
- [ ] decoder：Responses 响应 reasoning item（`encrypted_content`）→ IR Thinking（`source=responses`）。
- [ ] encoder：IR Thinking → Anthropic `thinking` block，`signature = envelope(encrypted_content)`。
- [ ] 反向：Claude Code 带回的 thinking block → unwrap → 还原 Responses reasoning item 放回 `input`。
- [ ] 富↔富流式：Responses SSE ↔ Anthropic SSE（两侧都是块结构，index/类型对齐）。
- [ ] 集成测试：Claude Code → 本网关 → Responses 后端，多轮 + tool-use，reasoning 往返不报错。

**验收**：**Claude Code 接 Responses 后端完成带 reasoning + tool-use 的多轮对话，签名往返无 400。**

---

## M6 — 链 2：Anthropic → Responses（服务 Codex，富↔富）

**目标**：Codex 接本网关 + Anthropic 后端（真 Claude 模型）。全部 4 条链打通。

- [ ] Anthropic 后端客户端。
- [ ] decoder：Anthropic 响应 `thinking` block（含真 signature）→ IR Thinking（`source=anthropic`）。
- [ ] encoder：IR Thinking → Responses reasoning item，`encrypted_content = envelope(thinking+signature)`。
- [ ] 反向：Codex 带回的 reasoning item → unwrap → 还原带原始签名的 thinking block 发回 Anthropic。
- [ ] 富↔富流式：Anthropic SSE ↔ Responses SSE。
- [ ] 可选：Anthropic 后端的 `cache_control` 断点纯函数注入（省钱，DESIGN §3.1）。
- [ ] 集成测试：Codex → 本网关 → Anthropic 后端，多轮 + tool-use。

**验收**：**Codex 接 Anthropic 后端完成带 reasoning + tool-use 的多轮对话。4 条链全部可用。**

---

## M7 — 加固与运维

**目标**：从"能跑"到"能用"。

- [ ] **配置系统**（`config.rs`）：后端路由、模型别名（client model 名 → 后端 + 改名）、
      鉴权凭据、profile 选择。支持文件 + 环境变量。
- [ ] **错误映射**：各协议错误 JSON 结构、类型分类、状态码、`Retry-After`/限流头翻译。
- [ ] **鉴权头翻译**：`x-api-key`+`anthropic-version` ↔ `Authorization: Bearer`。
- [ ] **不支持特性表**：每个 `IR→协议` 方向的 drop / emulate / 400 决策集中管理。
- [ ] **Observability**：`tracing` 结构化日志、请求耗时/token 统计、可选请求 dump（调试用）。
- [ ] **限流/重试**：对后端的重试与退避。
- [ ] 端到端回归测试套件：4 条链各录制若干真实会话做快照比对。

**验收**：配置化多后端、错误可读、有基本可观测性，通过回归套件。

---

## 横切关注点（贯穿所有里程碑）

- **测试策略**：优先"录制真实 client 请求 + 后端响应 → 快照比对"。reasoning 保真、tool-call
  流式重组、ID 映射是 🔒 必须锁死的三块。**真实世界联调**（真实 Claude Code / Codex + 真实
  后端）见 [TESTING.md](./TESTING.md)；`.envrc`（已 gitignore）预置了 DeepSeek / Responses /
  Anthropic 三组真实后端凭据，供各 `-RV` 里程碑验收使用，凭据禁止写进任何入库文件。
- **无状态铁律**：任何里程碑不得引入会话状态存储；唯一例外是 M4 预留的降级接口（默认关闭）。
- **profile 可扩展**：新增"OpenAI 兼容"后端（Groq/Together/月之暗面…）应只需加 profile，不改核心。

---

## 待确认项（阻塞相关里程碑）

| 项 | 影响里程碑 | 如何确认 |
|---|---|---|
| Codex 客户端是否校验 reasoning item 的 `encrypted_content`/`id` 格式（DESIGN §4.4） | M4/M6 | M3 期间用假 Responses 端点抓 Codex 真实 payload |
| 不透明字段（signature/encrypted_content）长度上限 | M4 | 实测超长 envelope 是否被拒 |
| DeepSeek 官方文档版本不一致（FC 支持、context 长度，DESIGN §5） | M1 | 以 `thinking_mode` 新页为准，实测校验 |
