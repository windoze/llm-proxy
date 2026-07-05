## Execution plan

I will maintain a concise, step-by-step execution log here. I cannot record private chain-of-thought, but this file will include the actionable plan, decisions, blockers, and completed milestones.

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Review only the files and context needed for that task, including the latest commit if it directly mentions an unfinished issue relevant to the selected task.
3. Implement the selected task completely, without narrowing scope or introducing workarounds.
4. Run the required formatting, linting, and tests in the requested order.
5. Update `TODO.md` to prefix the completed task with `[DONE]` and record completion details, or add a prerequisite task if a concrete blocker prevents completion.
6. Update this file at key milestones and update `PLAN.md` only if the phase-level plan changes.
7. Commit all relevant changes for this invocation and stop without starting the next task.

## Progress log

- Created this invocation plan before inspecting project files.
- Selected first incomplete task: `M5-06` (`/v1/messages` can route to a Responses backend, with wiremock integration coverage for encrypted reasoning + tool-use round trips).
- Latest commit is `[M5-05] Implement Responses rich streaming`; it directly provides the stream decoder/encoder needed for this task, with no separate unfinished prerequisite noted.
- Implementation direction: add temporary pre-M7 route selection for `/v1/messages`, using DeepSeek Chat for `deepseek-*` models and Responses for non-DeepSeek models when `OPENAI_API_ENDPOINT`/`OPENAI_API_KEY` are configured, with an explicit override for tests/operators if needed.
- Implemented M5-06: `/v1/messages` now routes Anthropic requests to the Responses backend for configured non-DeepSeek models, supports non-streaming and streaming Responses responses, and preserves Responses encrypted reasoning through Anthropic signatures.
- Added wiremock coverage for non-streaming multi-turn reasoning + tool-use signature round trip and streaming Responses SSE → Anthropic SSE signature delta output.
- Updated `TESTING.md` with the temporary pre-M7 backend selection behavior.
- Validation completed: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets` all passed.
- Marked `M5-06` as `[DONE]` in `TODO.md`. No `PLAN.md` update was needed because phase-level sequencing did not change.
