# Execution Plan

I will follow `TODO.md` as the source of truth, identify the first task whose heading is not prefixed with `[DONE]`, complete exactly that task, update its completion record, run the required formatting/linting/tests for the changes made, commit the result, and stop.

Steps:
1. Read `TODO.md` and the latest commit message to determine the current task and any directly relevant unfinished work.
2. Inspect only the files needed for that task and identify the implementation and validation requirements.
3. Implement the task without workarounds or scope narrowing; if a concrete blocker prevents completion, add the minimum prerequisite task to `TODO.md`, commit that bookkeeping, and stop.
4. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and the relevant/full test suite as required by the task and observed changes.
5. Mark the task heading `[DONE]`, update its completion record with what changed and validation performed, commit all task-related changes, and stop.

Current task: M7-09 GitHub CI pipeline.

Implementation notes:
1. Add `.github/workflows/ci.yml` for push and pull requests to `main`.
2. Use stable Rust with `rustfmt` and `clippy` components.
3. Cache Cargo registry, git checkout cache, and `target/`.
4. Run exactly the non-network validation commands documented in `TESTING.md`: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets` without `--ignored`.

Progress:
1. Added the GitHub Actions workflow for M7-09.
2. Ran the required local validation commands successfully.
3. Marked M7-09 `[DONE]` in `TODO.md` with its completion record.
