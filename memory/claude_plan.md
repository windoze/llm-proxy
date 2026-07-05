# Execution Plan

Current task: `[DONE] M2-08 装配链 3 端到端路由`.

1. Confirm whether the latest commit mentions unfinished work directly relevant to `M2-08`.
2. Inspect the current router, config, provider profiles, Anthropic decoder/encoder, Chat encoder/stream decoder, SSE helpers, and existing route tests.
3. Add `POST /v1/messages` so Anthropic requests are decoded to IR, encoded as Chat/DeepSeek requests, sent to the configured Chat-compatible upstream, and returned as Anthropic non-streaming or streaming responses.
4. Implement required `x-api-key` to upstream authorization handling, preserve required Anthropic response headers, apply a reasonable Chat-side default when Anthropic `max_tokens` is absent, and keep system prompt handling through the existing IR path.
5. Add focused unit tests for non-streaming route behavior, streaming SSE conversion, auth/header translation, and default handling.
6. Run formatting, linting, and tests in order; address any observed unscheduled failures.
7. Mark `M2-08` `[DONE]` in `TODO.md` with a completion record.
8. Commit all task-related changes with the required co-author trailer and stop.

Status: Completed `POST /v1/messages` for chain 3, including Anthropic request decoding, default `max_tokens`, DeepSeek Chat request encoding, upstream authorization handling, non-streaming response conversion, streaming Chat SSE to Anthropic SSE conversion, and focused route tests. `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets` passed.
