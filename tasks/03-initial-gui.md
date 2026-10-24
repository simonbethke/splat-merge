# 3) Initial GUI implementation

Status: **not started**

## Goal
Add a small GUI front end to the tool.

## Requirements (verbatim spec)
- Rather small window, layout top to bottom:
  1. A list of input files at the top; to the right of it, a chooser for the voxel size.
  2. Below that, a single base-file field; to the right of it, a chooser for the scale threshold.
  3. Below these sections, a chooser for the output file.
  4. Below that, a log-output area.
  5. At the very bottom, a run-button.
- Choosing files works via drag and drop or via a file chooser.
- All files have to be `*.ply`; other files are rejected.
- The file list has "Add" and "Remove" buttons:
  - "Add" opens a file picker that also allows picking multiple files at once.
  - "Remove" is only enabled when a file in the list is selected.
- After hitting the run-button:
  - all controls in the top are disabled;
  - status is shown in the log-output;
  - during a run the run-button becomes an "Abort" button;
  - clicking "Abort" opens an are-you-sure dialog;
  - on confirmation the run is canceled and all artifacts (temp files and a
    partially written output file) are cleaned up;
  - the GUI must end in a state that immediately allows a rerun.

## Open decision (must resolve before coding)
- GUI framework is not specified. This adds a dependency, so per AGENTS.md ask the
  user first (candidates: `egui`/`eframe` in Rust vs. web-based Tauri vs. others).
  Decision to be recorded in `docs/documentation.md` (Architecture + changelog).

## Notes
- Task 3 depends on tasks 1 (CLI semantics: inputs list + base) and 2 (progress
  output, which feeds the log-output area) being done first — implement sequentially.
- Abort/cancel must propagate to the worker (no orphan threads/processes) and must
  delete TEMP1/TEMP2 and the partial output before re-enabling the controls.

## Definition of done
- `docs/documentation.md` updated first (new "GUI" section, changelog).
- Layout and all interaction rules above work as specified, including `.ply`
  rejection, Remove enablement, Abort confirmation, and artifact cleanup.
- `cargo clippy --all-targets -- -D warnings` and `cargo test --release` clean,
  `cargo fmt --all` applied.
- `tasks/tasklog.md` updated with date + time; this file removed.
