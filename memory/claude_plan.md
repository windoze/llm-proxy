## Execution plan

1. Read `TODO.md` and identify the first task whose heading is not prefixed with `[DONE]`.
2. Review only the files and recent commit context needed to understand that task and its direct prerequisites.
3. Implement the task exactly as specified, avoiding scope changes or workaround behavior.
4. Run the required formatting, linting, and tests in the requested order for any code changes.
5. Update `TODO.md` by prefixing the completed task title with `[DONE]` and filling in its completion record.
6. Update this file when a key step completes or the plan materially changes.
7. Commit all task-related changes with a descriptive message and the required co-author trailer.
8. Stop after completing and committing exactly one task.

## Progress

- Identified `M1-01` as the first incomplete task: define IR content block types in `src/ir/message.rs`.
- Added the canonical role, content block, thinking, provider, image source, and message IR types in `src/ir/message.rs`.
- Validated with `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all --all-targets`.
- Marked `M1-01` as `[DONE]` in `TODO.md` with a completion record.
