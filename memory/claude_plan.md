# Execution Plan

I will follow TODO.md as the authoritative task source, complete only the first task whose heading is not prefixed with `[DONE]`, and stop after committing that task.

## Steps
1. Read `TODO.md` to identify the first incomplete task and its requirements, dependencies, validation requirements, and completion record expectations.
2. Inspect only the files needed to understand and implement that task, plus `PLAN.md` only if phase-level sequencing or dependencies appear affected.
3. Check repository status and recent commit context relevant to the selected task so existing uncommitted work is preserved and any directly relevant unfinished issue is handled correctly.
4. Implement the selected task completely without narrowing scope or using workarounds.
5. Run formatting, linting, and tests required by the task and repository policy; fix any failing unscheduled test encountered, or add the minimum prerequisite task in `TODO.md` if it blocks completion.
6. Update `TODO.md` by prefixing the completed task title with `[DONE]` and filling its completion record. Update `PLAN.md` only if phase-level plan details actually change.
7. Commit all changes required for the completed task with a descriptive message and the required co-author trailer.
8. Stop without starting the next TODO item.

## Progress
- Initial execution plan written before task implementation.

## Selected Task
- First incomplete TODO item: `M4-RV` — Review M4 reasoning mechanism.
- Review scope: envelope round-trip, Responses reasoning-item disguise, Anthropic signature transport, default-off stateful fallback, field fidelity for `encrypted_content` and `status`, checksum/tamper detection, and validation results.

## Review Findings Before Validation
- `src/reasoning/envelope.rs` contains versioned envelope wrapping, CRC integrity checks over version/source/payload, Responses reasoning item wrapping/unwrapping, Anthropic signature wrapping/unwrapping, and explicit default-off store fallback via `NoopStore`.
- Tests cover byte-for-byte round trips for Responses `encrypted_content`, Anthropic `signature`, tool-use payloads, oversized-store behavior, id/status compatibility with Codex 0.142.5 behavior, and payload/source tamper detection.
- Responses reasoning-item preservation code normalizes `status:null` by omitting it while preserving `encrypted_content` and provider extension fields.
- No PLAN.md phase-level changes are required for this review task.
- Next step: run `cargo fmt`, `cargo clippy`, and the full test suite.

## Completion
- Validation passed: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- `TODO.md` has been updated to mark `M4-RV` as `[DONE]` with a completion record and no PLAN.md change.
- Next step: commit the review task changes only, then stop.
