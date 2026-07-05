# Execution Plan

I will follow `TODO.md` as the authoritative task list and complete exactly the first task whose heading is not prefixed with `[DONE]`.

1. Read `TODO.md` first to identify the first incomplete task. **Done:** first incomplete task is `M0-03` (`定义统一错误类型`).
2. Check repository status and the latest commit only for unfinished work directly relevant to `M0-03`.
3. Inspect the existing error module, crate dependencies, and nearby application code needed to define the unified error type.
4. Implement `ProxyError`, `Result<T>`, and `IntoResponse` in `src/error.rs` with JSON error responses and reasonable HTTP status mappings. **Done:** added the unified error enum, result alias, status/code helpers, JSON `IntoResponse`, and focused unit coverage.
5. Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo build --quiet`, and `cargo test --all --all-targets --quiet`. **Done:** all commands passed after exposing `error` as a public module to avoid dead-code warnings for the shared API.
6. Mark `M0-03` as `[DONE]` in `TODO.md`, update its completion record, update this file at key milestones, commit all task-related changes, and stop. **Done:** `TODO.md` now marks `M0-03` complete with implementation and validation notes.
