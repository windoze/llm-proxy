## Execution Plan

I will complete exactly the first incomplete task listed in `TODO.md`, using `TODO.md` as the source of truth.

1. Read `TODO.md` to identify the first task whose title is not prefixed with `[DONE]`.
2. Review the task body, dependencies, validation requirements, and relevant recent commit context.
3. Inspect only the files needed for that task and implement the required changes without broad unrelated triage.
4. Run formatting, linting, and tests required by the task and repository conventions.
5. If an unscheduled blocking issue or test failure is discovered, add the minimum prerequisite task to `TODO.md`, commit that bookkeeping, and stop.
6. If the task is completed, update `TODO.md` by prefixing the task title with `[DONE]` and filling in its completion record.
7. Commit all task-related changes with a descriptive message, then stop without starting the next task.

## Current Task

First incomplete task identified: `M1-04` — define the provider capability profile trait in `src/provider/mod.rs`.

Execution steps for this task:

1. Inspect the existing provider module, IR message types, and DESIGN §5 to match naming and capability semantics.
2. Define `CapabilityProfile` with the required methods for parameter blocking, reasoning effort normalization, reasoning echo policy, multiple choice support, base URL, model mapping, and thinking-model detection.
3. Add a `GenericOpenAi` default implementation with no parameter blocklist, `EchoPolicy::Never`, support for multiple choices, identity model mapping, and no thinking-model behavior unless existing design text requires otherwise.
4. Add focused tests for the trait defaults and owned model-name mapping behavior.
5. Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M1-04` as `[DONE]` in `TODO.md`, add a completion record, commit the task changes, and stop.

## Progress

- Selected `M1-04` as the first incomplete task.
- Latest commit completed `M1-03`; no unfinished issue from that commit needs to preempt `M1-04`.
- Implemented `CapabilityProfile` and `GenericOpenAi` in `src/provider/mod.rs` with focused unit tests for neutral defaults and custom base URLs.
- Completed validation with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M1-04` as `[DONE]` in `TODO.md` with a completion record.
- Next step: commit the task changes and stop.
