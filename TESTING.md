# 测试说明

> 本文档说明如何测试 llm-proxy，重点是**真实世界联调**——用真实后端凭据把
> Claude Code / Codex 接到本网关，验证 [DESIGN.md](./DESIGN.md) 的 4 条转换链。
> 单元 / 集成测试见各里程碑任务（[TODO.md](./TODO.md)）。

---

## 1. 测试分层

| 层级 | 工具 | 需要真实后端？ | 何时跑 |
|---|---|---|---|
| 单元测试 | `cargo test` | 否 | 每个任务完成时（`param_blocklist`、decode/encode、envelope round-trip 等） |
| 集成测试 | `cargo test` + `wiremock` | 否（mock 后端） | 每条链装配完成时（M2-09 / M3-07 …） |
| **端到端测试（真实工具驱动）** | 真实 `codex` / `claude` CLI + 真实后端 | **是**（`#[ignore]` 默认跳过） | 里程碑 review（M2-RV / M3-RV / M5-RV / M6-RV） |
| **手动真实世界联调** | 交互式 Claude Code / Codex + 真实后端 | **是** | 需要人工观察时 |

前两层不依赖网络，任何环境都能跑（这也是 CI 跑的范围）：

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --all-targets
```

后两层（端到端 / 手动联调）需要下面的真实后端凭据。

---

## 2. 真实后端凭据（环境变量）

项目根目录的 `.envrc` 里预置了一组**真实后端凭据**，供 coding agent 做真实世界
测试。`.envrc` 已列入 [.gitignore](./.gitignore)，**不会入库**。

> ⚠️ **安全铁律**：真实密钥 / 内部端点 URL **只存在于 `.envrc`**，
> 禁止写进任何会提交的文件（源码、文档、测试快照、日志）。本文档只引用变量名。
> 日志 / dump 里凭据必须脱敏（见 [TODO.md](./TODO.md) M7-05）。

加载方式（任选其一）：

```bash
# 若装了 direnv：进入目录自动加载
direnv allow

# 否则手动 source
set -a && source .envrc && set +a
```

### 变量清单

这三组凭据恰好对应 DESIGN 里代理要**调用的三类真实后端**：

| 环境变量 | 用途 | 对应后端 | 服务的链 |
|---|---|---|---|
| `DEEPSEEK_API_KEY` | DeepSeek Chat API 凭据（base URL 已在 `provider::deepseek` 硬编码为 `https://api.deepseek.com`） | DeepSeek（Chat 兼容，贫 API） | 链 1、链 3 |
| `OPENAI_API_ENDPOINT` | OpenAI **Responses** 协议后端端点（`/openai/responses`） | Responses（富 API） | 链 4 |
| `OPENAI_API_KEY` | 上述 Responses 后端的凭据 | 同上 | 同上 |
| `ANTHROPIC_BASE_URL` | Anthropic 协议后端端点（`/anthropic`） | Anthropic（富 API） | 链 2、链 6 |
| `ANTHROPIC_AUTH_TOKEN` | 上述 Anthropic 后端的凭据 | 同上 | 同上 |
| `ANTHROPIC_DEFAULT_OPUS_MODEL` | 该后端上默认使用的模型名 | 同上 | 同上 |

> 说明：`ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` 也正是 **Claude Code 客户端**
> 读取的标准变量。做链 2/6 联调时它们指向真实 Anthropic 后端；把 Claude Code 指向
> **本网关**时（链 3/4）则要把这两个变量改指向网关地址（见 §4）。别把两种用途搞混。
>
> 后端凭据在网关里的正式装配（配置文件 + 环境变量覆盖、模型别名路由）属于 M7-01 /
> M7-02。在那之前做真实联调，可临时从这些环境变量读取。

---

## 3. 启动网关

```bash
# 监听地址默认 127.0.0.1:8080，可用 LLM_PROXY_ADDR 覆盖
# 打开详细日志便于观察转换过程
RUST_LOG=llm_proxy=debug,info cargo run
```

