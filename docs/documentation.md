# splat-merge: Documentation

Single source of truth for project content. Agent behavior rules are in `AGENTS.md`.

## Purpose
Rust CLI (with a small GUI front end in the same executable) to merge or filter
3D Gaussian Splatting PLY files. Built for multi-GB inputs on consumer hardware
(target: 64 GB RAM) using memory mapping (`memmap2`).

Motivation: a single training run is limited to ~20M splats (24 GB VRAM). A scene is trained
multiple times on different pose areas (each at 20M splats, SH1), then the results are merged
into one larger splat.

## Modes

### voxel (density merge)
- Space is divided into cubes of `--voxel-size` (0.5 = 50 cm).
- Per voxel, the input with the most splats in that voxel wins: its splats there
  are kept and the splats of all other inputs in that voxel are dropped
  (XOR by density).
- All inputs are merged in one run: one density hashmap per non-empty input, then
  a per-voxel winner pass — no pairwise steps, no intermediate re-maps of results.
- Ties (two or more inputs at the max count for a voxel): the earliest input in the
  list wins. Kept splats are written in input-list order (record order within each
  file), so a 3-input run yields the same bytes as the old two-step chain.
- Used to combine detail-area splats.
- Side effect: floaters in areas occluded in one detail view (typically large splats).

### scale (size merge)
- Keep splats from file 1 whose max world-space scale is < `--threshold`.
- Keep splats from file 2 whose max world-space scale is >= `--threshold`.
- Used to remove floaters: file 1 = voxel-merged details, file 2 = base scene (all poses,
  20M splats, also used for LOD). Empirically good threshold: 0.01.
- Runs as a single pass over the two sources (selection and output together); progress
  reports only the `3 Scale` task (no mapping phase, see "Progress").

### Null input
Filename `-` in any input slot (input list entry or `--base`) means an empty file.
This allows empty slots in the input list. `--base -` is equivalent to omitting
`--base` (a single input then passes through; with several inputs the output is
the voxel-merged list alone). If every data source (all inputs, and the base if
given) is `-`, the run is rejected with an error (no file to supply stride and header).

## Binary format rules (must not break)
- PLY data is binary little-endian with a fixed per-splat stride
  (position, SH, opacity, scale, rotation).
- Kept splat records are copied byte-exact; output preserves the exact stride and property layout.
- Output header is reconstructed from the source header: all lines are emitted
  unchanged except `element vertex <count>`, which is rewritten with the final count.
- Scale properties are stored as log values. World-space scale = `exp(scale_n)`;
  "max scale" = maximum over the three axes.
- Inputs are never loaded fully into RAM: mmap for reading, buffered writer for output.

## Invocation
One executable, two modes:
- **No arguments**: launches the GUI window (see "GUI").
- **One or more arguments**: CLI mode with the flags below.

## CLI
| Flag | Meaning |
|---|---|
| `-i, --inputs <a,b,c>` | Comma-separated list of input PLY files; an entry may be `-` (empty) |
| `--base <file>` | Optional base PLY (file 2 of the scale step); omitted or `-` = no base (no scale step) |
| `-o, --output <file>` | Output PLY |
| `-v, --voxel-size <f>` | Cube edge length for the voxel step (default: `0.5`) |
| `-t, --threshold <f>` | Max-scale threshold for the scale step (default: `0.01`) |

Input list entries are trimmed of surrounding whitespace; an empty entry (e.g. `a,,b`)
is an error. `--mode` no longer exists: the voxel step always runs on the input list, and
the scale step runs when `--base` is a real file (`--base -` = no base; see "Pipeline").

## Typical workflow
```bash
# Merge detail areas, then combine with the base scene (one command):
cargo run --release -- -i detail_a.ply,detail_b.ply,detail_c.ply,detail_d.ply --base base.ply -o merged.ply

# Voxel-only merge of two detail files:
cargo run --release -- -i detail_a.ply,detail_b.ply -o detail_ab.ply

# Single input without a base: byte-identical copy (passthrough):
cargo run --release -- -i scene.ply -o copy.ply
```

