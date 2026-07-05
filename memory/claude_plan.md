# Execution Plan

I will follow the repository task order without doing broad issue triage first. I will:

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit message only for unfinished work directly relevant to that selected task.
3. Inspect the files and tests that the selected task references.
4. Implement the task fully, or add the minimum prerequisite task to `TODO.md` if a concrete blocker makes completion impossible.
5. Run formatting, linting, and relevant tests in the required order, expanding to the full suite if code changes require it.
6. Update this file at major milestones, update `TODO.md` with the completion record and `[DONE]` prefix if the task is completed, and avoid routine `PLAN.md` changes unless phase-level planning changes.
7. Commit all resulting changes with a descriptive message and then stop.

## Current Task

Selected task: `M7-03` — improve error mapping in `error.rs`.

Planned execution:

1. Use `TODO.md` as the source of truth and treat `M7-03` as the first incomplete task.
2. Treat the latest `[M7-02] Implement model routing` commit as completed context; only carry forward directly relevant routing/error-surface expectations.
3. Inspect current `error.rs`, route handlers, provider clients, and protocol encoders/decoders to identify every path that emits local or upstream errors.
4. Implement protocol-aware error formatting for Anthropic Messages and OpenAI Responses, including stable error type/category mapping and HTTP status selection.
5. Preserve and translate upstream status codes plus retry/rate-limit headers, covering backend 4xx/5xx responses instead of only 4xx.
6. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`.
7. Update `TODO.md` with `[DONE] M7-03` and completion evidence, update this file at key milestones, commit, and stop.

Progress:

- Identified `M7-03` as the first incomplete task.
- Confirmed the latest commit is `[M7-02] Implement model routing`; no unfinished issue in that commit preempts `M7-03`.
- Baseline validation before code changes passed: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Implemented protocol-aware error response formatting: generic, Anthropic Messages, and OpenAI Responses schemas now use protocol-specific error bodies and classification.
- Preserved upstream non-success statuses for both 4xx and 5xx responses, including upstream body message extraction and Retry-After/rate-limit header forwarding/translation.
- Wired `/v1/messages` and `/v1/responses` to return frontend-specific errors, including JSON extractor failures.
- Added unit and route tests for Anthropic/Responses error bodies plus OpenAI↔Anthropic rate-limit header translation.
- Completed validation after changes: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets` all passed.
- Updated `TODO.md` to mark `M7-03` `[DONE]` with completion evidence.
