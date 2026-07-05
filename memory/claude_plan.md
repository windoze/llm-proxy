# Execution Plan

I will follow `TODO.md` as the authoritative task list and complete exactly the first task whose heading is not prefixed with `[DONE]`.

1. Read `TODO.md` first and identify the first incomplete task by heading prefix.
2. Check recent repository state only as needed for that task, including the latest commit if it directly mentions unfinished work relevant to the selected task.
3. Inspect the files and existing implementation related to the selected task.
4. Implement the task completely, or add the minimum prerequisite task to `TODO.md` if a concrete blocker makes completion impossible.
5. Run formatting, linting, and relevant/full tests as required by the task and repository policy.
6. Update `TODO.md` by prefixing the completed task heading with `[DONE]` and filling its completion record, or document any blocker/prerequisite without marking it done.
7. Update this plan file at major milestones.
8. Commit all task-related changes with a descriptive message and the required co-author trailer.
9. Stop after this one task.

## Current Invocation

Selected task: `M5-04` — reverse-restore Claude Code-returned Anthropic `thinking.signature` values into Responses reasoning items for the backend request `input`.

Task-specific steps:

1. Inspect the existing Anthropic request decoder, Responses encoder helpers, and reasoning envelope APIs.
2. Teach Anthropic thinking decode to recognize gateway-owned signatures, unwrap them, and convert Responses-source envelopes back into `Thinking { source: Responses, opaque: original_payload }`.
3. Add or extend Responses request encoding so IR messages containing Responses-origin thinking emit `type:"reasoning"` input items with restored `encrypted_content`/preserved item fields.
4. Add focused unit tests covering signature unwrap, restored Responses request input, and invalid/wrong-source signature rejection.
5. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M5-04` as `[DONE]` in `TODO.md`, update this file with completion status, and commit the task changes.

## Progress Update

Implemented M5-04. Anthropic request decoding now recognizes gateway-owned thinking signatures, unwraps Responses-source envelopes into Responses-origin IR thinking blocks, and rejects wrapped signatures carrying any other source. Responses request encoding now emits restored `reasoning` input items from Responses-origin thinking, preserving full reasoning item JSON when available or rebuilding a minimal item with the restored `encrypted_content` otherwise. Added focused unit coverage for signature unwrap/rejection and Responses request input restoration. Formatting, clippy, and the full test suite pass.

## Completion Update

`M5-04` is marked `[DONE]` in `TODO.md` with implementation notes and validation results. No phase-level `PLAN.md` update was needed.
