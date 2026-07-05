# Execution Plan

Selected task: `M5-01 [TODO] Responses 后端客户端 (provider/responses_backend.rs)`.

## Steps
1. Check the latest commit message for any unfinished issue directly relevant to `M5-01`.
2. Inspect existing provider/backend, route, config, and Responses protocol code to match project conventions and avoid unrelated changes.
3. Implement a Responses backend client module that sends requests to a Responses-compatible upstream, forces `store=false`, forces `include:["reasoning.encrypted_content"]`, handles bearer-token authentication, and exposes streaming response bytes.
4. Add focused tests for request shaping, include/store enforcement, authentication, and streaming response handling using existing test patterns.
5. Run formatting, clippy, and tests required by repository policy; fix any unscheduled failing tests or add a prerequisite task in `TODO.md` if a concrete blocker prevents completion.
6. Mark `M5-01` as `[DONE]` in `TODO.md` with a completion record. Update `PLAN.md` only if phase-level sequencing changes.
7. Commit all task-related changes with a descriptive message and the required co-author trailer.
8. Stop without starting the next TODO item.

## Progress
- Identified first incomplete task from `TODO.md`: `M5-01`.
- Updated this execution plan before implementation work.
- Baseline validation passed before Rust code changes.
- Added `provider::responses_backend` with request shaping, bearer-auth sending, streaming response preservation, and focused unit tests.
- Validation after implementation passed: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M5-01` as `[DONE]` in `TODO.md`; no `PLAN.md` update was needed.
- Next step: commit the M5-01 changes and stop.
