# LLM Proxy 设计文档

> 一个通用 LLM API 网关，让 **Claude Code**（Anthropic 协议）和 **Codex**（OpenAI Responses 协议）
> 能够接入其他模型供应商的后端。

本文档记录经过多轮联网核对后确定的设计决策与技术约束，作为实现依据。
文中标注 ⚠️ 的是踩坑点，标注 🔒 的是需要用测试锁死的行为。

---

## 1. 项目范围（已缩减）

出发点：让 Claude Code 和 Codex 用上其他供应商的模型。因此**只做两个"前端"协议**：

- **Anthropic**（对外伪装成 Anthropic server，服务 Claude Code）
- **OpenAI Responses**（对外伪装成 Responses server，服务 Codex）

后端（我们实际去调的）：OpenAI Chat、DeepSeek（≈ Chat + `reasoning_content`）、Anthropic、Responses。

### 明确不做

- **通用 N×N 互转**：只做下面 4 条链。
- **Ollama**：其原生支持 Anthropic / OpenAI Responses 格式，无需代理转换。
- **DeepSeek → Anthropic**：DeepSeek 原生支持 Anthropic 格式（`/anthropic` 端点），
  可直通。但若为统一 reasoning/usage 处理，仍可选择走我们自己的转换链（见 §5）。

### 转换矩阵（4 条链）

| # | 后端 → 前端 | 服务对象 | 难度 | 备注 |
|---|---|---|---|---|
| 1 | OpenAI Chat / DeepSeek → Responses | Codex | 中 | 贫→富，需合成流式事件 |
| 2 | Anthropic → Responses | Codex | **高** | reasoning 双向保真（signature 穿过 encrypted_content） |
| 3 | OpenAI Chat / DeepSeek → Anthropic | Claude Code | 中 | 贫→富 |
| 4 | Responses → Anthropic | Claude Code | **高** | reasoning 双向保真（encrypted_content 穿过 signature） |

链 2 与链 4 互为逆向；二者都是富 API、块结构、带 reasoning——这是仍然值得做统一 IR 的理由。

### 实施优先级

1. **先做链 1、3**（Chat/DeepSeek → 前端）：立即让 Claude Code / Codex 接上 DeepSeek 这类
   Chat 后端，价值最高，reasoning 复杂度最低（DeepSeek 的 `reasoning_content` 是纯文本无签名）。
2. **后做链 2、4**（Anthropic ↔ Responses）：不透明推理令牌往返是硬骨头。

---

## 2. 核心架构：IR + 无状态

### 2.1 规范中间表示（Canonical IR）

每个协议只实现 `协议 → IR`（decoder）和 `IR → 协议`（encoder），不做协议间直连。

- IR 应为 **Anthropic + Responses 语义的并集**（二者表达力最强）。
- OpenAI Chat / DeepSeek 只是 IR 的**有损投影**。
- 骨架采用 Anthropic 的内容块模型：`text` / `image` / `tool_use` / `tool_result` / `thinking`。

### 2.2 网关是无状态的 ✅

**结论：不需要会话状态存储（无 DB / Redis）。** 依据（见 §7 核对记录）：

- **Codex 无状态发送**：每轮通过 `for_prompt()` 重建并发送**完整历史**，把 reasoning item
  （含 `encrypted_content`）自己带在 `input` 里回传，**不依赖 `previous_response_id`**。
- **Claude Code 无状态**：Anthropic 协议本身无 `previous_message_id` 概念，每轮必发全量历史。

唯一会逼出状态存储的场景是"前端用 `previous_response_id` 让服务端记状态"——Codex 不这么做，
故不触发。**若将来出现该场景，把状态存储做成可选后端（内存/Redis），只在检测到
`previous_response_id` 时启用，不污染无状态主路径。**

---

## 3. 三个"缓存/状态"概念的辨析

这三者常被混为一谈，实际只有一个逼出状态，且那个我们不做：

| 概念 | 是什么 | key 由谁持有 | 逼我们做状态？ | 我们的处理 |
|---|---|---|---|---|
| **Prompt caching**（`cache_control` / 自动前缀缓存 / DeepSeek ctx cache） | 成本优化，key = token 前缀 hash | **provider 端** | 否 | 见 §3.1 |
| **不透明推理往返**（signature / `encrypted_content`） | 上轮推理产物，客户端下轮原样带回 | **客户端带着走** | 否（client-as-storage） | 见 §4 |
| **会话状态**（Responses `store=true` + `previous_response_id`） | 服务端真的存历史 | **服务端** | 是 | 不触发（Codex 不用） |

