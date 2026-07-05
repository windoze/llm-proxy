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

## Selected Task

First incomplete task: `M5-03` — encode `Thinking { source: Responses }` as an Anthropic `thinking` content block whose `signature` carries a reasoning envelope produced from the opaque Responses reasoning payload.

## Task-Specific Steps

1. Update `src/protocol/anthropic/encode.rs` so Anthropic response encoding can fail with `ProxyError` instead of panicking or silently dropping envelope errors.
2. For `Provider::Responses`, require `Thinking.opaque`, wrap it as `SourceBlock { source: Responses, payload: opaque }`, and set the Anthropic `signature` to `wrap_as_signature(...)`.
3. Preserve existing Anthropic-origin signature behavior and current text/tool/image encoding behavior.
4. Update callers and unit tests for the new fallible encoder shape.
5. Add focused tests that unwrap the generated signature and prove the original Responses opaque bytes are preserved.
6. Run formatting, clippy, and the full test suite, then update `TODO.md` and commit the completed task.

## Progress Update

Implemented M5-03. The Anthropic non-streaming encoder now returns `Result<Value>`, propagates protocol/envelope errors, preserves existing Anthropic-origin signatures, and wraps Responses-origin `Thinking.opaque` bytes into an Anthropic thinking `signature` via the reasoning envelope. Added unit coverage for successful signature unwrap and missing opaque payload rejection. Formatting, clippy, and the full test suite pass.

## Completion Update

`M5-03` is marked `[DONE]` in `TODO.md` with implementation notes and validation results. No phase-level `PLAN.md` update was needed.