## GUI
A small native window (framework: `eframe` 0.37 / `egui`; native file dialogs via
`rfd`), launched when the binary is run without arguments (see "Invocation").
Layout, top to bottom:
1. Input file list: full width, one entry per line, with a static height
   for 7 rows (beyond 7 entries it scrolls inside the fixed-height box),
   with a control row below it — "Add" (multi-select picker) and "Remove"
   (enabled only while a list entry is selected) on the left, and the
   voxel-size chooser right-aligned on the right.
2. Base-file field (single-file picker via a "Browse" button; an empty base
   means no base, like omitting `--base`). The field expands to the remaining
   width of the row; the "Browse" button and the scale-threshold chooser are
   right-aligned at the end of the row.
3. Output-file chooser (save dialog). The field expands to the remaining
   width of the row; the "Browse" button is right-aligned at the end of the
   row.
4. Log output area: multiline, fills all remaining window height; new lines
   appear at the bottom.
5. Run button at the very bottom of the window, aligned to the right.

Theme and spacing:
- Dark theme, applied once at startup (independent of the system theme).
- Every interactive widget - the file list rows, the Base and Output text
  inputs, all buttons and the choosers - gets a visible 1px border that
  defines its area.
- 8px spacing between all elements.

Files:
- Chosen with a file chooser or by dragging them onto the window.
- `*.ply` only (case-insensitive extension); other files are rejected with a log
  line. A path already in the list is not added twice.

Run:
- Validation before start: at least one input, a `.ply` output path, voxel size > 0,
  threshold >= 0. On failure the reason is logged and nothing starts.
- While a run is active, the controls above are disabled and the pipeline's
  progress/status/success lines stream into the log area (the same lines the CLI
  would print on stdout).
- The Run button becomes "Abort". Clicking it opens a native are-you-sure dialog;
  on confirmation the cancel flag is set, the run stops at the next record
  boundary, and the temp files plus the partially written output file are removed
  (see "Pipeline" — Cancellation).
- After an abort, a failure, or a success the controls are re-enabled and the
  settings are preserved, so a rerun is immediate.

## Pipeline
A run executes these steps in order:

1. **Voxel step** (only with 2 or more input-list entries): all inputs are merged
   into one file in a single run (see "Modes"). The step writes directly to the
   output file when there is no base; with a base it writes the result to a
   temporary file and re-maps it for the next step.
2. **Scale step** (only when a base is given): the accumulated source (the single
   input, or the voxel-step result) is file 1; the base file is file 2. Kept splats
   (file 1: max scale `<` threshold; file 2: `>=`) are written to the output file.
3. **Passthrough** (exactly one input, no base): the input is copied to the
   output verbatim (header count unchanged). No progress lines are emitted.

Temporary files:
- Named `<output-stem>.temp1.ply` beside the output file; it holds the
  voxel-step result when a scale step follows. A second slot (`.temp2.ply`) is
  no longer created, but cleanup still removes both names.
- Always removed at the end of the run, on success or on error. A failed removal of
  a temp file produces a warning, not an error.

Errors:
- Empty input-list entry, or all data sources `-`: error (nothing to read).
- Mismatched stride between any non-empty file: error (property layouts differ;
  a byte-exact merge is impossible).

Cancellation (GUI abort; not reachable from the CLI):
- The merge loops check a shared cancel flag at each record; when it is set the
  run stops with the error `Aborted by user` and writes no further output.
- The temp files are removed as on any finished run; additionally the partially
  written output file is removed and the line
  `Aborted; temporary and partial output files removed.` is written to the output
  sink (stdout in the CLI, the log area in the GUI).
- Other (non-abort) errors do not remove the output file.

## Progress
Progress is written to the run's output sink (stdout in the CLI, the log area
in the GUI), one set of lines per phase. A status line is
printed whenever the phase's cumulative splat counter crosses another million
(triggered at each multiple of 1,000,000), and each phase closes with a final
100% line.

