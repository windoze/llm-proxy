## Current Task

Selected first incomplete task: `M3-01` — Responses request parsing in `protocol/responses/decode.rs`.

## Progress

- Confirmed the latest commit completed M2-RV and found no task-relevant unfinished prerequisite.
- Implemented `responses_request_to_ir` and exposed `protocol::responses::decode`.
- Added unit tests for Codex-style input messages, reasoning `encrypted_content`, function call/output items, tool definitions, tool choice modes, system/developer hoisting, and invalid inputs.
- Ran `cargo fmt --all`.
- Ran `cargo clippy --all-targets -- -D warnings`.
- Ran `cargo test --all --all-targets`.
- Marked `M3-01` as `[DONE]` in `TODO.md` with a completion record.

## Remaining Plan

1. Review the final git diff for only task-relevant changes.
2. Commit all relevant changes with a descriptive message and required co-author trailer, then stop.
