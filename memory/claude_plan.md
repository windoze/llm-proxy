## Execution Plan

I will complete exactly the first incomplete task listed in `TODO.md`, using `TODO.md` as the source of truth.

1. Read `TODO.md` to identify the first task whose title is not prefixed with `[DONE]`.
2. Review the task body, dependencies, validation requirements, and relevant recent commit context.
3. Inspect only the files needed for that task and implement the required changes without broad unrelated triage.
4. Run formatting, linting, and tests required by the task and repository conventions.
5. If an unscheduled blocking issue or test failure is discovered, add the minimum prerequisite task to `TODO.md`, commit that bookkeeping, and stop.
6. If the task is completed, update `TODO.md` by prefixing the task title with `[DONE]` and filling in its completion record.
7. Commit all task-related changes with a descriptive message, then stop without starting the next task.

## Current Task

First incomplete task identified: `M1-03` — define streaming IR event types in `src/ir/event.rs`.

Execution steps for this task:

1. Inspect existing IR modules to match naming, serde, dead-code allowances, and test conventions.
2. Implement `IrEvent` and `BlockKind` in `src/ir/event.rs` with the variants required by `TODO.md`.
3. Ensure the IR module exports or wires the new event definitions consistently with existing staged modules.
4. Add focused tests if existing IR modules include serde shape tests for staged types.
5. Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M1-03` as `[DONE]` in `TODO.md`, add a completion record, and commit the task changes.

## Progress

- Implemented `IrEvent` and `BlockKind` in `src/ir/event.rs` with serde support and focused serialization tests.
- Completed validation with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M1-03` as `[DONE]` in `TODO.md` with a completion record.
- Next step: review the final diff and commit the task changes.
