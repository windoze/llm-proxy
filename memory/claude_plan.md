# Execution Plan

I will follow `TODO.md` as the source of truth and complete exactly the first task whose heading is not prefixed with `[DONE]`.

Current task: `M1-07` — implement `chat_response_to_ir(body:&Value) -> Result<IrResponse>` for non-streaming OpenAI Chat/DeepSeek responses.

Steps:
1. Read `TODO.md` to identify the first incomplete task and its stated validation requirements.
2. Check the latest commit message only for an explicitly mentioned unfinished issue that is directly relevant to that selected task.
3. Implement response decoding in `src/protocol/openai_chat/decode.rs`, reusing existing parsing helpers where appropriate. **Completed.**
4. Cover response content, DeepSeek reasoning, tool calls, finish reasons, and usage/cache token fields with focused tests. **Completed.**
5. Run the required formatting, linting, and tests in the requested order, fixing any failures that are not already explicitly scheduled. **Completed.**
6. Update `TODO.md` by prefixing M1-07 with `[DONE]` and filling in its completion record; update `PLAN.md` only if phase-level planning changes. **Completed; no PLAN.md change needed.**
7. Commit all changes for this task with a clear task-specific message and the required co-author trailer. **Pending.**
8. Stop after this one task is complete.