健康检查：

```bash
curl -s http://127.0.0.1:8080/health   # => {"status":"ok"}
```

---

## 4. 各链真实世界联调

下表给出每条链联调时"前端客户端 → 本网关 → 后端"的接法。**验收标准统一是：
完成一次带工具调用的多轮对话，且 reasoning 正确往返**（见各 `-RV` 任务）。

| 链 | 前端客户端 | 网关端点 | 后端 | 用到的凭据 |
|---|---|---|---|---|
| 链 3 | Claude Code | `POST /v1/messages` | DeepSeek | `DEEPSEEK_API_KEY` |
| 链 1 | Codex | `POST /v1/responses` | DeepSeek | `DEEPSEEK_API_KEY` |
| 链 4 | Claude Code | `POST /v1/messages` | Responses | `OPENAI_API_ENDPOINT` / `OPENAI_API_KEY` |
| 链 2 | Codex | `POST /v1/responses` | Anthropic | `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_DEFAULT_OPUS_MODEL` |

### 4.1 把 Claude Code 指向本网关（链 3 / 链 4）

Claude Code 通过标准变量决定发往哪里。**联调时覆盖为网关地址**：

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:8080"
export ANTHROPIC_AUTH_TOKEN="<任意占位，网关侧鉴权翻译见 M2-08>"
# 然后在另一个 shell 里正常启动 claude
```

> 注意：这会**覆盖** `.envrc` 里指向真实 Anthropic 后端的同名变量。若同时需要
> 链 2/6（网关去调真实 Anthropic 后端），应把后端凭据放到网关自己的配置里，
> 避免与 Claude Code 客户端变量冲突。

### 4.2 把 Codex 指向本网关（链 1 / 链 2）

把 Codex 的 Responses base URL 指向网关的 `/v1/responses`，API key 用占位值。
具体配置项以 Codex 版本为准。

### 4.3 临时观察后端字节流（M0 passthrough）

M0 的 `POST /passthrough` 可把请求原样转发到 `LLM_PROXY_UPSTREAM_URL`，
用于抓取 / 比对真实后端的 SSE 字节（例如 M3-06 抓 Codex 真实 payload）：

```bash
export LLM_PROXY_UPSTREAM_URL="<后端端点>"
curl -N -X POST http://127.0.0.1:8080/passthrough \
  -H 'content-type: application/json' \
  -d '<请求体>'
```

---

## 5. 用真实工具驱动的端到端测试（`codex` / `claude`）

本项目的目标就是让 **Codex** 和 **Claude Code** 用上其他后端，因此测试必须包含
**用这两个真实 CLI 驱动**的端到端验证，确保这两个工具确实能跑通——mock 测试无法
替代这一层。

### 5.1 前置：两个 CLI 已在 PATH 中

测试机上 `codex` 与 `claude` 已安装在 `PATH`，测试程序可直接调用：

```bash
command -v codex   # codex-cli
command -v claude  # Claude Code CLI
```

### 5.2 用临时配置隔离，**绝不改全局配置** 🔒

端到端测试**必须为每个 CLI 生成临时配置目录 / 文件**，通过环境变量让 CLI 只读临时
配置，跑完清理。**禁止修改用户全局配置**（如 `~/.codex/`、`~/.claude/`、`~/.claude.json`
及全局 `settings.json`）——测试不能污染开发者 / CI 机器的真实设置。

做法：用 `tempfile` 建临时目录，写入最小配置（把 base URL 指向本网关、放占位/真实凭据），
再用各 CLI 的"配置目录"环境变量指过去：

- **Codex**：`CODEX_HOME` 指向临时目录（Codex 从 `$CODEX_HOME/config.toml` 读配置、
  auth 也在此目录），把 Responses base URL 指向本网关。还可加 `--ignore-user-config`
  彻底不读用户配置，或用 `-c key=value` 单项覆盖，进一步隔离。
- **Claude Code**：`CLAUDE_CONFIG_DIR` 指向临时目录，或用 `--settings <临时文件>` 加载
  独立设置；`ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` 指向本网关。

> 具体的配置目录变量名 / 配置字段以各 CLI 当前版本为准（`codex` 0.142.x、
> `claude` 2.1.x）；测试代码应先探测、失败时给出清晰跳过原因，而不是回退去写全局配置。

### 5.3 非交互（headless）调用

在临时配置 + 指向网关的前提下 spawn CLI 并断言输出：

```bash
# Claude Code：独立配置目录 + 指向本网关，-p 跑单次 prompt
CLAUDE_CONFIG_DIR="$TMPDIR/cc" \
ANTHROPIC_BASE_URL="http://127.0.0.1:8080" ANTHROPIC_AUTH_TOKEN="<占位>" \
  claude -p '用一句话回答：2+2 等于几？'

