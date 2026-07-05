## Execution Plan

I will follow the repository task order exactly and complete only the first incomplete task from `TODO.md`.

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`. Completed: first incomplete task is `M3-05` 装配链 1 端到端路由.
2. Check the latest commit message only for unfinished work that is directly relevant to `M3-05`. Completed: latest commit is `M3-04` and does not introduce an unfinished blocker for this route task.
3. Inspect the existing route assembly, OpenAI Chat, Anthropic, Responses, and provider code needed to wire chain 1 end-to-end. Completed.
4. Implement the `M3-05` route behavior completely according to `TODO.md`, reusing existing protocol helpers and avoiding task-private workarounds. Completed: `/v1/responses` now decodes Responses requests, encodes Chat backend requests, calls the DeepSeek-compatible Chat backend, and returns Responses JSON/SSE.
5. Add or update focused tests that validate the end-to-end route behavior required by `M3-05`. Completed: added non-streaming, streaming tool-use, and bearer-token translation coverage.
6. Run formatting, linting, and relevant validation in the required order; address any unscheduled failures. Completed: baseline and post-change formatting, clippy, and full test suite passed.
7. Mark `M3-05` `[DONE]` in `TODO.md` and update its completion record. Completed.
8. Commit all changes for this invocation with the required co-author trailer, then stop.