### 3.1 Prompt caching：不模拟、不产生状态

- 客户端从不"持有 cache key 单独复用"——每次发全量内容 + 断点，provider 自己 hash。**没有 key 要模拟。**
- 后端是 Chat/DeepSeek（自动前缀缓存）：**直接丢弃** 客户端传来的 `cache_control` marker。
- 后端是 Anthropic：可**自己算断点注入**省钱，但这是从消息结构算出的纯函数，无状态。

---

## 4. Reasoning 保真（本项目 90% 的难度）

服务对象 Claude Code / Codex 都是**重度 reasoning + 重度 tool-use** 的编码 agent。
普通聊天代理里 reasoning 可丢，这里**丢了就崩**。

### 4.1 三家 reasoning 的回传语义各不相同

| Provider | 载荷 | 回传规则 |
|---|---|---|
| **Anthropic** | `thinking` block 带 **signature** | 多轮必须原样带回，后端校验签名 |
| **Responses** | reasoning item（id `rs_...`）带 **`encrypted_content`** | 无状态模式下需原样放回 `input` |
| **DeepSeek** | `reasoning_content`（纯文本，**无签名**） | **条件性**：见下 |

**DeepSeek 条件性规则**（⚠️ 上轮曾讲反，已纠正）：

- 普通多轮（**无** tool_calls）：上轮 `reasoning_content` **可省略**。
- 有 tool_calls 的轮次：`reasoning_content` **必须完整回传**，否则 **400**。

### 4.2 IR 的 thinking block 建模

```rust
enum EchoPolicy { Always, OnlyWithToolCall, Never }

struct Thinking {
    text: Option<String>,      // 可读思考文本（可能没有）
    opaque: Option<Vec<u8>>,   // signature / encrypted_content / 原始载荷
    source: Provider,          // anthropic | responses | deepseek
    echo_policy: EchoPolicy,   // 决定多轮转发时去留
}
```

### 4.3 client-as-storage：无状态搬运不透明令牌 ✅

每条链里，真假两端恰好一边是真后端、另一边由我们完全掌控。把要记住的东西**编码进对端那个
"反正要原样带回"的不透明字段**里，即可无状态搬运。

- **链 4（Responses 后端 → Anthropic 前端）**：Claude Code 把我们当 Anthropic server，
  **signature 由我们自己签自己验**。令 `signature = envelope(后端原始 encrypted_content)`，
  Claude Code 下轮原样带回，我们解出还原成 Responses reasoning item 发回后端。
- **链 2（Anthropic 后端 → Responses 前端）**：Codex 把我们当 Responses server，
  **`encrypted_content` 由我们生成**。令 `encrypted_content = envelope(后端真实 thinking block 含真 signature)`，
  Codex 带回，我们还原出**带原始签名的** thinking block 发回真 Anthropic 后端，后端验签通过。

优点：无状态、可水平扩展、崩溃无状态丢失。

### 4.4 Envelope 设计约束 ⚠️

- **`encrypted_content` 是 provider 私钥加密、不可跨 provider/组织解密**（核对确认，见 §7）。
  因此链 2 里我们**必须自造 envelope**，不能透传真 OpenAI 的格式。
- Envelope 应输出 Responses-compatible reasoning item 结构：带 `type: reasoning`，
  我们的载荷放进 `encrypted_content`（base64）。真实 Responses 后端/上游会校验此字段
  （`invalid_encrypted_content` 错误可证），但 Codex 客户端本身只把
  `encrypted_content` 当不透明字符串搬运（见 §7 实测记录）。
- Envelope 内含：版本号 + 完整性校验 + 后端来源标记（source provider）。
- **风险**：不透明字段可能有长度上限（`encrypted_content` 可能很大；Anthropic `signature`
  是否限长未知）。若撑爆，退化为有状态方案（`id → 原始 block` 映射）。🔒
- **Codex 客户端实测**：0.142.5 在下轮请求中不回传 reasoning item 的 `id` / `status`，
  但会逐字节回传 `encrypted_content`；非 `rs_` id、非 base64 内容、256 KiB 级载荷均未触发客户端校验。

### 4.5 保真坑 🔒

