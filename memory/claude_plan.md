# Execution plan

I will follow `TODO.md` as the source of truth, select the first task whose heading is not prefixed with `[DONE]`, complete exactly that task, update its completion record, commit the result, and stop.

Current task: M6-04, restoring Codex-returned Responses `reasoning.encrypted_content` into Anthropic `thinking` blocks with the original signature for chain 2.

Steps:
1. Inspect the Responses decoder, Anthropic encoder, envelope helpers, and related tests/snapshots to locate the existing one-way M6-03 encoding path and the M4/M5 reverse paths to mirror.
2. Added decoding support so Responses reasoning items containing llm-proxy Anthropic envelopes unwrap to IR `Thinking{source:Anthropic, opaque:signature, echo_policy:Always}` instead of remaining opaque Responses reasoning.
3. Added an Anthropic backend request encoder path that emits restored Anthropic-origin IR thinking blocks as `thinking` content blocks with the original signature, and rejects non-Anthropic thinking for real Anthropic backend calls.
4. Added focused unit tests covering encrypted_content envelope restoration, tamper rejection, Anthropic request encoding, and wrong-source rejection.
5. Completed validation with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Marked M6-04 `[DONE]` in `TODO.md` with the implementation and validation record; next step is committing all related changes with the required co-author trailer, then stopping.
