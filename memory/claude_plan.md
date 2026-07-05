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

Selected task: `M6-07` — assemble chain 2 and add integration tests.

Planned execution:

1. Read the `M6-07` task details in `TODO.md` plus the relevant phase plan/design notes.
2. Inspect the current Anthropic-to-Responses request/stream conversion code, backend clients, router assembly, and existing integration-test patterns.
3. Implement chain 2 end-to-end routing so Anthropic requests can target the Responses backend while preserving rich thinking/tool semantics and enabling the optional Anthropic backend cache-control behavior only where specified.
4. Add focused integration tests for the assembled chain 2 route, including request conversion and streaming behavior expected by the task.
5. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M6-07` done in `TODO.md`, update this progress file, commit, and stop.

Progress:

- Identified `M6-07` as the first incomplete task.
- Confirmed the latest commit completed `M6-06` and did not name unfinished work that changes this task.
- Found the existing Anthropic request/response encoders, Anthropic SSE decoder, and Responses SSE encoder already preserve Anthropic thinking signatures through Responses `encrypted_content`; the remaining work is route/backend assembly plus integration tests.
- Added `/v1/responses` backend selection for Anthropic, Anthropic backend client wiring with cache-control injection, non-streaming and streaming Anthropic-to-Responses response adapters, route tests for chain 2, and Anthropic request-message coalescing needed for Codex reasoning/function-call histories.
- Completed `M6-07`, updated `TODO.md` with the `[DONE]` prefix and completion record, and completed formatting, clippy, and full test-suite validation.
