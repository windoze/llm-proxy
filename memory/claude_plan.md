# Execution Plan

1. Read TODO.md to identify the first incomplete task, treating only headings prefixed with [DONE] as complete.
2. Inspect the latest commit only for unfinished work directly relevant to that task.
3. Read the task-specific requirements and nearby project context needed to implement it without broad unrelated triage.
4. Implement the task exactly as specified, adding prerequisite TODO entries only if a concrete blocker makes direct completion impossible.
5. Run formatting, linting, and relevant tests in the required order; fix any observed unscheduled failures or add prerequisite tasks before completion.
6. Update TODO.md by prefixing the completed task heading with [DONE] and filling in its completion record; update PLAN.md only if phase-level sequencing changes.
7. Commit all resulting changes with a descriptive message and the required co-author trailer, then stop without starting the next task.

Progress: M4-06 selected for this invocation.

Checkpoint:
- First incomplete task identified: M4-06, envelope round-trip tests.
- Latest commit completed M4-05 and does not mention unfinished work that blocks M4-06.
- Implementation plan: read the M4-06 task body, inspect latest commit for directly relevant unfinished work, review existing reasoning envelope tests and protocol surfaces, add the required round-trip tests for Responses reasoning items and Anthropic thinking signatures, run formatting, clippy, and the relevant/full test suite as required, then mark M4-06 `[DONE]` with a completion record.
- Added explicit simulated client-echo round-trip tests for Responses `encrypted_content` and Anthropic `signature`, both preserving tool-use-related opaque payload bytes exactly.
- Completed validation: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked M4-06 `[DONE]` in `TODO.md` with its completion record. Next step is committing the completed task changes.
