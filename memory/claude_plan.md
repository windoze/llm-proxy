## Execution plan

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit message only for directly relevant unfinished work tied to that task.
3. Inspect the task requirements, affected files, and existing validation commands.
4. Implement the task exactly as specified, adding prerequisite TODO entries only if a concrete blocker makes direct completion impossible.
5. Run formatting, linting, and tests required by the task and repository conventions.
6. Update `TODO.md` with a `[DONE]` prefix and completion record if the task is completed, or record any blocker/prerequisite if it cannot be completed.
7. Commit all task-related changes, then stop without starting the next task.

## Current task

- Selected first incomplete task: `M5-RV` — review M5 chain 4 plus real integration.
- Latest commit: `[M5-06] Wire Anthropic messages to Responses backend`; no unfinished issue was mentioned in the commit subject/body, and it is directly relevant as the implementation under review.
- Review focus confirmed: M5 requires Responses `encrypted_content` to round-trip losslessly through an Anthropic `thinking.signature` envelope, plus rich stream index/tool-call correctness.
- Local prerequisites found: `claude`, `codex`, `.envrc`, `OPENAI_API_ENDPOINT`, and `OPENAI_API_KEY` are present; values must not be printed or committed.
- Rust validation passed: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Live M5 check found a real protocol-boundary blocker: Claude Code sends Anthropic-only `output_config`; the current Anthropic → IR → Responses path forwarded it unchanged, and the Responses backend rejected it with `unknown_parameter`.
- Fixed extra passthrough: Responses request encoding now forwards only Responses-native extra fields, preserving supported extras such as `metadata`/`store` while dropping Anthropic-only fields before backend submission.
- Live retry reached the second turn and confirmed tool-use execution, then exposed a second real Responses request mismatch: generated `function_call_output` items included `is_error`, which the backend rejects. The encoder now omits generated `is_error` fields while still decoding optional inbound `is_error` into IR.
- Raw M5 check showed that simply dropping Claude Code `output_config` made the tool-use round trip work but did not request Responses reasoning. Captured Claude Code request shape uses `output_config: {"effort":"high"}` plus top-level `thinking`; direct backend probing confirmed Responses accepts `reasoning: {"effort":"high"}` and returns reasoning items with `encrypted_content`.
- Fixed reasoning mapping: Anthropic/Claude `output_config.effort` now maps into Responses `reasoning.effort` while unsupported Anthropic-only fields remain filtered.
- Final live validation passed: Claude Code 2.1.200 → local `/v1/messages` → real Responses backend `gpt-5.5` completed a two-turn Bash tool-use flow without 400.
- Final raw real-backend validation passed: a Responses reasoning item was emitted as Anthropic `thinking.signature` on turn 2, then sent back on turn 3 and accepted by the backend, confirming signature → encrypted_content round trip.
- `TODO.md` has been updated with `[DONE] M5-RV` and completion records. Next step is committing the task changes.