Tasks:
- `1 Mapping` (voxel step only): the per-input density hashmaps are filled.
- `2 Merging` (voxel step only): the splats are run through, selected, and written.
- `3 Scale` (scale step only): the splats are run through, selected, and written.
The passthrough reports no progress.

Weighting: each file of a phase contributes to the 100% proportionally to its
byte size on disk (the mapped file size); sub-progress within a file is
`splats processed / total splats in that file` (a zero-splat file counts as
fully processed). Overall percentage = the sum over files of
`weight x sub-progress`, clamped to 0–100.
Worked example: two files, 1 GB and 9 GB (weights 10% and 90%), the first with
5 000 000 splats. At 1 000 000 of file 1's splats processed:
`10% x (1M / 5M) = 2%` → `1 Mapping: 2% (1.0M splats)`.

Line format: `<task>: <pct>% (<value>M splats)`, e.g. `2 Merging: 43% (12.3M splats)`.
`<pct>` is an integer (rounded); `<value>` is the crossed million boundary
(per-million lines) or the phase's total splats (final 100% line), in millions
with one decimal. I/O failures on progress lines are ignored.

## Architecture
Single binary (CLI or GUI — see "Invocation"). Dependencies: `clap` (CLI), `memmap2`
(input mapping), `hashbrown` (voxel density maps), `eframe` 0.37 / `egui` (GUI window),
`rfd` 0.17 (native file dialogs in the GUI).

Source layout:
- `src/main.rs`: clap-derived `Args` (the shared CLI argument struct); `main` launches
  the GUI when the binary is started without arguments (see "Invocation"), otherwise
  parses the args and calls `pipeline::run`.
- `src/ply.rs`: `PlyInfo` + `parse_ply_header`, `validate`, `get_pos`, `get_max_scale`,
  and `rewrite_header` (reconstructed output header).
- `src/merge.rs`: `merge_voxel` (all non-empty inputs in one run: one density map per
  input, per-voxel earliest-input-at-max wins, kept splats in input-list order) and
  `merge_scale` (single pass, threshold keep). Both write the reconstructed header
  then the kept records byte-exact, checking the given cancel flag at each record
  (`UserAborted` error — see "Pipeline" — Cancellation). Operate on byte slices, not
  files; emit progress through the given progress writer.
- `src/progress.rs`: `Progress` — file-weighted phase progress (per-million status
  lines, final 100% line); weights = byte-size shares of the phase's files.
- `src/gui.rs`: `eframe`/`egui` window per the "GUI" section (`launch` is the GUI
  entry point). `is_ply` extension check; `add_input` (`.ply` + duplicate rejection
  with a log line); `ChanWriter` (an `io::Write` sink forwarding complete lines to
  the log area over an mpsc channel); `start_run` (validation, worker thread with a
  shared cancel flag); the abort confirmation dialog.
- `src/pipeline.rs`: input-list parsing, file mapping/validation, stride check,
  single voxel step (to the output, or to temp1 then re-map when a scale step
  follows), scale step, passthrough, cleanup. `run` (CLI) locks stdout and delegates
  to `run_with`, which also serves the GUI worker; progress lines and the final
  `Success:` line go to the given sink; a canceled run additionally removes the
  partial output (see "Pipeline" — Cancellation).

Responsibilities by item:
- `Args`: clap-derived CLI (flags in the "CLI" section).
- `PlyInfo` + `parse_ply_header`: scans the ASCII header; computes the per-splat byte
  stride from the `property` declarations (`float` = 4, `double` = 8, `uchar`/`uint8` = 1
  byte), stores the vertex count, records the byte offset of `scale_0`, and keeps the raw
  header used to re-emit the output header.
- `validate`: rejects non-empty files whose binary data length is not a multiple of the
  stride or is truncated below the declared record count.
- `get_pos(record)`: decodes the first 12 bytes of a record as the little-endian f32
  position (assumes position is the first property; used for voxel keys).
- `get_max_scale(record, offset)`: decodes `scale_0..2` and returns the max of the three
  log values, exponentiated to world space.
