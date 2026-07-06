# llm-proxy

[English](./README.md) | [中文](./README.zh-CN.md)

`llm-proxy` is a stateless LLM API gateway for coding agents. It exposes the two client-facing APIs used by Claude Code and Codex, then routes each request to a configured upstream backend while preserving tool calls, streaming events, usage, and reasoning payloads.

The project currently exposes:

| Client | Frontend endpoint | Compatible upstream backend |
|---|---|---|
| Claude Code | `POST /v1/messages` | OpenAI Chat / DeepSeek, or OpenAI Responses |
| Codex | `POST /v1/responses` | OpenAI Chat / DeepSeek, or Anthropic Messages |

See [`DESIGN.md`](./DESIGN.md) for the protocol mapping design and [`TESTING.md`](./TESTING.md) for local, integration, and real-client test procedures.

## Quick start

Requirements:

- Rust toolchain with edition 2024 support
- Network access and upstream credentials for any real backend you want to call

Run the default DeepSeek-compatible setup:

```bash
export DEEPSEEK_API_KEY="<your DeepSeek API key>"
cargo run
```

The server listens on `127.0.0.1:8080` by default. Override it with `LLM_PROXY_ADDR`:

```bash
LLM_PROXY_ADDR=127.0.0.1:18080 cargo run
```

Health check:

```bash
curl -s http://127.0.0.1:8080/health
```

## Configuration

`llm-proxy` loads configuration from `LLM_PROXY_CONFIG` when set. The file may be TOML, YAML, or YML. Environment variables override the file where supported.

Example `config.toml`:

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

### Backend failover

A model alias can list several backend targets. Index 0 is preferred; the rest are failover candidates. When the active backend accumulates enough failures (HTTP 429, 5xx, or connection/timeout errors) within a sliding window, the alias advances to the next target for **subsequent** requests. Switching is one-way (it never falls back to a preferred target) and is rate-limited by a minimum switch interval — even if the failure threshold is met, no switch happens until that interval has elapsed since the last switch. The request that trips the threshold still returns its error; only later requests use the new target.

```toml
[model_aliases."codex-default"]
targets = [
  { backend = "responses", model = "gpt-5.1" },
  { backend = "anthropic", model = "claude-sonnet-4-5" },
]

[model_aliases."codex-default".failover]
window_ms = 60000              # count failures over this sliding window
failure_threshold = 3          # failures within the window that satisfy the switch condition
min_switch_interval_ms = 30000 # minimum time between two consecutive switches
```

The `failover` block is optional; when omitted it defaults to `window_ms = 60000`, `failure_threshold = 3`, `min_switch_interval_ms = 30000`. The legacy single-target form (`backend = "..."` + `model = "..."`) remains supported and is equivalent to a one-entry `targets` list with no failover.

Do not commit real credentials. Keep secrets in local environment variables or an untracked local config. For the example above:

```bash
export LLM_PROXY_CONFIG="$PWD/config.toml"
export DEEPSEEK_API_KEY="<deepseek key>"
export OPENAI_API_KEY="<responses backend key>"
export ANTHROPIC_AUTH_TOKEN="<anthropic backend key>"
cargo run
```

Run client commands in a separate shell from the proxy when variable names overlap. `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` can mean "Anthropic backend credentials" to the proxy process, but "Claude Code should call this base URL" to the Claude Code process; `OPENAI_API_KEY` can likewise be a Responses backend key for the proxy or a local placeholder for Codex.

### Important environment variables

| Variable | Purpose |
|---|---|
| `LLM_PROXY_CONFIG` | Path to TOML/YAML config file |
| `LLM_PROXY_ADDR` | Listen address, default `127.0.0.1:8080` |
| `DEEPSEEK_API_KEY` | Chat/DeepSeek backend API key |
| `LLM_PROXY_CHAT_COMPLETIONS_URL` | Override Chat Completions endpoint |
| `OPENAI_API_ENDPOINT` / `OPENAI_API_KEY` | Responses-compatible backend endpoint and key |
| `ANTHROPIC_BASE_URL` / `ANTHROPIC_AUTH_TOKEN` | Anthropic-compatible backend base URL and credential |
| `ANTHROPIC_VERSION` | Anthropic backend version header, default `2023-06-01` |
| `ANTHROPIC_DEFAULT_OPUS_MODEL` | Default upstream model for Anthropic backend routes |
| `LLM_PROXY_OBSERVABILITY_DUMP` | Redacted JSON dump logging for debugging |
| `LLM_PROXY_BACKEND_MAX_RETRIES` | Retry count for retryable upstream failures |
| `LLM_PROXY_BACKEND_TIMEOUT_MS` | Per-attempt upstream timeout |
| `LLM_PROXY_BACKEND_CONCURRENCY_LIMIT` | Global upstream concurrency limit |

