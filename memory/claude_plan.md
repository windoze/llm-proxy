# Execution Plan

I will work through exactly one `TODO.md` task in this invocation. This file records the actionable plan and progress updates without exposing private reasoning.

1. Read `TODO.md` first and identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit only for an explicitly unfinished issue that is directly relevant to that task.
3. Inspect the relevant source, tests, and project configuration for that task.
4. Implement the task as written, adding or updating tests and documentation only where directly required.
5. Run formatting, linting, and the relevant/full validation required by the task and repository conventions.
6. If validation exposes unscheduled failures, either fix them or add the minimum prerequisite task(s) to `TODO.md` before marking the current task complete.
7. Mark the completed task title with `[DONE]` in `TODO.md` and update its completion record.
8. Commit all changes for this invocation with a clear task-specific message and the required co-author trailer, then stop.

Progress:
- Replaced the existing plan file before running repository commands.
- Identified first incomplete task: `M1-02` (`ir/request.rs`) to define the unified IR request/response types.
- Latest commit is `[M1-01] Define IR content blocks`; it does not explicitly mention an unfinished issue that changes the selected task.
- Implemented the unified IR request/response structs, tool choice, stop reason, usage types, and focused serialization tests in `src/ir/request.rs`.
- Validation passed: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M1-02` as `[DONE]` in `TODO.md` with a completion record.
