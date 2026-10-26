# AGENTS.md

Rust CLI project. This file defines how the agent works. It contains no project content.
All project knowledge (purpose, modes, binary format rules, CLI, architecture, test cases)
lives in `docs/documentation.md`. That file is the single source of truth.

## Documentation protocol (highest priority)
1. At the start of a session, read `docs/documentation.md` fully, once. Do not re-read it
   later in the same session; before changing a section, re-read only that section.
2. For every new requirement, changed behavior, new flag, or new constraint:
   update `docs/documentation.md` in the same task as the code change.
3. Write the docs change first, then implement against it.
4. If a request contradicts the docs, stop and ask one specific question. Do not silently
   override the docs and do not silently ignore the request.
5. Docs describe the current state only. Record changes in the "Changelog" section
   (one line per change, newest first), not as history inside other sections.
6. Put project facts only in the docs. Put agent rules only in this file. Never duplicate
   content between the two; link by section name instead.
7. A task is not done until the docs match the code. Compare the sections you touched
   with the code before finishing.
8. Do not edit `README.md` unless asked. If a change makes it outdated, say so in your summary.

## Commands
Always `--release` for build and test: debug builds are unusably slow on real data.
- Check + lint (includes type checking, use this instead of `cargo check`):
  `cargo clippy --all-targets --message-format=short -q -- -D warnings`
- Build: `cargo build --release -q`
- Test: `cargo test --release -q`
- Single test: `cargo test --release -q <name> -- --nocapture`
- Format: `cargo fmt --all`

Output rules:
- Always use the `-q` and `--message-format=short` flags shown above.
- Fix the first reported error only, then run the command again.
- Never run the same failing command again without changing something first.

## Tasks
- A sequential task queue lives in `tasks/`: one file per pending task
  (`NN-<name>.md`, ordered by the number prefix) plus `tasks/tasklog.md`.
- `tasks/tasklog.md` is the source of truth for order and status; the task files
  hold the full spec for each work item.
- When asked to implement work, work tasks strictly one at a time in order.
  Implement only the current task; do not start the next one in the same session.
- When a task is finished (code done, docs in sync, clippy/tests/fmt clean):
  record the date and time under the task's headline in `tasks/tasklog.md`
  (moved to "Completed") and delete the task file. Only then may the next task start.

## Workflow
1. Read docs (once), then the relevant code. Do not guess APIs.
   Locate with search first, then read only the needed range. Do not re-read unchanged files.
2. Update docs (see protocol), then make the smallest code change that satisfies them.
3. After every change: run the clippy command, and only the tests related to the change.
4. Before finishing: full `cargo test --release -q`, `cargo fmt --all`, clippy clean, docs in sync.
5. If a command fails, fix the cause. Never weaken tests or lints.

## Editing
- Change files with targeted edits (replace only the changed lines).
- Never rewrite a whole file to change a few lines. This applies to code and to the docs.

## Code rules
- `unsafe` only for `memmap2::Mmap::map` and unavoidable byte reinterpretation.
  Every `unsafe` block needs a `// SAFETY:` comment stating the invariant.
- No `unwrap()` / `expect()` outside tests. Use `?` with `anyhow` (binary) or `thiserror`.
- Validate input before indexing. Return a clear error on bad files; never panic.
- Hot loops: no per-item allocation, no `.clone()`, no string formatting. Prefer slices and iterators.
- `#[allow(...)]` requires a comment with the reason.
- Public items get `///` doc comments.
- Follow existing logging style; do not introduce new logging crates.

## Testing
- Unit tests next to code in `#[cfg(test)] mod tests`.
- Use small synthetic data generated in the test, never real multi-GB files.
- Cover every case listed in the docs section "Required test cases" when touching related logic.
- New behavior and bug fixes require a test. For bugs: write the failing test first.

## GUI verification
- A GUI task (any change to layout, theming, or widget state) is not finished
  until the agent has created a state JSON per the docs "Debug rendering"
  section, run `cargo run --release -- --debug-render <state.json>`, and
  visually compared `screenshot.jpg` against the docs "GUI" section.
- Screenshots are evidence: do not claim a GUI change looks right without one.

## Performance
- Never claim a speedup without measuring before and after on the same input.
- No new dependencies or parallelism (`rayon`) without asking.

## Git and files
- Never commit, push, or switch branches unless asked.
- Never edit `Cargo.lock` by hand. Do not touch `target/`, `.git/`, or any `.ply` files.

## Boundaries
- Ask before: adding dependencies, changing CLI flags or output format, changing documented semantics.
- No unrelated refactoring.

## Communication
- Concise: what changed, why, which doc sections were updated.
- If ambiguous, ask one specific question instead of guessing.
- If you could not run or verify something, say so explicitly.