# Execution Plan

## Reasoning summary
- `TODO.md` is the authoritative task list, and the first heading not prefixed with `[DONE]` is the only task to execute in this invocation.
- I will not perform broad triage before selecting that task. I will only inspect history or related files when needed to understand or validate the selected task.
- If the selected task is blocked by a concrete unscheduled prerequisite, I will add the minimum required prerequisite task to `TODO.md`, leave the current task incomplete, commit that bookkeeping, and stop.
- If implementation proceeds, I will complete the selected task as written, validate it with the repository's existing formatting/lint/test workflow, update the completion record, commit the changes, and stop without starting the next task.

## Step-by-step plan
1. Read `TODO.md` and identify the first incomplete task using the `[DONE]` prefix rule. **Completed:** first incomplete task is `M1-06`.
2. Check the latest commit message only for unfinished work directly relevant to that selected task. **Completed:** latest commit is `[M1-05] Implement DeepSeek profile`; no unfinished issue was mentioned.
3. Read the selected task details and immediately relevant files: OpenAI Chat protocol module, IR request/message types, provider profile trait and DeepSeek profile, error type, and DESIGN sections for DeepSeek request/tool/reasoning mapping.
4. Implement `protocol/openai_chat/decode.rs` with `chat_request_to_ir(body: &Value, profile: &dyn CapabilityProfile) -> Result<IrRequest>`. **Completed.**
5. Ensure mappings cover system messages, normal message content, assistant `tool_calls`, `role:tool` results, DeepSeek `reasoning_content`, tools, tool choice, sampling/max-token/stop/stream parameters, and provider-specific `extra` handling with profile blocklist behavior. **Completed.**
6. Add focused unit tests for request parsing behavior introduced by M1-06. **Completed.**
7. Run `cargo fmt --all`, then `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`. **Completed; all passed.**
8. Mark `M1-06` as `[DONE]` in `TODO.md` and update its completion record with implementation and validation notes. **Completed.**
9. Commit all task-related changes, including this plan file. **Pending.**
10. Stop after committing `M1-06`; do not start `M1-07`.
