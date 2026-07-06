# Execution Plan

Current task: `M7-RV` review and project acceptance.

1. Confirm no earlier `TODO.md` task is incomplete and check for any `[BLOCKED]` entries.
2. Review the M7 acceptance surfaces: configuration, model routing, error mapping, observability, retry/limit behavior, regression tests, README/deployment docs, and CI.
3. Check the codebase for project-wide design risks relevant to the review, especially the no-state rule and reasoning/tool-use fidelity paths.
4. Completed validation in order: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`; all passed.
5. No unscheduled validation failure or blocking implementation gap was found.
6. Update `TODO.md` by marking `M7-RV` `[DONE]` with a completion record listing findings, blocked items, and follow-up recommendations.
7. Because this completes the last task in `TODO.md`, commit the review record and create the final `endtag` tag, then stop.
