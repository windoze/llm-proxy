## Execution plan

I will follow `TODO.md` as the authoritative task list and complete only the first task whose heading is not prefixed with `[DONE]`. I will keep this file updated at key milestones with the observable plan, decisions, and results; I will not record private chain-of-thought.

## Current task: M4-03

First incomplete task selected: `M4-03 [TODO]` — implement the Anthropic signature-side symmetric envelope helpers in `reasoning/envelope.rs`.

## Step-by-step plan

1. Check the latest commit message only for any explicitly unfinished issue directly relevant to `M4-03`.
2. Inspect the existing reasoning envelope implementation, Anthropic signature handling, and relevant DESIGN notes from M4.
3. Implement helpers that wrap an envelope as an Anthropic thinking `signature` string.
4. Implement matching signature unwrap/validation behavior, reusing existing envelope checksum verification.
5. Add focused tests for legal signature shape, round-trip recovery, malformed signature rejection, byte preservation, and tamper detection.
6. Run `cargo fmt --all`, then `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`.
7. If validation succeeds, update `TODO.md` by marking `M4-03` `[DONE]` and filling in the completion record. Update `PLAN.md` only if the phase-level plan changes.
8. Commit all changes for `M4-03` with a descriptive message and stop.

## Progress

- Selected task: `M4-01 [TODO]`.
- Read the M4 task/design requirements and confirmed no latest-commit unfinished issue blocks `M4-01`.
- Added the initial `reasoning::envelope` module with versioned provider source blocks, base64 JSON transport, CRC integrity checks, and focused unit tests.
- Validation passed with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M4-01` `[DONE]` in `TODO.md` with completion notes; the task changes are ready to commit, and this invocation will stop without starting `M4-02`.
- Selected task: `M4-02 [TODO]`.
- Confirmed latest commit is completed M4-01 work with no unfinished issue blocking M4-02.
- Added Responses reasoning item wrapper/unwrapper helpers in `reasoning/envelope.rs`, including `rs_` id generation, permissive id omission on unwrap for current Codex behavior, and checksum-backed tamper detection tests.
- Validation passed with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M4-02` `[DONE]` in `TODO.md` with completion notes; next step is committing the task changes.
- Selected task: `M4-03 [TODO]`.
- Confirmed latest commit is completed M4-02 work with no unfinished issue blocking M4-03.
- Added Anthropic signature wrapper/unwrapper helpers in `reasoning/envelope.rs`, including a `llm_proxy_sig_v1:` sentinel prefix and checksum-backed tamper detection.
- Validation passed with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M4-03` `[DONE]` in `TODO.md` with completion notes; next step is committing the task changes.
