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

Selected task: `M5-05` — implement rich-to-rich streaming conversion from Responses SSE through IR events to Anthropic SSE.

Task-specific steps:

1. Inspect the existing Responses non-streaming decoder, Anthropic SSE encoder, stream modules, and any prior Chat SSE decoder patterns.
2. Add `stream/responses_decoder.rs` to convert Responses SSE events into provider-neutral `IrEvent`s while preserving block indexes and reasoning/tool-call structure.
3. Reuse or extend the existing Anthropic SSE encoder so the decoded IR event stream emits valid Anthropic SSE events.
4. Add focused unit tests for Responses text/reasoning/function-call streaming, index/type alignment, and end-to-end Responses SSE → IR event → Anthropic SSE behavior.
5. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M5-05` as `[DONE]` in `TODO.md`, update this file with completion status, and commit the task changes.

## Progress Update

Implemented M5-05. Added `stream::responses_decoder` for Responses SSE → IR event decoding with strict `output_index` alignment, text/reasoning/function-call lifecycle handling, usage/status decoding, and encrypted-content recovery from `output_item.done` or terminal `response.output`. Extended streaming IR with `ThinkingMetadata{source,opaque}` so rich reasoning metadata is not lost, and taught Anthropic SSE encoding to emit Responses-origin metadata as a wrapped `signature_delta`. Added coverage for direct decoding, terminal encrypted-content fallback, missing encrypted-content rejection, Responses SSE → IR → Anthropic SSE signature deltas, and Responses stream encoder metadata preservation.

## Completion Update

`M5-05` is marked `[DONE]` in `TODO.md` with implementation notes and validation results. No phase-level `PLAN.md` update was needed.