多个库（liteLLM PR #17130、langchainjs #10844）踩过：转换 reasoning item 时误删
`encrypted_content`，或保留了 API 拒绝的 `status=null` → 400 "encrypted content could not be verified"。
→ reasoning item 转换必须**逐字段保真**，专门写测试覆盖 `encrypted_content` / `status`。

---

## 5. DeepSeek Capability Profile

DeepSeek 是"OpenAI 兼容但有脾气"的后端。把"OpenAI 兼容后端"抽象成**带 capability profile 的
provider**（将来接 Groq / Together / 月之暗面等只需加 profile，不改核心）。

经核对（V3.1+，见 §7）DeepSeek 现状：

```
profile: deepseek
  # 混合推理模型：同底座两种模式，均 128K context
  models:
    deepseek-chat      -> thinking: off
    deepseek-reasoner  -> thinking: on
    # 文档已出现 deepseek-v4-pro

  # 设了不报错但无效，转发时静默 drop
  param_blocklist: [temperature, top_p, presence_penalty,
                    frequency_penalty, logprobs, top_logprobs]

  # reasoning_effort 归一
  reasoning_effort: { low->high, medium->high, xhigh->max, default: high }
  # Anthropic 格式等价：output_config: { effort: ... }

  # thinking 开关：reasoning_effort，或 extra_body={"thinking":{"type":"enabled/disabled"}}

  reasoning_content:
    - 映射到 IR thinking block（source=deepseek, 无 signature）
    - echo_policy:
        OnlyWithToolCall   # 有 tool_calls 的轮次必须回传（否则 400）
                           # 无 tool_calls 时可 drop

  features:
    function_calling: yes   # 思考模式下也支持；strict 在 Beta API
    json_output:      yes

  usage_extra: [prompt_cache_hit_tokens, prompt_cache_miss_tokens]
  max_output: 32K default / 64K max   # 含 CoT；CoT 不计入 context
  n>1: 不支持
  native_anthropic_endpoint: yes      # /anthropic
```

### ⚠️ 官方文档版本不一致

- `guides/reasoning_model` 页（R1 时代旧页）仍写 Function Calling "Not Supported"、context 64K；
- `guides/thinking_mode` + `news/news250821`（V3.1+ 新页）：FC 支持、思考模式下支持 FC、128K context。

**以新页面为准**（`thinking_mode`）。写死进代码时在注释标注此不一致。第三方页面（apidog 等）更旧，勿信。

---

## 6. 其余映射难点（缩减后仍需处理）

### 6.1 流式状态机（代码量最大）

统一为 `后端字节流 → IR event 流 → 前端字节流`，encoder/decoder 只跟 IR event 打交道。

各协议流格式：

| 协议 | 传输 | 结构 |
|---|---|---|
| OpenAI Chat | SSE `data:` | `choices[].delta`，`[DONE]` 结尾 |
| Anthropic | SSE 带类型事件 | `message_start` / `content_block_start`/`_delta`/`_stop` / `message_delta` / `message_stop`，按 block index |
| OpenAI Responses | SSE 带类型事件 | `response.output_text.delta` / `response.function_call_arguments.delta` 等 |

贫→富（Chat → 任意前端）需状态机：推断内容块边界、补发 `content_block_start/stop`、维护 index。

### 6.2 Tool-call 流式重组（编码 agent 的生命线）🔒

- OpenAI Chat 后端按 tool index 吐 `arguments` JSON 字符串碎片，**无显式边界**；
  需合成 Responses 的 `response.function_call_arguments.delta`/`.done`，
  或 Anthropic 的 `input_json_delta`（`partial_json`）+ `content_block_start/stop`。
- **ID 映射**：`tool_call_id` ↔ `tool_use_id` ↔ Responses `call_id`，以及
  "assistant 发起调用" 与 "下轮带回的 tool_result / function_call_output" 的配对链。
  **ID 错位 = agent 循环断裂。**

### 6.3 工具结果挂载方式差异

- OpenAI Chat：assistant 消息带 `tool_calls`，结果是独立 `role:tool` 消息 + `tool_call_id`。
- Anthropic：assistant 的 `tool_use` block，结果放在 **user 消息里** 的 `tool_result` block + `tool_use_id`。
- Responses：`function_call` / `function_call_output` item。

### 6.4 消息结构与顺序

- **system prompt**：Anthropic 是顶层独立参数；OpenAI/Responses 是 `role:system/developer` 消息。需 hoist/inject。
- **DeepSeek 严格交替**：要求 user/assistant 交替，从 Anthropic 转来时若出现连续 user（tool_result 拆成独立 user）可能需合并。

