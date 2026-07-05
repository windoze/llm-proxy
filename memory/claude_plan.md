# Execution Plan

1. Read TODO.md to identify the first incomplete task, treating only headings prefixed with [DONE] as complete.
2. Inspect the latest commit only for unfinished work directly relevant to that task.
3. Read the task-specific requirements and nearby project context needed to implement it without broad unrelated triage.
4. Implement the task exactly as specified, adding prerequisite TODO entries only if a concrete blocker makes direct completion impossible.
5. Run formatting, linting, and relevant tests in the required order; fix any observed unscheduled failures or add prerequisite tasks before completion.
6. Update TODO.md by prefixing the completed task heading with [DONE] and filling in its completion record; update PLAN.md only if phase-level sequencing changes.
7. Commit all resulting changes with a descriptive message and the required co-author trailer, then stop without starting the next task.

Progress: M4-05 selected for this invocation.

Checkpoint:
- First incomplete task identified: M4-05, length-limit protection and default-disabled fallback interface.
- Latest commit completed M4-04 and does not mention unfinished work that blocks M4-05.
- Implementation plan: inspect the M4-05 task body and existing `reasoning/envelope.rs` implementation, add configurable maximum opaque payload protection, expose a disabled-by-default fallback storage interface rather than silently accepting oversized payloads, cover both Responses and Anthropic envelope paths as applicable, add focused unit tests, then run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Completion plan: after validation, mark M4-05 `[DONE]` in `TODO.md`, record the validation outcome, update this file, and commit the completed task.
- Implemented `EnvelopeLimits`, `ReasoningStore`, and default `NoopStore`; default wrap/unwrap paths remain stateless and oversized envelopes fail unless a store is explicitly supplied.
- Added store-reference envelopes for oversized payloads and wired fallback-aware APIs for raw envelopes, Responses reasoning items, and Anthropic signatures.
- Added unit tests for under-limit inline behavior, disabled-store rejection, configured-store round-trip, Responses `encrypted_content` limits, and Anthropic prefixed signature limits.
- Completed validation: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked M4-05 `[DONE]` in `TODO.md` with its completion record. Next step is committing the completed task changes.
