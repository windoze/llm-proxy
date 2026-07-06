# Execution Plan

I will use `TODO.md` as the source of truth, complete exactly the first incomplete task, update the task record, validate the result, commit the changes, and stop.

## Steps

1. Read `TODO.md` to identify the first task whose title is not prefixed with `[DONE]`.
2. Inspect only the files and context needed for that task, including `PLAN.md` only if phase-level context is required.
3. Check the latest commit message for any unfinished issue that directly blocks or belongs to the selected task.
4. Implement the selected task without changing unrelated behavior or working around spec mismatches.
5. Run the required formatting, linting, and tests in the order specified by the task and project instructions.
6. If validation exposes an unscheduled failing test or blocking mismatch, either fix it or add the minimum prerequisite task to `TODO.md` before the blocked task, then commit and stop.
7. Mark the task title `[DONE]` in `TODO.md` and update its completion record with the implemented changes and validation performed.
8. Commit all task-related changes with a clear message and the required co-author trailer.

## Progress Log

- Started invocation and refreshed this plan file before inspecting the repository.
- Read `TODO.md`; the first incomplete task is `M7-06 [TODO] 限流与重试`.
- Next steps: inspect the latest commit for directly relevant unfinished work, then review configuration, backend client, routing, and tests needed to add backend retry/backoff plus configurable concurrency/timeout.
- Latest commit is `M7-05` observability and does not identify an unfinished issue that blocks M7-06.
- Baseline validation passed: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Implementation plan: add global backend request configuration, centralize backend HTTP send/retry/concurrency handling in a provider module, wire it into Chat/Responses/Anthropic backend calls, add targeted tests for retries, `Retry-After`, timeout, and stream-held concurrency permits, then update `TODO.md` and commit.
- Implemented backend request controls in `provider::backend_request`, wired them into Chat, Responses, and Anthropic backend calls, and added config/env support for retries, backoff, timeout, and concurrency.
- Validation passed after changes: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `cargo test --all --all-targets`.
- Next step: mark `M7-06` done in `TODO.md`, include the completion record, and commit all task changes.