# Codex：独立 CODEX_HOME + Responses base URL 指向本网关，exec 非交互执行
CODEX_HOME="$TMPDIR/codex" \
  codex exec '用一句话回答：2+2 等于几？'
```

测试骨架（Rust，示意隔离配置的生命周期）：

```rust
// 1) tempdir 建临时配置目录，写入最小 config（base URL 指向网关）
// 2) Command::new("claude"/"codex").env("CLAUDE_CONFIG_DIR"/"CODEX_HOME", tmp)
//        .env("ANTHROPIC_BASE_URL", gateway_url) ...
// 3) 断言输出/退出码；tempdir Drop 时自动清理，全局配置不受影响
```

### 5.4 这些测试默认 `#[ignore]`

用真实 CLI + 真实后端的端到端测试**必须标注 `#[ignore]`**，`cargo test` 默认
**不会**运行它们。理由：

- 依赖真实网络、真实凭据（`.envrc`）、本机装有 `codex` / `claude`——
  **GitHub CI 上都不具备**，跑了必然失败或泄露凭据。
- 耗时、不确定（真实模型输出非固定）。

约定写法：

```rust
/// 端到端：真实 `claude` CLI → 本网关 → DeepSeek 后端（链 3）。
/// 需要 .envrc 凭据 + PATH 中的 claude；默认忽略，勿在 CI 运行。
#[test]
#[ignore = "e2e: requires real codex/claude CLI, network, and .envrc credentials; run manually"]
fn claude_code_over_deepseek_end_to_end() {
    // 启动网关（或连已跑的实例）→ spawn `claude -p ...` → 断言完成一次带工具调用的多轮对话
}
```

本地手动运行（需先加载 `.envrc`、启动网关）：

```bash
set -a && source .envrc && set +a
cargo test -- --ignored                       # 跑全部被忽略的 e2e 测试
cargo test claude_code_over_deepseek -- --ignored --nocapture   # 跑单个并看输出
```

> 这些 e2e 测试对应 `-RV` 里程碑的"真实客户端联调"验收（M2-RV / M3-RV /
> M5-RV / M6-RV）；交互式手动联调的接法见 §4。CI 只跑 §1 的非忽略测试，
> 见 `.github/workflows/`（TODO M7-09）。

---

## 6. 联调检查清单

对照 `-RV` 任务逐项确认：

- [ ] 流式 block 的 index / start / stop 序列正确（DESIGN §6.1）。
- [ ] tool-call 流式重组无碎片错误，`tool_call_id` ↔ `tool_use_id` ↔ `call_id` 配对不错位（DESIGN §6.2）。
- [ ] reasoning 正确呈现且多轮往返不报 400（DESIGN §4.5，链 2/4 的 signature / encrypted_content）。
- [ ] usage / stop_reason 映射正确（DESIGN §6.6）。
- [ ] 日志 / dump 中凭据已脱敏。
- [ ] 未引入会话状态存储（无状态铁律，DESIGN §2.2）。
