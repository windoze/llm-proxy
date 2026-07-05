# Execution Plan

I will follow the project task order exactly and complete only the first incomplete task from `TODO.md`.

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit message only for unfinished work that is directly relevant to that task.
3. Inspect the files and tests needed for that single task.
4. Implement the task completely, avoiding workaround behavior or unrelated fixes.
5. Run the required formatting, linting, and tests for the changed code.
6. If validation exposes unscheduled failures, fix them if they are in scope or add the minimum prerequisite task in `TODO.md` before the current task and stop.
7. Mark the completed task with `[DONE]` in `TODO.md` and update its completion record.
8. Commit all changes for this invocation with a clear task-scoped message and then stop.

Progress:
- Plan recorded before task execution.
- First incomplete task identified: `M1-RV` review of M1 IR, OpenAI Chat/DeepSeek parsing, profile rules, and reasoning echo policy.
- Completed focused design/code review for M1 surfaces; no prerequisite blocker or task split is needed.
- Next step: run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`.
- Validation passed: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Updated `TODO.md` to mark only `M1-RV` as `[DONE]` with the review completion record.
