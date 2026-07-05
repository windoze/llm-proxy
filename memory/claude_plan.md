# Execution Plan

I will follow the repository task order without doing broad issue triage first. I will:

1. Read `TODO.md` to identify the first task whose heading is not prefixed with `[DONE]`.
2. Check the latest commit message only for unfinished work directly relevant to that selected task.
3. Inspect the files and tests that the selected task references.
4. Implement the task fully, or add the minimum prerequisite task to `TODO.md` if a concrete blocker makes completion impossible.
5. Run formatting, linting, and relevant tests in the required order, expanding to the full suite if code changes require it.
6. Update this file at major milestones, update `TODO.md` with the completion record and `[DONE]` prefix if the task is completed, and avoid routine `PLAN.md` changes unless phase-level planning changes.
7. Commit all resulting changes with a descriptive message and then stop.

## Current Task

Selected task: `M7-01` — implement the configuration system in `config.rs`.

Planned execution:

1. Use `TODO.md` as the source of truth and treat `M7-01` as the first incomplete task.
2. Treat the latest `[M6-RV] Review Anthropic chain` commit as completed review context; only carry forward the directly relevant note that formal model/config handling is scheduled in M7-01/M7-02.
3. Inspect current `config.rs`, route startup, provider client construction, existing environment variables, and tests to understand all configuration surfaces already in use.
4. Implement strongly typed config loading with file support plus environment overrides for listen address, backend definitions, model aliases, and feature switches, preserving existing environment behavior where it is the current public interface.
5. Add startup validation and focused tests for file loading, env override precedence, required backend fields, model alias parsing, and feature switch defaults.
6. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, then `cargo test --all --all-targets`.
7. Update `TODO.md` with `[DONE] M7-01` and completion evidence, update this file at key milestones, commit, and stop.

Progress:

- Identified `M7-01` as the first incomplete task.
- Confirmed the latest commit is `[M6-RV] Review Anthropic chain`; no unfinished issue in that commit preempts `M7-01`.
- Baseline validation passed before code changes: formatting check, clippy with denied warnings, and the full test suite were green.
- Implemented the typed configuration system with TOML/YAML loading, legacy and structured environment overrides, startup validation, model alias parsing, backend definitions, and cache/reasoning switches.
- Wired startup and route state construction through `Config`, preserving the existing env-only behavior, keeping the global Anthropic default max-token override, and making Anthropic cache-control injection configurable.
- Added focused config tests and completed validation after changes: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets` all passed.
- Updated `TODO.md` to mark `M7-01` `[DONE]` with completion evidence.
