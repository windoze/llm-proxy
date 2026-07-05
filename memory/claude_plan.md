## Execution Plan

I will follow the task order in `TODO.md` and complete exactly the first task whose heading is not prefixed with `[DONE]`.

1. Read `TODO.md` to identify the first incomplete task and its requirements, dependencies, and validation instructions.
2. Check the latest commit message only for unfinished work directly relevant to that task.
3. Inspect the relevant project files for that task, preserving unrelated user changes.
4. Implement the required changes completely, or add the minimum prerequisite task to `TODO.md` if a concrete blocker makes correct implementation impossible.
5. Run required formatting, linting, and tests in the requested order.
6. Update `TODO.md` with `[DONE]` and a completion record only after the task is implemented and validated.
7. Commit all task-related changes with a descriptive message and stop without starting the next task.

## Current Task

First incomplete task: `M2-04` — implement the OpenAI Chat/DeepSeek SSE chunk to `IrEvent` state machine in `stream/chat_decoder.rs`.

Focused steps:
1. Inspect existing IR event definitions, SSE helper, stream module wiring, and existing protocol test conventions.
2. Add a stateful decoder that emits message/block lifecycle events for text, reasoning, and streamed tool calls.
3. Cover multi-tool streaming and mixed reasoning/content cases with unit tests.
4. Run formatting, linting, and tests before marking `M2-04` done and committing.

Progress:
- Added `src/stream/chat_decoder.rs` and wired it from `src/stream/mod.rs`.
- First validation pass reached clippy and found the new staged module is not yet called by routing, matching earlier staged modules; I will add the same local dead-code allowance used by existing staged components and rerun validation.
- Validation completed successfully with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M2-04` as `[DONE]` in `TODO.md` with a completion record; next step is committing the task changes and stopping.
