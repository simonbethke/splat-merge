# 1) Command Line Adjustments

Status: **not started**

## Goal
Change the CLI from two fixed inputs to a list of inputs plus an optional base file,
and set the new defaults.

## Requirements (verbatim spec)
- Instead of `-i` and `-j`, there will be a comma-separated list of inputs for the
  voxel task.
- The scale task gets a base file that is called `base`.
- If there are multiple inputs, the voxel task will be executed on the inputs.
- If there is at least one input and the base, the scale task will be executed.
- Initial implementation: always merge all inputs first, then finish with the scale
  task. With inputs `A,B,C,D`:
  1. `A + B  -> TEMP1`
  2. `TEMP1 + C -> TEMP2`
  3. `TEMP2 + D -> TEMP1`
  4. apply the scale task to `TEMP1` and write the final output.
- Default voxel size: `0.5`
- Default threshold: `0.01` (currently `0.1` in `src/main.rs`)

## Notes
- Flag naming for the input list (e.g. `-l/--inputs`) and `--base` to be settled in
  the CLI section of `docs/documentation.md` before coding (docs first, per AGENTS.md).
- Temp file naming/location (TEMP1/TEMP2) to be decided: where they live and whether
  they are deleted after a successful run.
- This is a documented-CLI change explicitly requested by the user (AGENTS.md
  boundary satisfied by this request).
- Existing pairwise semantics (voxel tie, scale threshold boundary) must stay as
  documented in `docs/documentation.md` and stay covered by "Required test cases".

## Definition of done
- `docs/documentation.md` CLI + workflow sections updated first; changelog entry added.
- Code implements the listed behavior; defaults `0.5` / `0.01` in `Args`.
- `cargo clippy --all-targets -- -D warnings` and `cargo test --release` clean,
  `cargo fmt --all` applied.
- `tasks/tasklog.md` updated with date + time; this file removed.
