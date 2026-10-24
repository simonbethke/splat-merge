# AGENTS.md

Rust CLI project. This file defines how the agent works. It contains no project content.
All project knowledge (purpose, modes, binary format rules, CLI, architecture, test cases)
lives in `docs/documentation.md`. That file is the single source of truth.

## Documentation protocol (highest priority)
1. Before any change, read `docs/documentation.md` fully.
2. For every new requirement, changed behavior, new flag, or new constraint:
   update `docs/documentation.md` in the same task as the code change.
3. Write the docs change first, then implement against it.
4. If a request contradicts the docs, stop and ask one specific question. Do not silently
   override the docs and do not silently ignore the request.
5. Docs describe the current state only. Record changes in the "Changelog" section
   (one line per change, newest first), not as history inside other sections.
6. Put project facts only in the docs. Put agent rules only in this file. Never duplicate
   content between the two; link by section name instead.
7. A task is not done until the docs match the code. Check this explicitly before finishing.
8. Do not edit `README.md` unless asked. If a change makes it outdated, say so in your summary.

## Commands
Always `--release`: debug builds are unusably slow on real data.
- Check: `cargo check --all-targets`
- Build: `cargo build --release`
- Test: `cargo test --release`
- Single test: `cargo test --release <name> -- --nocapture`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Format: `cargo fmt --all`

## Workflow
1. Read docs, then read the relevant code. Do not guess APIs.
2. Update docs (see protocol), then make the smallest code change that satisfies them.
3. After every change: `cargo check`, `cargo clippy`, `cargo test --release`.
4. Before finishing: `cargo fmt --all`; clippy and tests clean; docs in sync.
5. If a command fails, fix the cause. Never weaken tests or lints.

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