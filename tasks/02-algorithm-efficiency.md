# 2) Algorithm Efficiency

Status: **not started**

## Goal
Merge all input splats in one run (instead of pairwise) and add proportional
progress output.

## Requirements (verbatim spec)
- The algorithm shall merge all input splats in one run, not only two at a time.
  It needs one hashmap per input file, then runs across them.
- Progress output with two tasks:
  - `1 Mapping` — fills the hashmaps
  - `2 Merging` — runs through the splats (selection/output)
- Progress output per million splats in both phases (status line triggered when
  counter `% 1_000_000 == 0`).
- Each file contributes to the 100% progress proportionally to its file size.
  Example: a 1 GB and a 9 GB file → the first accounts for 0–10%, the second for
  10–100%.
- Each phase finishes at 100%.
- Sub-progress within a file = splats processed / total splats in that file.
  Example: at 1,000,000 of 5,000,000 splats in the first of the two files above
  (10% weight) → 2% overall.

## Notes
- Applies to voxel mode; decide and document whether scale mode gets the same
  two-phase progress treatment.
- Progress lines go to stdout; style/format to be documented (e.g.
  `2 Merging: 43% (12.3M splats)`).
- "File size" basis for the proportional weighting (bytes on disk) to be confirmed
  in the docs.

## Definition of done
- `docs/documentation.md` updated first (modes/progress behavior + changelog).
- One hashmap per input in voxel mode; multi-input merge in a single pass.
- Progress output matches the weighting math including the worked example.
- `cargo clippy --all-targets -- -D warnings` and `cargo test --release` clean,
  `cargo fmt --all` applied.
- Tests exist for the new logic per "Required test cases".
- `tasks/tasklog.md` updated with date + time; this file removed.
