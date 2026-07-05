# Execution Plan

1. Read TODO.md to identify the first incomplete task, treating only headings prefixed with [DONE] as complete.
2. Inspect the latest commit only for unfinished work directly relevant to that task.
3. Read the task-specific requirements and nearby project context needed to implement it without broad unrelated triage.
4. Implement the task exactly as specified, adding prerequisite TODO entries only if a concrete blocker makes direct completion impossible.
5. Run formatting, linting, and relevant tests in the required order; fix any observed unscheduled failures or add prerequisite tasks before completion.
6. Update TODO.md by prefixing the completed task heading with [DONE] and filling in its completion record; update PLAN.md only if phase-level sequencing changes.
7. Commit all resulting changes with a descriptive message and the required co-author trailer, then stop without starting the next task.

Progress: Plan initialized before task inspection.

Checkpoint:
- First incomplete task identified: M4-04, Responses reasoning item field-preservation.
- Latest commit only completed M4-03 and does not introduce an explicit unfinished prerequisite.
- Implementation focus: preserve `encrypted_content` during Responses reasoning item conversion and ensure `status: null` is omitted rather than forwarded.
- Implemented a Responses-local reasoning helper that normalizes reasoning items by preserving all fields, requiring `encrypted_content`, and dropping only `status: null`.
- Wired Responses decode/encode to carry normalized raw reasoning item JSON through `Thinking.opaque` when available, with tests for encrypted-content preservation and status handling.
- Refined the Responses encoder to propagate protocol errors for malformed preserved reasoning items instead of panicking.
- Completed validation: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked M4-04 `[DONE]` in TODO.md with its completion record. Next step is committing the completed task changes.
