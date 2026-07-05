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

Selected task: `M7-02` — implement model routing in `provider/router.rs`.

Planned execution:

1. Use `TODO.md` as the source of truth and treat `M7-02` as the first incomplete task.
2. Treat the latest `[M7-01] Implement configuration system` commit as completed context; only carry forward directly relevant model/config routing expectations.
3. Inspect current `config.rs`, route startup, provider client construction, profile selection, and endpoint-specific routing code to understand all model routing surfaces already in use.
4. Implement a centralized router in `provider/router.rs` that selects the configured backend/profile by requested model and endpoint type, rewrites the backend model name, and returns clear errors for missing matches.
5. Add focused tests for alias routing, endpoint restrictions, backend/profile lookup, legacy/default behavior, and no-match diagnostics.
6. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`.
7. Update `TODO.md` with `[DONE] M7-02` and completion evidence, update this file at key milestones, commit, and stop.

Progress:

- Identified `M7-02` as the first incomplete task.
- Confirmed the latest commit is `[M7-01] Implement configuration system`; no unfinished issue in that commit preempts `M7-02`.
- Baseline validation before code changes passed: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Implemented `provider::router::ModelRouter` with endpoint-aware backend/profile selection, exact model alias rewriting, legacy override handling, clear incompatibility/missing-backend errors, and default DeepSeek chat fallback for envless compatibility.
- Wired `/v1/messages` and `/v1/responses` through the router so each handler rewrites `IrRequest.model` before upstream encoding and builds Chat/Responses/Anthropic clients from the selected backend config.
- Updated route tests to construct `Config` objects and added focused router tests for alias routing, endpoint restrictions, implicit rich-backend selection, legacy override diagnostics, and default DeepSeek behavior.
- Completed validation after changes: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets` all passed.
- Updated `TODO.md` to mark `M7-02` `[DONE]` with completion evidence.