- `merge_voxel`: merges all non-empty inputs in one run into any `Write` target
  (temp or output). Mapping phase: one hashbrown map per input counts per-voxel
  density, keyed by `(x, y, z)` i32 voxel indices (`floor(p / voxel_size)`). The
  per-voxel winner (max count, ties → earliest input) and the kept count (sum of
  the winning counts) are derived from the maps, so the reconstructed header
  (all source header lines except `element vertex`, rewritten with the final
  count) can be written before any record. Merging phase: for each input in list
  order, its winning records are copied byte-exact. Progress: `1 Mapping` and
  `2 Merging` phases per the "Progress" section.
- `merge_scale`: single pass. Counts — then writes — the kept records: file 1
  kept on max world scale `< threshold`, file 2 kept on `>= threshold`, file 1's
  splats followed by file 2's. Errors `No scale in Input 1/2` when a present
  source declares no scale property. Progress: only the `3 Scale` phase.
- `Progress`: per-phase progress state (weights, per-file totals and processed
  counters). `advance(file, n)` records splats and prints a line for each
  crossed million counter boundary; `finish()` prints the final 100% line with
  the phase's total splats unless a 100% line was already printed. Line format
  per the "Progress" section.
- `UserAborted` (merge.rs): error type returned when the cancel flag is set; message
  is `Aborted by user`.
- `merge_voxel` / `merge_scale`: take the cancel flag and check it at each record in
  every loop; stop with `UserAborted` and write no further output (see "Pipeline" —
  Cancellation).
- `pipeline::run`: CLI entry — locks stdout and delegates to `pipeline::run_with`
  with a never-set cancel flag.
- `pipeline::run_with`: orchestrates per the "Pipeline" section — parses the input
  list, maps each file (`-` = empty), validates, requires equal strides across
  all non-empty files, runs the single voxel step (directly to the output, or to
  `temp1` then re-map when the scale step follows), then the scale step with
  `--base`, or the single-input passthrough (no progress). Progress lines and the
  final `Success:` line go to the given sink; on `UserAborted` the partially written
  output file is removed (temp files always are) and the documented abort line is
  written to the sink.

## Required test cases
Status: implemented as of 2026-10-25 — merge-level cases in `src/merge.rs`,
progress math in `src/progress.rs`, pipeline-level cases in `src/pipeline.rs`,
GUI helper cases in `src/gui.rs`.

- Empty input via `-` (either side; all-`-` data sources return the documented error)
- Voxel tie (equal counts per voxel): the earliest input in the list keeps its splats
- Voxel with 3 inputs: per-voxel winner is the earliest input at the max count;
  3+ inputs merge in a single run with kept splats in input-list order
- Voxel keys for negative coordinates
- Threshold boundary: `<` goes to file 1, `>=` to file 2
- Output header vertex count equals the number of written splats
- Kept splat records are byte-identical to the input
- Malformed input (truncated data, `len % stride != 0`, missing scale property in the
  scale step) returns an error
- Single input without `--base` yields a byte-identical passthrough copy
- 2+ inputs merge in a single voxel step, followed (with `--base`) by the scale step
- Temp files do not exist after the run (on success or on error)
- Mismatched stride between non-empty files returns an error
- Progress: a per-million line on each counter crossing; each phase ends with exactly
  one 100% line (no duplicate when the last boundary line already printed 100%)
- Progress: weighted sub-progress matches the worked example (1M of 5M splats in a
  10%-weighted file → 2%)
- Scale step reports only `3 Scale` (no `1 Mapping` lines); passthrough prints no
  progress lines
- Abort: `merge_voxel` and `merge_scale` error with `Aborted by user` and write no
  output bytes when the cancel flag is pre-set
- Abort: a canceled pipeline run (two inputs + base) leaves neither `temp1` nor
  `temp2` nor the partial output file behind, and reports the documented abort line
  on the sink
- GUI: `is_ply` accepts a `.ply`/`.PLY` name and rejects other or missing
  extensions; `ChanWriter` forwards only complete lines, in order, keeping a
  trailing partial line until `flush`

