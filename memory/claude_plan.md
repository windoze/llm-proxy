## Execution Plan

I will follow the repository task order exactly and complete only the first incomplete task from `TODO.md`.

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`. Completed: first incomplete task is `M3-04` Responses tool ID mapping and pairing.
2. Check the latest commit message only for unfinished work that is directly relevant to that task. Completed: latest commit is `M3-03`, directly preceding this task and not an unfinished blocker.
3. Inspect the Responses/OpenAI tool ID handling, especially existing Chat↔Anthropic helpers and Responses request/response encode/decode paths.
4. Implement Responses `call_id` pairing validation and Chat↔Responses ID mapping for `function_call` / `function_call_output`. Completed.
5. Add focused tests for Codex multi-turn ID continuity, unknown outputs, duplicates, and cross-protocol mapping. Completed.
6. Run formatting, linting, and relevant validation in the required order. Completed: baseline and post-change checks passed.
7. Mark `M3-04` `[DONE]` in `TODO.md` and update its completion record. Completed.
8. Commit all changes for this invocation with the required co-author trailer, then stop.
