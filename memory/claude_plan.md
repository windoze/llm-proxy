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

Selected task: `M6-06` — optional stateless Anthropic backend `cache_control` injection.

Planned execution:

1. Read DESIGN §3.1 and the current Anthropic request encoding/backend route code.
2. Add a pure, opt-in helper that computes safe Anthropic cache breakpoints from the encoded message structure and injects `cache_control` blocks without storing state.
3. Wire the helper behind an explicit backend-client switch so M6-07 can enable it when assembling the Anthropic route.
4. Add focused tests for disabled-by-default behavior, injection placement, idempotence, and backend-client request preparation.
5. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M6-06` done in `TODO.md`, update this progress file, commit, and stop.

Progress:

- Identified `M6-06` as the first incomplete task.
- Confirmed the previous commit completed `M6-05` and did not name unfinished work that changes this task.
- Added the stateless Anthropic cache-control injection module and backend-client opt-in switch.
- Added tests for breakpoint selection, string content conversion, idempotence, over-limit errors, and the client switch.
- Completed the required formatting, linting, and full test-suite validation.
- Marked `M6-06` as `[DONE]` in `TODO.md`.
