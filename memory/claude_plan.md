# Execution Plan

## Scope
Work on exactly the first incomplete task listed in `TODO.md`, using `TODO.md` as the authoritative ordering and completion record.

## Steps
1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit message for any explicitly unfinished issue directly relevant to that task.
3. Inspect only the files needed to understand and implement that task.
4. Implement the task completely, preserving existing behavior outside the requested scope.
5. Run formatting, linting, and relevant tests in the required order; fix any observed unscheduled failures or add the minimum prerequisite task if a concrete blocker prevents completion.
6. Update `TODO.md` by prefixing the completed task heading with `[DONE]` and filling its completion record.
7. Update this file at major milestones.
8. Commit all task-related changes with a descriptive message, including the required co-author trailer.
9. Stop after completing this single task.

## Progress
- Created initial execution plan.
- Identified first incomplete task: `M3-03` (`IR event -> Responses SSE encoding` in `protocol/responses/stream.rs`).
- Reviewed the existing Anthropic SSE encoder, Responses non-streaming encoder, IR event model, and OpenAI SDK stream event shapes needed for `M3-03`.
- Added `protocol::responses::stream` with a stateful IR-event to Responses SSE encoder and unit tests for text, reasoning, tool-call argument streaming, and ordering validation.
- Validation passed after implementation: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M3-03` as `[DONE]` in `TODO.md` with its completion record.
