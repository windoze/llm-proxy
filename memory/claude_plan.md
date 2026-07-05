## Execution plan

I will follow `TODO.md` as the authoritative task list, identify the first task whose heading is not prefixed with `[DONE]`, and complete exactly that task. This file records the actionable plan and progress checkpoints for the invocation.

1. Read `TODO.md` first to identify the first incomplete task and its validation requirements. **Done:** first incomplete task is `M0-02` (`建立目录结构与模块骨架`), requiring the planned `src/` module skeleton and a passing `cargo build`.
2. Check the latest commit only for unfinished work directly relevant to that selected task.
3. Inspect the relevant code, tests, and documentation for that task.
4. Implement the task completely, or add the minimum prerequisite task to `TODO.md` if a concrete blocker makes direct completion impossible. **Done:** added the requested `src/` skeleton modules and wired top-level declarations in `main.rs`.
5. Run formatting, linting, and tests required by the task and repository policy. **Done:** `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo build --quiet`, and `cargo test --all --all-targets --quiet` passed.
6. Update `TODO.md` by prefixing the completed task heading with `[DONE]` and filling in its completion record, or record any required prerequisite/blocker without marking the task done. **Done:** `M0-02` is marked `[DONE]` with validation notes.
7. Update this file at major milestones.
8. Commit all changes for this invocation with a descriptive message and stop.