`LLM_PROXY_BACKENDS` and `LLM_PROXY_MODEL_ALIASES` can replace the configured backend list and model alias map with JSON values.

## Point clients at the gateway

### Claude Code

Claude Code speaks Anthropic Messages. Point it at the proxy base URL and use any local placeholder token if the proxy itself has the upstream credential configured:

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:8080"
export ANTHROPIC_AUTH_TOKEN="local-placeholder"
claude -p "Use one sentence to say hello through llm-proxy."
```

Use a model name that the proxy can route, such as `deepseek-chat` or a configured alias like `claude-code-rich`.

### Codex

Codex speaks OpenAI Responses. Configure its Responses base URL to the proxy, keep the API key as a local placeholder when upstream credentials are configured in the proxy process, and use an isolated `CODEX_HOME` for testing:

```bash
export CODEX_HOME="$(mktemp -d)"
export OPENAI_API_KEY="local-placeholder"
# Configure your Codex version's provider/base-url setting to:
#   http://127.0.0.1:8080/v1
codex exec "Use one sentence to say hello through llm-proxy."
```

For exact Codex CLI provider syntax, use your installed Codex version's help output; the proxy endpoint to target is `/v1/responses` under the `/v1` base URL.

## Supported backends and profiles

| Backend type | Profiles | Notes |
|---|---|---|
| `chat` | `deep_seek`, `generic_openai` | OpenAI Chat-compatible backends. DeepSeek applies its documented parameter blocklist, `reasoning_effort` normalization, and `n > 1` rejection. |
| `responses` | `generic_openai` | OpenAI Responses-compatible backends. Requests force `store=false` and include `reasoning.encrypted_content` when calling the backend. |
| `anthropic` | `anthropic` | Anthropic Messages-compatible backends. Supports optional stateless cache-control injection and Anthropic thinking signatures. |

Model routing uses exact `model_aliases` first. Without an alias, `deepseek-*` models route to the Chat backend; non-DeepSeek models route to the rich backend compatible with the frontend endpoint when one is configured.

## Deployment notes

Build a release binary:

```bash
cargo build --release
```

Run it with a local config and environment-held secrets:

```bash
export LLM_PROXY_CONFIG=/etc/llm-proxy/config.toml
export DEEPSEEK_API_KEY="<deepseek key>"
export OPENAI_API_KEY="<responses key>"
export ANTHROPIC_AUTH_TOKEN="<anthropic key>"
RUST_LOG=llm_proxy=info ./target/release/llm-proxy
```

If you enable `LLM_PROXY_OBSERVABILITY_DUMP`, request and response JSON is logged with credentials, `encrypted_content`, and signatures redacted. Do not enable body dumps in shared production logs unless you have reviewed the privacy impact.

## Testing

Local validation that does not require real network credentials:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all --all-targets
```

Real Claude Code / Codex end-to-end tests are intentionally `#[ignore]` because they require installed CLIs, network access, and untracked credentials. See [`TESTING.md`](./TESTING.md) for the exact real-world testing workflow.

## Known limits

- The gateway is intentionally stateless. Requests that require server-side conversation storage, such as unsupported `previous_response_id` state, are rejected rather than emulated.
- Reasoning payloads are preserved through envelopes. The optional reasoning store fallback exists for oversized envelopes but is disabled by default.
- OpenAI Chat / DeepSeek backends are a lower-fidelity target than Anthropic or Responses; unsupported features are either dropped according to profile rules or rejected with a protocol error.
- Real CLI configuration flags can change across Claude Code and Codex releases. Keep client-specific setup isolated with temporary config directories when testing.