### 6.5 参数映射（有损）

- `max_tokens`：Anthropic **必填**；Responses 用 `max_output_tokens`。转 Anthropic 时给合理默认。
- 值域：Anthropic `temperature` 0–1，OpenAI 0–2。
- 结构化输出：Responses 有 json_schema；Anthropic 无原生等价，需用 tool 模拟。
- `tool_choice`：Anthropic `any` ≈ OpenAI `required`。
- 每个 `IR → 协议` 方向维护一张"不支持特性"表，决定 **drop / emulate / 400 拒绝**。

### 6.6 边角映射

- **finish/stop reason**：`stop/length/tool_calls` ↔ `end_turn/max_tokens/stop_sequence/tool_use`。
- **usage**：`prompt/completion_tokens` ↔ `input/output_tokens`（+ Anthropic cache token、DeepSeek cache hit/miss）。
- **鉴权头**：`x-api-key` + `anthropic-version` ↔ `Authorization: Bearer`。
- **错误格式**：错误 JSON 结构、类型分类、`Retry-After`/限流头各异。
- **模型名路由**：client 的 model 名 → 后端选择 + 改名，需别名配置。

---

## 7. 联网核对记录

以下结论经联网核对官方文档（核对日期：2026-07）。

### DeepSeek

- `deepseek-reasoner` = 思考模式，`deepseek-chat` = 非思考模式，同底座，**128K context**（V3.1，news250821）。
- 不支持参数（设了不报错但无效）：`temperature`/`top_p`/`presence_penalty`/`frequency_penalty`；
  `logprobs`/`top_logprobs` 不支持。（reasoning_model 页）
- **Function Calling / JSON Output 现已支持，思考模式下也支持 FC**；Strict FC 在 Beta。（thinking_mode 页、news250821）
- reasoning_content 回传：无 tool call 可省略；**有 tool call 必须完整回传否则 400**。（thinking_mode 页原文）
- reasoning_effort：`low`/`medium`→`high`，`xhigh`→`max`，默认 `high`。
- 最大输出（含 CoT）默认 32K / 最大 64K。
- 原生支持 Anthropic API 格式（`/guides/anthropic_api`）。

### Codex / Responses

- **Codex 每轮发全量历史**，`for_prompt()` 返回所有 item（含带 `encrypted_content` 的 Reasoning），
  不依赖 `previous_response_id`。（openai/codex issue #17541，2026-04）
- 无状态模式（`store=false` 或 ZDR）保留 reasoning：请求带 `include:["reasoning.encrypted_content"]`，
  响应 reasoning item（id `rs_...`）带 `encrypted_content`，下轮原样放回 `input`。（OpenAI reasoning 指南 2026-04-30）
- 本地假 Responses 端点 + 真实 Codex CLI 0.142.5 实测（2026-07-06）：Codex 客户端不会在发回
  reasoning item 前校验 `encrypted_content` 格式；非 base64 字符串会原样进入下一轮请求。响应侧非
  `rs_` id 也不会被客户端拒绝，且下一轮请求不携带 reasoning `id` / `status`，只携带
  `type:"reasoning"`、`summary` 与 `encrypted_content`。已验证 256 KiB 级 `encrypted_content` 原样回传。
- **`encrypted_content` 是 provider/组织私钥加密，不可跨 provider 解密**；跨 provider 透传会
  `invalid_encrypted_content`。（issue #17541、liteLLM 事故报告）
- 已知保真坑：转换 reasoning item 时误删 `encrypted_content` 或保留 `status=null` → 400。
  （liteLLM PR #17130、langchainjs #10844）

### 仍未完全钉死

- 不透明字段（`signature` / `encrypted_content`）的绝对长度上限；Codex 客户端已实测可原样回传
  256 KiB 级 `encrypted_content`，但上限仍需在 M4 长度保护任务中防御性处理。

---

## 8. 技术选型（Rust）

- HTTP 骨架：`axum` + `tower`
- 上游请求 + 流式：`reqwest`（`bytes_stream`）
- SSE 解析：`eventsource-stream`
- 多态 content block：`serde` 的 `#[serde(tag=...)]` / `#[serde(untagged)]`
- 每个 `IR → 协议` encoder 对 reasoning 保真单独写测试（§4.5）
