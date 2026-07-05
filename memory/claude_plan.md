# Execution Plan

I will follow `TODO.md` as the source of truth, complete only the first task whose heading is not prefixed with `[DONE]`, validate the result, update the task record, commit the completed work, and stop.

1. Read `TODO.md` to identify the first incomplete task and its validation requirements.
2. Check the latest commit message only for directly relevant unfinished work tied to that selected task.
3. Inspect the task-relevant code, tests, and documentation before editing.
4. Implement the selected task without changing unrelated behavior or working around spec mismatches.
5. Run formatting, linting, and relevant/full tests required by the task and repository policy.
6. If a blocking prerequisite is discovered, update `TODO.md` with the minimum necessary prerequisite task, leave the current task incomplete, commit that bookkeeping, and stop.
7. If the task is completed, prefix its `TODO.md` heading with `[DONE]`, update its completion record with the validation performed, commit all related changes, and stop.

## Current Task

First incomplete task: `M6-01` — Anthropic backend client (`provider/anthropic_backend.rs`).

Execution steps for this task:

1. Review the existing backend client patterns, especially `provider/responses_backend.rs`, shared error handling, and route/client call sites.
2. Add an Anthropic backend client module that validates endpoint/API key/version, sends JSON requests with `x-api-key` and `anthropic-version`, preserves request body fields, and returns the raw `reqwest::Response` for non-buffered streaming via `bytes_stream()`.
3. Expose the module from `provider::mod`.
4. Add unit tests covering header translation, invalid configuration, upstream error propagation, JSON POST shape, and streaming response consumption.
5. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
6. Mark `M6-01` as `[DONE]` in `TODO.md`, record validation, commit the task changes, and stop.

## Progress

- Implemented `src/provider/anthropic_backend.rs` and exposed it from `provider::mod`.
- Added tests for JSON body preservation, invalid configuration, Anthropic auth/version headers, streaming response consumption, and upstream error body propagation.
- Validation passed: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
