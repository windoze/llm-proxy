## Execution Plan

I will follow `TODO.md` as the authoritative task list and complete exactly the first task whose heading is not prefixed with `[DONE]`.

1. Read `TODO.md` to identify the first incomplete task and its validation requirements.
2. Inspect the immediately relevant project files and recent commit context only as needed for that task.
3. Implement the task as specified, without narrowing scope or using workarounds.
4. Run formatting, linting, and the relevant/full test suite according to the task requirements and repository conventions.
5. If validation exposes an unscheduled failure, fix it if in scope or add the minimum prerequisite task to `TODO.md` before the blocked task, then stop.
6. Mark the completed task heading with `[DONE]`, update its completion record, and update this file with key progress.
7. Commit all resulting changes for this task with a descriptive message and stop without starting the next task.

## Current Task

Selected first incomplete task: `M2-09 [TODO] 链 3 集成测试`.

Task-specific plan:

1. Reuse the existing `/v1/messages` route test harness with `wiremock` DeepSeek mocks.
2. Add recorded Claude Code-style Anthropic request samples for streaming plain text, reasoning, and multi-turn tool-use histories.
3. Snapshot the full Anthropic SSE frame sequence with `insta` instead of checking only substring fragments.
4. Keep existing non-streaming/configuration route coverage intact.
5. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M2-09` as `[DONE]`, update its completion record, then commit the task changes.

Progress:

- Added route-level fixtures/helpers and three `wiremock` + `insta` streaming snapshot cases for text, reasoning, and multi-turn tool-use coverage.
- Ran baseline and final validation successfully: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M2-09` as `[DONE]` in `TODO.md` with completion notes.
