# Execution Plan

I cannot include private chain-of-thought, but I will keep this file updated with the concrete plan, decisions, and progress for the current invocation.

Current task: `M2-01` Anthropic request parsing in `protocol/anthropic/decode.rs`.

1. [DONE] Read the existing IR definitions and OpenAI Chat decoder to match established data shapes, validation style, and tests.
2. [DONE] Add an Anthropic decoder module that implements `anthropic_request_to_ir(body: &Value) -> Result<IrRequest>`.
3. [DONE] Cover top-level `system`, message content blocks (`text`, `image`, `tool_use`, `tool_result`, `thinking`), tools, tool choice, max tokens, stream, stop, and provider-specific extras as required by `TODO.md`.
4. [DONE] Add focused unit tests for string/block-array system prompts, tool use/result mapping, tool choice mapping, and Anthropic thinking signatures.
5. [DONE] Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. [DONE] Mark `M2-01` `[DONE]` in `TODO.md` and update its completion record.
7. [DONE] Commit the task changes and stop.
