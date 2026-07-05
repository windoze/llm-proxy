## Execution plan

I will follow `TODO.md` as the authoritative task list and complete only the first task whose heading is not prefixed with `[DONE]`. I will keep this file updated at key milestones without recording private chain-of-thought.

1. Read `TODO.md` to identify the first incomplete task and its validation requirements.
2. Check the latest commit only for directly relevant unfinished context.
3. Inspect the relevant project files for that task.
4. Implement the task exactly as specified, avoiding workarounds or unrelated changes.
5. Run formatting, linting, and tests required by the task and repository conventions.
6. If validation exposes an unscheduled failing test or blocker, fix it if in scope or add the minimum prerequisite task to `TODO.md`, then stop.
7. Mark the completed task title in `TODO.md` with `[DONE]` and update its completion record.
8. Commit all changes for this invocation with a descriptive message and stop without starting the next task.

## Current task: M3-07

First incomplete task selected: `M3-07` chain 1 integration tests. The task requires `wiremock` DeepSeek mocks, recorded Codex-style requests sent to `POST /v1/responses`, `insta` snapshots for Responses SSE output, and coverage for text plus multi-turn tool-use.

Completed steps:

1. Ran the existing formatting, lint, and full test baseline successfully.
2. Inspected the Responses route tests, SSE encoder behavior, and existing snapshot conventions.
3. Added deterministic Responses SSE snapshot coverage for Codex-style text and multi-turn tool-use requests.
4. Re-ran formatting, clippy, and the full test suite successfully after the code changes.
5. Marked `M3-07` `[DONE]` in `TODO.md` with a completion record.

Final step: commit the task changes and stop without starting `M3-RV`.
