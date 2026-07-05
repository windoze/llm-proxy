## Execution Plan

I will complete exactly the first incomplete task listed in `TODO.md`, using `TODO.md` as the source of truth.

1. Read `TODO.md` to identify the first task whose title is not prefixed with `[DONE]`.
2. Review the selected task body, any directly relevant latest-commit context, and only the design/code areas needed for that task.
3. Implement the selected task without broad unrelated triage or workaround behavior.
4. Run formatting, linting, and tests required by the task and repository conventions.
5. If an unscheduled blocking issue or test failure appears, fix it if in scope; otherwise add the minimum prerequisite task to `TODO.md`, commit that scheduling change, and stop.
6. If the task is completed, update `TODO.md` by prefixing the task title with `[DONE]` and adding a completion record.
7. Commit all task-related changes with a descriptive message, then stop without starting the next task.

## Current Task

First incomplete task identified: `M1-05` — implement the DeepSeek provider profile in `provider/deepseek.rs`.

Execution steps for this task:

1. Inspect the existing provider module, IR echo-policy type, DESIGN §5, and the latest commit message for directly relevant unfinished work.
2. Add a `provider::deepseek` module and implement `CapabilityProfile` for a `DeepSeek` profile with the exact blocklist, reasoning effort normalization, echo policy, multiple-choice support, thinking-model detection, and base URL required by `TODO.md`.
3. Add the required code comment documenting the DESIGN §5 warning about official documentation version inconsistency and the `thinking_mode` page being authoritative.
4. Add focused tests for every DeepSeek profile rule.
5. Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M1-05` as `[DONE]` in `TODO.md`, add a completion record, update this progress file, commit, and stop.

## Progress

- Selected `M1-05` as the first incomplete task.
- Added `src/provider/deepseek.rs` and wired `provider::deepseek` into `src/provider/mod.rs`.
- Added focused DeepSeek profile tests covering base URL, model mapping, parameter blocklist, reasoning effort normalization, echo policy, multiple-choice support, and thinking-model detection.
- Completed validation with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M1-05` as `[DONE]` in `TODO.md` with a completion record.
- Next step: commit the task changes and stop.
