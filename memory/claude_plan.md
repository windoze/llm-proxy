## Execution Plan

I cannot provide private chain-of-thought reasoning, but I will keep this file updated with a concrete execution plan and progress notes.

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Inspect only the files and context needed for that task, including the latest commit if it directly mentions an unfinished issue relevant to the selected task.
3. Implement the selected task completely, without changing task scope or using workarounds.
4. Run the required formatting, linting, and test validation for the changed code.
5. Update `TODO.md` by prefixing the completed task title with `[DONE]` and filling in its completion record.
6. Update this plan file at key milestones.
7. Commit all changes for this single task with a descriptive message and then stop.

## Progress

### Current invocation

- Created the required plan file before project inspection.
- Read `TODO.md` and identified the first incomplete task as `M7-08 [TODO] README 与部署文档`.
- Latest commit is `[M7-07] Add end-to-end regression snapshots`; it does not mention an unfinished issue blocking M7-08.
- This invocation will update only the README/deployment documentation and task bookkeeping, then commit and stop.
- Added `README.md` with setup, configuration, client routing, backend/profile, deployment, testing, and known-limit documentation.
- Updated `TODO.md` to mark `M7-08` as `[DONE]` with a documentation-only completion record.
- Ran a final documentation diff whitespace check; skipped Rust formatting/lint/test because no compiled source changed.

### Previous invocation notes preserved

- Selected first incomplete task: `M7-07` end-to-end regression test suite.
- Latest commit `b3fe349` only records completion progress for `M7-06`; it did not mention an unfinished issue that changed the `M7-07` scope.
- Added a dedicated `src/tests/e2e_regression.rs` module for missing chain-1 reasoning and rich-chain text snapshots.
- Added M7 snapshots to existing chain 2 and chain 4 rich reasoning/tool-use/multiturn JSON/SSE tests.
- Generated and reviewed committed `insta` snapshots; no real credentials were introduced.
- Validation passed: baseline and final `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets --quiet`.
- Marked `M7-07` as `[DONE]` in `TODO.md`.
