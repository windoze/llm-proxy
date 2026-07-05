## Execution Plan

I will complete exactly the first incomplete task listed in `TODO.md`, then stop after committing the result.

1. Read `TODO.md` to identify the first heading that is not prefixed with `[DONE]`.
2. Check the latest commit message only for unfinished work directly relevant to that selected task.
3. Inspect the task requirements, dependencies, validation notes, and nearby project context needed to implement it.
4. Implement the task without changing unrelated behavior or using workarounds.
5. Run the required formatting, linting, and tests for the changed scope, escalating to the full suite when required by the task or by observed failures.
6. If a blocking prerequisite is discovered, update `TODO.md` with the minimum required prerequisite task, commit that bookkeeping, and stop.
7. If the task is completed, mark its title in `TODO.md` with `[DONE]`, update its completion record, and avoid routine `PLAN.md` changes unless phase-level sequencing changed.
8. Commit all relevant changes for this task with a clear task-specific message and the required co-author trailer.

## Progress

- Created this plan before running repository commands.
- Read the top of `TODO.md`; the first incomplete task is `M2-06` (`tool ID 映射与配对`).
- Latest commit is `M2-05` and does not mention unfinished work directly relevant to `M2-06`.
- Inspected the current IR/protocol code and DESIGN §6.2/§6.3. Current adapters preserve IDs verbatim but lack a central invariant for Chat `tool_call_id` ↔ Anthropic `tool_use_id` and for prior-call/result pairing.
- Implementation approach: add a focused protocol helper that records bidirectional Chat/Anthropic tool ID mappings, builds the stateless identity mapping from IR request history, rejects duplicate or unknown tool results, and covers multi-tool/multi-turn cases with tests.
- Added `protocol::tool_ids` with `ToolIdMap`, request-history pairing validation, and tests for explicit mappings, multi-turn identity mappings, unknown result IDs, duplicate results, duplicate tool-use IDs, unresolved calls, and tool blocks on invalid roles.
- Validation completed: formatting, clippy with warnings denied, and the full Rust test suite all passed.
- Marked `M2-06` as `[DONE]` in `TODO.md` with completion notes.