## Open questions
- README calls the threshold "1cm³ (0.01)", but the described comparison uses max axis
  length, not volume. Confirm intended semantics and fix the wording.
- "Keep only the small-scale splats of a single file" (a scale step with an empty
  file 2) cannot currently be expressed: `--base -` equals no base, so a single
  input passes through. Decide whether `--base -` should mean "scale step with an
  empty file 2" (keep only file 1's splats under the threshold).

## Changelog

- 2026-10-25 Progress label: the scale step's merge phase is now reported as
  `3 Scale` (previously `2 Merging`, which collided with the voxel step's
  `2 Merging` phase); the voxel step's labels are unchanged.
- 2026-10-25 GUI theme and sizing (follow-up to task 3): the input list now
  has a static height for 7 rows; the window uses a dark theme, a visible 1px
  border around all interactive widgets, and 8px spacing between elements.
  Appearance only, no behavior changes.
- 2026-10-25 GUI layout fix (follow-up to task 3): the input list is now full
  width, one entry per line, with Add/Remove and the voxel-size chooser on a
  row below it; the base and output fields expand to the remaining row width
  with the "Browse" button and the scale-threshold chooser right-aligned at
  the end of the row; the log area fills all remaining window height (new
  lines stick to the bottom); the Run/Abort button moved to the bottom-right
  of the window. Layout only, no behavior changes.
- 2026-10-25 GUI (task 3): the binary now doubles as a GUI — without arguments it
  launches an `eframe` 0.37 / `egui` window (native dialogs via `rfd` 0.17) with the
  "GUI" section layout (input list + Add/Remove, base + threshold, output, log area,
  Run); drag-and-drop and `.ply`-only file rules; while running, controls are
  disabled and the pipeline's stdout lines stream into the log area; a confirmed
  Abort sets a shared cancel flag checked per record in every merge loop
  (`UserAborted`: "Aborted by user") and the pipeline removes the temp files plus
  the partial output file and reports the abort line; the GUI resets to a
  re-runnable state; the CLI (with arguments) is unchanged.
- 2026-10-25 Docs fix: `--base -` documented as equal to omitting `--base` (no base;
  passthrough for a single input) — the earlier "standalone filter" / "keep only the
  small-scale splats" examples were never implemented; the typical-workflow example
  was replaced with the passthrough; README updated to the single-pass voxel merge,
  the current CLI, and the progress lines.
- 2026-10-25 Voxel efficiency (task 2): all voxel inputs now merge in a single run
  with one hashmap per non-empty input (winner = earliest input at the max count;
  kept splats in input-list order, byte-identical to the old chain); pairwise chain
  removed and the voxel step writes to the output (with base: only
  `<output-stem>.temp1.ply`); added `src/progress.rs` and progress lines on stdout —
  `1 Mapping` (voxel step) + `2 Merging` (both steps), per-million status lines, one
  final 100% line per phase, weights = file byte sizes; the scale step reports only
  `2 Merging` and the passthrough prints no progress.
- 2026-10-24 CLI redesign (task 1): `-i/--inputs` takes a comma-separated input list
  (replaces `-i/-j`); `--mode` removed; `--base` added, driving the final scale step;
  voxel chain over the input list with `<output-stem>.temp1.ply`/`.temp2.ply` temp
  files (always removed); single input without `--base` is a passthrough copy;
  all-`-` and stride-mismatch inputs are errors; `--threshold` default is now `0.01`;
  added `validate` (truncated / non-divisible data).
- 2026-10-24 Code split: `src/main.rs` refactored into `main.rs` (CLI), `ply.rs`
  (format), `merge.rs` (pairwise merge), `pipeline.rs` (run orchestration); behavior unchanged.
- 2026-10-24 Docs sync: completed "Architecture"; added CLI short flags and defaults
  (`-m` default `voxel`, `-v` default `0.5`, `-t` default `0.1`); recorded voxel-tie
  behavior (file 1 kept), both-`-` input error, and header re-emission rule; marked the
  "Required test cases" list as not yet implemented; noted the 0.1 vs 0.01 default
  question.
- 2026-10-24 Initial documentation.