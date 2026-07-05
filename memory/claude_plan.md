# Execution Plan

I will follow the repository task order without doing broad issue triage first. I will:

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit message only for unfinished work directly relevant to that selected task.
3. Inspect the files and tests that the selected task references.
4. Implement the task fully, or add the minimum prerequisite task to `TODO.md` if a concrete blocker makes completion impossible.
5. Run formatting, linting, and relevant tests in the required order, expanding to the full suite if code changes require it.
6. Update this file at major milestones, update `TODO.md` with the completion record and `[DONE]` prefix if the task is completed, and avoid routine `PLAN.md` changes unless phase-level planning changes.
7. Commit all resulting changes with a descriptive message and then stop.

## Current Task

Selected task: `M6-RV` — review M6 chain 2 and the full four-chain milestone.

Planned execution:

1. Use `TODO.md` as the source of truth and treat `M6-RV` as the first incomplete task.
2. Treat the latest `[M6-07] Assemble Responses to Anthropic chain` commit as directly relevant review input, not as a reason for broad unrelated triage.
3. Inspect the documented real-client test procedure and existing ignored e2e test surfaces for M6 chain 2.
4. Run required validation in order: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`.
5. If credentials and CLIs are available, run the M6 real Codex → gateway `/v1/responses` → Anthropic backend validation; if a directly blocking defect appears, fix it before marking the task done.
6. Confirm all four chains have completed review evidence in `TODO.md` and that chain 2 preserves Anthropic signature ↔ Responses `encrypted_content` reasoning round-trip.
7. Update `TODO.md` by marking `M6-RV` `[DONE]` with a completion record, update this file at milestones, commit, and stop.

Progress:

- Identified `M6-RV` as the first incomplete task.
- Confirmed the latest commit is `[M6-07] Assemble Responses to Anthropic chain`, directly relevant to this review.
- Repository validation passed once before real-client testing.
- Real Codex reached the gateway but failed because Codex 0.142.5 now includes a top-level Responses `custom` tool declaration for `apply_patch`; the decoder rejected that before the chain could run.
- Added a focused decoder fix to adapt Responses `custom` tool declarations into Anthropic-compatible string-input tools, plus a regression test based on the captured Codex request shape.
- A rerun exposed the next Codex 0.142.5 deferred-tool declaration, `tool_search`; added parsing for it as a function-style tool using its declared schema so the full current Codex tool list can enter the Anthropic backend.
- After tool parsing passed, the real Anthropic-compatible backend rejected the credential when sent as `x-api-key`; updated `AnthropicBackendClient` to use official `x-api-key` for `sk-ant-` keys and bearer auth for token-shaped credentials, with provider tests covering both modes.
- The real backend then rejected the `.envrc` model value because it includes a literal `[1m]` suffix; the next review retry will use a process-local sanitized model override and will not modify or commit `.envrc`.
- A forced real reasoning round-trip exposed the backend's Anthropic-compatible thinking dialect: it requires `thinking.type=adaptive` and `output_config.effort`; added `output_config` to Anthropic request extra pass-through so Codex-protocol requests can enable reasoning for this backend.
- Final validation passed: full Rust validation is green, real Codex CLI tool-use through `/v1/responses` to the Anthropic backend returned `m6-rv-tool-ok`, and a real adaptive-thinking Codex-protocol two-turn request verified Anthropic signature ↔ Responses `encrypted_content` round-trip.
- Updated `TODO.md` to mark `M6-RV` `[DONE]` with completion evidence and environment notes.
