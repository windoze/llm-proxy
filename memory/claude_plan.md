# Execution Plan

I will follow the task list in `TODO.md` as the source of truth and complete only the first task whose heading is not prefixed with `[DONE]`.

1. Read `TODO.md` to identify the first incomplete task and its requirements, dependencies, validation steps, and completion record format.
2. Check the latest commit message only after the current task is identified, and only consider it if it explicitly mentions an unfinished issue directly relevant to that task.
3. Inspect the repository structure and the files/tests related to the selected task.
4. Implement the task as specified, adding or updating tests and documentation only where directly required.
5. Run formatting, linting, and relevant/full tests according to the task’s validation requirements and repository conventions.
6. If validation exposes an unscheduled failing test or a concrete blocker, either fix it if it is in scope or add the minimum prerequisite task to `TODO.md`, commit that bookkeeping, and stop.
7. When the task is complete, update `TODO.md` by prefixing the task heading with `[DONE]` and filling in its completion record with the actual implementation and validation details.
8. Commit all changes for this task with a clear task-scoped commit message including the required co-author trailer.
9. Stop without starting the next task.

## Current Task

Selected first incomplete task: `M2-05` — implement `IrEvent` stream to Anthropic SSE event encoding in `protocol/anthropic/stream.rs`.

Task-specific steps:
1. Done: checked the latest commit message; it completed M2-04 and did not mention an unfinished issue relevant to M2-05.
2. Done: inspected IR event definitions, Anthropic non-stream encoder, stream decoder conventions, error handling, DESIGN §6.1, and Anthropic streaming docs.
3. Done: implemented the Anthropic SSE encoder with event names, JSON payloads, block index validation, stop reason mapping, usage mapping, and SSE framing.
4. Done: added focused tests covering thinking/text/tool-use events, usage/cache mapping, SSE bytes, non-sequential block indexes, and unopened block deltas.
5. Done: ran `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Done: marked M2-05 `[DONE]` in `TODO.md` and filled its completion record.

Remaining step: commit all task changes and stop.
