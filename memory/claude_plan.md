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

Selected task: `M7-04` — centralize unsupported-feature capability decisions in `protocol/capability.rs`.

Planned execution:

1. Use `TODO.md` as the source of truth and treat `M7-04` as the first incomplete task.
2. Treat the latest `[M7-03] Improve error mapping` commit as completed context; no unfinished issue from that commit preempts `M7-04`.
3. Inspect DESIGN §6.5 and the current IR-to-protocol encoders to find existing scattered drop / emulate / reject decisions.
4. Add `src/protocol/capability.rs` with a centralized capability table for each `IR -> protocol` request direction and helper functions for extra-parameter filtering/rejection.
5. Wire the existing Anthropic, Responses, and OpenAI Chat request encoders through the capability table without changing intended protocol output except where unsupported parameters should now be explicitly rejected.
6. Add tests that lock drop / emulate / reject decisions, including the documented Responses `json_schema` to Anthropic tool-emulation policy.
7. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`.
8. Update `TODO.md` with `[DONE] M7-04` and completion evidence, update this file at key milestones, commit, and stop.

Progress:

- Re-read `TODO.md` and found `M7-04` is now the first incomplete task.
- Baseline validation before code changes passed: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Implemented `src/protocol/capability.rs` as the central `IR -> protocol` feature decision table for pass-through, drop, emulate, and reject behavior.
- Wired OpenAI Chat, Anthropic Messages, and OpenAI Responses request encoders through the capability table, including structured-output and reasoning-effort emulation paths.
- Added tests covering capability decisions, extra filtering, Responses json_schema emulation to Anthropic tools, Chat/Responses structured-output translation, and unsupported structured-output/tool conflicts.
- Completed validation after changes: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets` all passed.
- Updated `TODO.md` to mark `M7-04` `[DONE]` with completion evidence.
