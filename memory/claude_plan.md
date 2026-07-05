## Execution plan

I will follow `TODO.md` as the authoritative task list and complete only the first task whose heading is not prefixed with `[DONE]`. I will keep this file updated at key milestones with the observable plan, decisions, and results; I will not record private chain-of-thought.

## Current task: M4-01

First incomplete task selected: `M4-01 [TODO]` — define the reasoning envelope format in `reasoning/envelope.rs`.

## Step-by-step plan

1. Read the `M4-01` task body and relevant design sections to determine the exact envelope fields, encoding rules, and validation expectations.
2. Inspect the existing module tree, IR thinking structures, protocol reasoning handling, and unit test style.
3. Check the latest commit message for any unfinished issue directly relevant to `M4-01`.
4. Implement the `reasoning::envelope` module and wire it into the crate using existing error/result and serde conventions.
5. Add focused tests for envelope construction, JSON/base64 behavior, round-trip decoding, invalid envelope handling, and preservation of provider/source metadata required by the task.
6. Run `cargo fmt --all`, then `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`.
7. If validation succeeds, update `TODO.md` by marking `M4-01` `[DONE]` and filling in the completion record. Update `PLAN.md` only if the phase-level plan changes.
8. Commit all changes for `M4-01` with a descriptive message and stop.

## Progress

- Selected task: `M4-01 [TODO]`.
- Read the M4 task/design requirements and confirmed no latest-commit unfinished issue blocks `M4-01`.
- Added the initial `reasoning::envelope` module with versioned provider source blocks, base64 JSON transport, CRC integrity checks, and focused unit tests.
- Validation passed with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M4-01` `[DONE]` in `TODO.md` with completion notes; the task changes are ready to commit, and this invocation will stop without starting `M4-02`.
