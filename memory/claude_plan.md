# Execution Plan

I will complete exactly the first incomplete task in `TODO.md` and then stop.

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit only for directly relevant unfinished work tied to that selected task.
3. Inspect the selected task requirements, related source files, and existing tests.
4. Implement the task fully, preserving existing behavior and avoiding workarounds.
5. Run formatting, linting, and tests in the requested order; fix or explicitly schedule any observed unscheduled failure before marking the task complete.
6. Update `TODO.md` by prefixing the completed task heading with `[DONE]` and updating its completion record.
7. Update this file at key milestones.
8. Commit all task-related changes with a descriptive message and the required co-author trailer.

Status: Implemented `M2-07` by adding `protocol::openai_chat::encode`, including IR-to-Chat request encoding, consecutive same-role message merging, Chat tool-call/tool-result mapping, DeepSeek blocklist handling, reasoning effort normalization, and `n > 1` rejection. Post-change `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets` passed.
