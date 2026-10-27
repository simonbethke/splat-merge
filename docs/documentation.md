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
- The cell axes are selectable (GUI only — the "Voxels: X, Y, Z" checkboxes,
  see "GUI"; all on by default): an unchecked axis is not part of the cell
  key, so only the checked axes define the cells (e.g. X and Z only means the
  cells are extruded along Y). With one axis only, the cells span all space
  orthogonal to it. The CLI has no flag for this and always uses all three axes.
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
  reports only the `3 Cleaning (scale based)` task (no mapping phase, see "Progress").

### Null input
Filename `-` in any input slot (input list entry or `--base`) means an empty file.
This allows empty slots in the input list. `--base -` is equivalent to omitting
`--base` (a single input then passes through; with several inputs the output is
the voxel-merged list alone). If every data source (all inputs, and the base if
given) is `-`, the run is rejected with an error (no file to supply stride and header).

## Binary format rules (must not break)
- PLY data is binary little-endian with a fixed per-splat stride
  (position, SH, opacity, scale, rotation).
- `x`, `y`, `z` must be the first three properties of every non-empty data
  source; the pipeline reads the position from the record start (any other
  order is an error before the run starts).
- The output layout is the common layout: the properties present in every
  non-empty data source (inputs + `--base`), in the order of the first such
  file. Properties present in only some files are dropped from the output
  (no zero-fill); the drop is announced (see "CLI" and "GUI" — Run). A shared
  property declared with a different byte size (PLY type) in different files
  is an error naming the property (see "Pipeline" — Errors).
- Kept splat records keep their property values byte-exact: with equal input
  layouts the full record is copied as-is; with mixed layouts each kept
  property is copied from its per-source offset to its output offset (the
  same property may sit at different positions in different files).
- Output header is reconstructed from the first non-empty file's header: all
  lines are emitted unchanged except `element vertex <count>` (rewritten with
  the final count) and the `property` lines of dropped properties (removed);
  a single-input passthrough emits the source header as-is.
- Scale properties are stored as log values. World-space scale = `exp(scale_n)`;
  "max scale" = maximum over the three axes.
- Inputs are never loaded fully into RAM: mmap for reading, buffered writer for output.

## Invocation
One executable, three modes:
- **No arguments**: launches the GUI window (see "GUI").
- **`--debug-render <state.json>`**: debug-render mode — renders the named GUI
  state to `screenshot.jpg` and exits (see "Debug rendering").
- **Any other arguments**: CLI mode with the flags below.

Windows: the binary is linked with the GUI subsystem
(`#![windows_subsystem = "windows"]`), so double-clicking it (no arguments)
opens only the window, no console. CLI mode and `--debug-render` require an
existing console (e.g. cmd / PowerShell); running them without one is an error,
which is not reachable by double-clicking.

## CLI
| Flag | Meaning |
|---|---|
| `-i, --inputs <a,b,c>` | Comma-separated list of input PLY files; an entry may be `-` (empty) |
| `--base <file>` | Optional base PLY (file 2 of the scale step); omitted or `-` = no base (no scale step) |
| `-o, --output <file>` | Output PLY |
| `-v, --voxel-size <f>` | Cube edge length for the voxel step (default: `0.5`) |
| `-t, --threshold <f>` | Max-scale threshold for the scale step (default: `0.01`) |
| `--debug-render <file>` | Debug-render mode: render a documented GUI state to `screenshot.jpg` and exit (see "Debug rendering"); conflicts with all pipeline flags |

Input list entries are trimmed of surrounding whitespace; an empty entry (e.g. `a,,b`)
is an error. `--mode` no longer exists: the voxel step always runs on the input list, and
the scale step runs when `--base` is a real file (`--base -` = no base; see "Pipeline").
The voxel-axis selection is GUI-only (see "GUI"); CLI runs always merge on all
three axes.
Mixed property layouts (the non-empty data sources declare different
`property` sets) are merged by dropping the properties present in only some
files; there is no new flag and no confirmation — exactly one notification
line is written to stdout at run start (see "Pipeline" — Errors).

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
1. Input file list: full width, one entry per line, filling all remaining
   window height (it scrolls if the entries do not fit). The list itself is a
   visible rounded box with the theme's 1px border, inset 16px from the
   window edges. When the list is empty, a hint line `No input files. Use Add,
or drag .ply files onto the window.` is shown at the top of the box.
    Below it a control row — "Add" (multi-select picker) and
   "Remove" (enabled only while a list entry is selected) on the left.
   Below that a second control row holds the voxel settings: the
   voxel-axis checkboxes "Voxels: X", "Y", "Z" (all checked by default;
they select which axes define the voxel cells — an unchecked axis is ignored)
   on the left, and the "Voxel size:" label with its chooser
   right-aligned at the end of the row.
2. Base-file field (single-file picker via a "Browse" button; an empty base
   means no base, like omitting `--base`). The field expands to the remaining
   width of the row; the "Browse" button and the scale-threshold chooser are
   right-aligned at the end of the row.
3. Output-file chooser (save dialog). The field expands to the remaining
   width of the row; the "Browse" button is right-aligned at the end of the
   row.
4. Progress table: it spans the full window width in three equal-width
   columns. Two rows — a header row with the column names "Mapping",
   "Merging" and "Cleaning" (larger, centered), and a row of three progress
   bars, one per column. Each bar is a full-width track with the theme's
   1px border and a left-anchored accent fill sized to the value; the
   percentage is painted centered over the track. The bars are 0% before a
   run, move as progress lines arrive, and keep their last value after the
   run ends.
5. A prominent filled Run button (accent blue — the app's primary action,
   larger than the other buttons) at the very bottom of the window, aligned
   to the right. While a run is active it reads "Abort" in a danger red.

Theme and spacing:
- Dark theme, applied once at startup (independent of the system theme).
- Every interactive widget - the file list rows, the Base and Output text
  inputs, the voxel-axis checkboxes, all buttons and the choosers - gets a
  visible 1px border that defines its area.
- The Run button is filled with an accent blue (the app's primary action) and
  the progress-bar fills use the same accent; while a run is active the
  "Abort" button is filled with a danger red.
- 8px spacing between all elements.
- The content is padded against the window edges with a 16px inset on all
  sides.
- The egui panels draw no separator lines; the gaps between elements are the
  margins above, so no extra line touches the Run button or the list box.

Files:
- Chosen with a file chooser or by dragging them onto the window.
- `*.ply` only (case-insensitive extension); other files are rejected with a
  message dialog. A path already in the list is not added twice (a message
  dialog says it is already there).

Run:
- Validation before start: at least one input, a `.ply` output path, voxel size > 0,
  threshold >= 0, and at least one voxel axis (X, Y or Z) checked. On failure the
  reason is shown in a message dialog and nothing starts.
- Layout check before start: the property sets of the non-empty data sources
  (inputs + base) are compared; each file is only opened and memory-mapped
  read-only to parse its ASCII header (nothing else is read or written), so
  an unreadable file or a missing `end_header` is an error dialog too. Then:
  - a shared property with conflicting PLY types, or `x`, `y`, `z` not the
    first three properties of a non-empty file: error dialog with the message
    (see "Pipeline" — Errors); the run does not start;
  - properties present in only some files: a native Ok/Cancel dialog with the
    question `Some properties are not present in every input file and will be
    dropped from the output:` plus the dropped names (comma-joined); Ok =
    start the run, Cancel = nothing starts and the controls stay enabled;
  - equal layouts: the run starts as today.
  The confirmation dialog is reachable only interactively: `--debug-render`
  never starts a run, so a screenshot can show the run state (progress bars,
  button) but never the dialog itself.
- While a run is active, the controls above are disabled and the pipeline's
  lines (the same lines the CLI would print on stdout) are interpreted:
  progress lines move the bar of their table column, and every other line is
  shown as a native message dialog — the `Success:` line as information, the
  abort line as a warning, everything else as an error. The drop-notification
  line is never shown as a dialog: the user already confirmed the drops in
  the pre-start Ok/Cancel dialog.
- The Run button becomes "Abort". Clicking it opens a native are-you-sure dialog;
  on confirmation the cancel flag is set, the run stops at the next record
  boundary, and the temp files plus the partially written output file are removed
  (see "Pipeline" — Cancellation).
- After an abort, a failure, or a success the controls are re-enabled and the
  settings are preserved, so a rerun is immediate.
- The run log (see "Run log") is written exactly as in the CLI: `splat-merge.log`
  beside the executable.

## Debug rendering
For scriptable, deterministic GUI verification (see the "GUI verification"
agent rule): `--debug-render <state.json>` loads a documented JSON state object,
renders the window (per "GUI") once at the state's size in the same dark theme,
saves `screenshot.jpg` in the current working directory (overwritten on each
run, JPEG quality 90), and exits. It never starts the pipeline, never writes the
run log, and never opens native dialogs.

On success it prints `Saved screenshot.jpg (<w>x<h>)` to stdout and exits 0.
Any failure (missing/unreadable state file, bad JSON, or a rule violation below)
prints one clear error line to stderr naming the offending field and value, and
exits non-zero.

State object (a top-level JSON object; exactly this set of fields):

| Field | Type | Optional | Default | Meaning |
|---|---|---|---|---|
| `version` | integer | required | must be `1` | Format version; any other value is an error |
| `window` | object `{"width": int, "height": int}` | yes | `1000`, `700` | Render size in pixels; each value must be `>= 100` |
| `inputs` | array of strings | yes | `[]` | Input list entries, in order (any string; `.ply` is not enforced in debug mode) |
| `selected_input` | integer or `null` | yes | `null` | 0-based index into `inputs`; when set, must be in range; renders the entry selected and "Remove" enabled |
| `voxel_axes` | object `{"x": bool, "y": bool, "z": bool}` | yes | all `true` | The three voxel-axis checkboxes |
| `voxel_size` | number | yes | `0.5` | Voxel-size chooser value; must be `> 0` |
| `base` | string | yes | `""` | Base-file field text |
| `output` | string | yes | `""` | Output-file field text |
| `progress` | object `{"mapping": int, "merging": int, "cleaning": int}` | yes | all `0` | The three progress-bar percentages, each `0`–`100` |
| `running` | bool | yes | `false` | `true` renders the running state: controls disabled and the Run button shows "Abort" |

Rules:
- Unknown fields at any level are rejected with an error naming the offending
  field, so a bad state file fails fast instead of silently rendering the wrong
  state.
- Wrong types and out-of-range values are errors with clear messages. Integer
  fields accept integral numbers only; a non-integral number is a type error.
- Message dialogs, drag-and-drop, and in-run pipeline behavior are out of scope
  for the state object; rendering is a single static frame.

Example state file (`state.json`):
```json
{
  "version": 1,
  "window": { "width": 1000, "height": 700 },
  "inputs": ["detail_a.ply", "detail_b.ply"],
  "selected_input": 0,
  "voxel_axes": { "x": true, "y": true, "z": false },
  "voxel_size": 0.5,
  "base": "base.ply",
  "output": "merged.ply",
  "progress": { "mapping": 100, "merging": 20, "cleaning": 0 },
  "running": false
}
```
Run it with `cargo run --release -- --debug-render state.json` and compare the
resulting `screenshot.jpg` against the "GUI" section.

The GUI screenshots committed for the README live in `docs/screenshots/`: one
state file per image (`gui-idle.json`, `gui-configured.json`,
`gui-running.json`) plus the rendered `gui-<name>.jpg`. Regenerate any image
with `cargo run --release -- --debug-render docs/screenshots/gui-<name>.json`
(from the repository root) and copy `screenshot.jpg` over
`docs/screenshots/gui-<name>.jpg`.

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
- Empty input-list entry, or all data sources `-`: error (nothing to read);
  fires before the output file is created.
- A shared property declared with a different byte size (PLY type) in
  different non-empty files: error before the output file is created —
  `Property '<name>' has conflicting PLY types between files (<type A> vs <type B>)`.
- `x`, `y`, `z` not the first three properties of a non-empty data source:
  error before the output file is created (the pipeline reads the position
  from the record start).
- 2+ inputs with no voxel axis enabled (the GUI checkboxes; the CLI always
  has all) — guard for the voxel step: error
  `No voxel axis enabled (at least one of X, Y, Z required)`.
- Properties present in only some non-empty files are **not** an error:
  exactly one notification line is written to the output sink at run start,
  before any progress line, with the names in first-input order —
  `Dropped properties not present in every file: <n1>, <n2>, …` — it is
  sink-only (in the GUI it appears as an information dialog during the run,
  see "GUI" — Run; it never reaches the run log), and the run proceeds. A
  scale step after such a merge still needs scale properties: when a present
  source has none, it errors with `No scale in Input 1` / `No scale in Input 2`
  before writing any record (temp files are still removed).

Cancellation (GUI abort; not reachable from the CLI):
- The merge loops check a shared cancel flag at each record; when it is set the
  run stops with the error `Aborted by user` and writes no further output.
- The temp files are removed as on any finished run; additionally the partially
  written output file is removed and the line
  `Aborted; temporary and partial output files removed.` is written to the output
  sink (stdout in the CLI, a message dialog in the GUI).
- Other (non-abort) errors do not remove the output file.

## Progress
Progress is written to the run's output sink (stdout in the CLI; in the GUI
the lines drive the progress table and message dialogs, see "GUI"), one set
of lines per phase. A status line is
printed whenever the phase's cumulative splat counter crosses another million
(triggered at each multiple of 1,000,000), and each phase closes with a final
100% line.

Tasks:
- `1 Mapping` (voxel step only): the per-input density hashmaps are filled.
- `2 Merging (voxel based)` (voxel step only): the splats are run through, selected, and written.
- `3 Cleaning (scale based)` (scale step only): the splats are run through, selected, and written.
The passthrough reports no progress.

Weighting: each file of a phase contributes to the 100% proportionally to its
byte size on disk (the mapped file size); sub-progress within a file is
`splats processed / total splats in that file` (a zero-splat file counts as
fully processed). Overall percentage = the sum over files of
`weight x sub-progress`, clamped to 0–100.
Worked example: two files, 1 GB and 9 GB (weights 10% and 90%), the first with
5 000 000 splats. At 1 000 000 of file 1's splats processed:
`10% x (1M / 5M) = 2%` → `1 Mapping: 2% (1.0M splats)`.

Line format: `<task>: <pct>% (<value>M splats)`, e.g. `2 Merging (voxel based): 43% (12.3M splats)`.
`<pct>` is an integer (rounded); `<value>` is the crossed million boundary
(per-million lines) or the phase's total splats (final 100% line), in millions
with one decimal. I/O failures on progress lines are ignored.

## Run log
Every run (CLI and GUI) writes a plain-text log file beside the executable,
named `splat-merge.log` (e.g. `C:\tools\splat-merge\splat-merge.exe` logs to
`C:\tools\splat-merge\splat-merge.log`). It is enabled always — no flag, no
GUI control — truncated at the start of every run, and kept after a failure
or an abort (the temp files and the partial output are removed, the log is
not). If the output path names the same file as the log (case-insensitive),
no log is written. A failure to locate the executable or to create the log
file is a run error; write failures on the already-open log are ignored.

Every line begins with the UTC time the line was emitted, with millisecond
precision: `[YYYY-MM-DD HH:MM:SS.mmm] <message>`. Lines are flushed as soon
as they are complete, so a crashed or aborted run leaves a complete record of
everything that happened before it.

The log does not contain progress lines — for each phase it carries only the
phase start and finish detail lines — and it does not contain the `Success:`,
drop-notification, or abort sink lines (the log has its own terminal lines
instead). The detail
lines below go to the log only; they never reach the CLI stdout or the GUI
message dialogs.

Line sequence (in order):
- `Run started (inputs: <list>; base: <name or "-">; output: <path>; voxel size: <v>; voxel axes: <axes>; threshold: <t>)`,
  where `<axes>` is the compact enabled-axis set in XYZ order (`XYZ` when all
  axes are enabled, e.g. `XZ` when only X and Z).
- One line per input slot: `Input <n>: <name> (<size>, <count> splats)`;
  an empty slot (`-`) reads `Input <n>: - (empty)`.
- If a base was given: `Base: <name> (<size>, <count> splats)` (or
  `Base: - (empty)`).
- Voxel step: `Voxel step started (<n> inputs)`, then per phase
  `<phase> started` and `<phase> finished after <dur>` (no progress lines);
  the second phase's finished line carries
  the kept count: `2 Merging (voxel based) finished after <dur> (<kept> kept)`.
- Scale step: `Scale step started (file 1: <n> splats; file 2: <n> splats; threshold <t>)`,
  then `3 Cleaning (scale based): counting kept splats` (before the count
  pass), `3 Cleaning (scale based) started (<kept> of <total> splats kept)`
  (after it), and `3 Cleaning (scale based) finished after <dur>` (no
  progress lines).
- Passthrough: `Passthrough started` … `Passthrough finished after <dur>`.
- Terminal line: `Run finished: <count> splats written to <path> (<size>) in <dur>`,
  or `Run aborted after <dur>`, or `Run failed after <dur>: <error>`.

Formatting: sizes are base-1024 with one decimal (`B`, `KB`, `MB`, `GB`,
`TB`); splat counts below 1000 are plain, then thousands/millions/billions
with one decimal (`1.5k`, `12.3M`, `2.0B`); durations are `<n>s`,
`<n>m <nn>s`, `<n>h <nn>m <nn>s`.

## Architecture
Single binary (CLI, GUI, or debug rendering — see "Invocation"). Dependencies:
`clap` (CLI), `memmap2` (input mapping), `hashbrown` (voxel density maps),
`eframe` 0.37 / `egui` (GUI window), `rfd` 0.17 (native file dialogs in the
GUI), `image` (JPEG encoding for debug rendering only — never in the pipeline).
Test-only dev-dependency: `winit` 0.30 (Windows) — the e2e debug-rendering
test sets `with_any_thread` on the event-loop builder so the window can be
created on the test worker thread.

Source layout:
- `src/main.rs`: `Cli` — the clap parser for the pipeline flags (see "CLI")
  plus `--debug-render <state.json>` (see "Debug rendering"); combining
  `--debug-render` with any pipeline flag is a parse error
  (clap `conflicts_with`); `Args` — the plain settings struct the pipeline
  and the GUI's run start read; `VoxelAxes` — the voxel-axis mask (X/Y/Z bits,
  default all axes on; not a CLI flag, set from the GUI checkboxes, and the
  default in the CLI); `main` dispatches: no arguments → the GUI (see
  "Invocation"), `--debug-render <file>` → debug rendering (see "Debug
  rendering"), otherwise builds the `Args` and calls `pipeline::run`.
- `src/ply.rs`: `PlyInfo` (the parsed property list — name, type name, size,
  record offset — plus stride, vertex count, raw header, `scale_0` offset) +
  `parse_ply_header`, `validate`, `get_pos`, `get_max_scale`, `common_layout`
  (the shared layout helper: the common layout in first-file order, the
  dropped names, and the all-empty / `x,y,z`-first / type-conflict errors),
  and `rewrite_header` (reconstructed output header, optionally with the
  non-common `property` lines removed).
- `src/merge.rs`: `merge_voxel` (all non-empty inputs in one run: one density map per
  input, per-voxel earliest-input-at-max wins, kept splats in input-list order) and
  `merge_scale` (single pass, threshold keep). Both write the reconstructed header
  (with the `property` lines filtered to the common layout) then the kept records
  with their values byte-exact — full-record copy for equal layouts, per-property
  remap to the common offsets for mixed ones, no per-record allocation — checking
  the given cancel flag at each record
  (`UserAborted` error — see "Pipeline" — Cancellation). Operate on byte slices, not
  files; emit progress through the given progress writer, and phase start/finish
  detail lines to the given log sink (see "Run log").
- `src/progress.rs`: `Progress` — file-weighted phase progress (per-million status
  lines, final 100% line); weights = byte-size shares of the phase's files; each
  emitted line is written to the given sink only (progress lines never reach
  the run log — see "Run log").
- `src/logfile.rs`: `LogFile` — a `Write` sink that prefixes every complete line
  with a UTC `[YYYY-MM-DD HH:MM:SS.mmm]` timestamp and flushes per line, including an
  unterminated final line on drop; the log formatters `format_utc`, `format_bytes`,
  `format_splats`, `format_duration` (see "Run log").
- `src/gui.rs`: `eframe`/`egui` window per the "GUI" section (`launch` is the GUI
  entry point). `GuiState` — the plain settings struct (inputs, selection, voxel
  axes, voxel size, threshold, base, output, progress, running) the window
  renders from; its `Default` equals the GUI's startup defaults, and the same
  struct drives debug rendering (a superset: `threshold` is in the window but
  not in the state object). `is_ply` extension check; `add_input` (`.ply` + duplicate rejection,
  returning the rejection message for a dialog); `ChanWriter` (an `io::Write` sink
  forwarding complete lines over an mpsc channel); `parse_progress_line`
  (recognizes the three progress lines, yielding column + percentage; any other
  line becomes a message dialog); `message_level` (the dialog level: `Success:` and the drop-notification line as info, the abort line as a warning, `Run failed` and everything else as an error); `probe_header` (maps one PLY read-only and parses its ASCII header); `layout_check` (before start, compares the non-empty sources' property sets to equal / would-be-dropped / conflict, the conflict naming the property and both types or the invalid `x`,`y`,`z` order); `start_run_error` (validates the settings, returning the first failed rule — including at least one
  checked voxel axis — without starting anything, so it is unit-testable) and `start_run` (runs the pre-start layout check first: a conflict or invalid `x`,`y`,`z` order fails with the error dialog and no start, a non-empty drop list asks the native Ok/Cancel confirmation where Cancel does not start the run; then calls it,
  then starts the worker thread with a shared cancel flag); the abort
  confirmation dialog and the per-line message dialogs.
- `src/debug_render.rs`: debug rendering (see "Debug rendering"). Parses the
  state file — hand-written recursive-descent JSON, validated strictly against
  the documented schema (unknown fields rejected naming the field, wrong types
  and out-of-range values are errors) — into `gui::GuiState`. Renders one real
  `eframe` window with that state (egui 0.37 has no headless pixel API) and
  requests the painted frame's pixels through egui's screenshot callback
  (`Context::request_screenshot`, delivered asynchronously on a wgpu thread and
  silently dropped on GPU error), self-correcting the pixel size to the
  requested `width x height` via `ViewportCommand::InnerSize`. Encodes the
  captured pixels as RGB8 JPEG (quality 90) with `image` into `screenshot.jpg`
  in the current working directory (overwritten), then closes the window. A
  bounded deadline turns a dropped callback into a timeout error instead of a
  hang. On success prints `Saved screenshot.jpg (<w>x<h>)` to stdout and exits
  0; any failure prints one error line to stderr and exits non-zero.
- `src/pipeline.rs`: input-list parsing, file mapping/validation, common-layout
  resolution (intersection of the non-empty sources' properties in first-file
  order, with the conflict / `x,y,z`-first / all-`-` checks and the one
  drop-notification line on the sink when the layout shrinks),
  single voxel step (to the output, or to temp1 then re-map when a scale step
  follows), scale step, passthrough, cleanup. `run` (CLI) locks stdout and delegates
  to `run_with`, which also serves the GUI worker; progress lines and the final
  `Success:` line go to the given sink; the run log file is created at run
  start as `splat-merge.log` beside the executable (skipped when it is the
  same file as the output) and receives the
  `Run started`, input/base, step/phase detail and terminal lines (see "Run log");
  it is kept after a failure or an abort; a canceled run additionally removes the
  partial output (see "Pipeline" — Cancellation).

Responsibilities by item:
- `Cli` (main.rs): the clap command line (flags in the "CLI" section);
  `--debug-render <file>` is its own flag, conflicting with every pipeline
  flag (see "Debug rendering") — the pipeline flags are required only when
  it is absent.
- `Args` (main.rs): the plain pipeline-settings struct the pipeline (and the
  GUI's run start) read; `voxel_axes` carries the `VoxelAxes` mask for the run.
- `VoxelAxes`: the enabled voxel-axis set as a bit mask (`X` = 1, `Y` = 2,
  `Z` = 4); `Default` is all three; `any()` reports whether any axis is
  enabled and `label()` renders the compact enabled set (`XYZ`, `XZ`, `Y`, …)
  used by the `Run started` log line and the GUI.
- `GuiState` (gui.rs): the settings struct the GUI renders from; `Default`
  equals the GUI's startup defaults (all axes on, voxel size 0.5, threshold
  0.01, empty inputs/base/output, zero progress, not running); its fields map
  1:1 onto the window's widgets, and it is also the target struct of the
  `--debug-render` state object (a superset — `threshold` is not in the JSON).
- `PlyInfo` + `parse_ply_header`: scans the ASCII header; collects the ordered
  property list (name, type name, size, offset within the record; property
  lines with an unrecognized type are skipped, as before) and computes the
  per-splat byte stride from it (`float` = 4, `double` = 8, `uchar`/`uint8` = 1
  byte, …); stores the vertex count, records the byte offset of `scale_0`, and
  keeps the raw header used to re-emit the output header.
- `validate`: rejects non-empty files whose binary data length is not a multiple of the
  stride or is truncated below the declared record count.
- `get_pos(record)`: decodes the first 12 bytes of a record as the little-endian f32
  position (assumes position is the first property; used for voxel keys).
- `get_max_scale(record, offset)`: decodes `scale_0..2` and returns the max of the three
  log values, exponentiated to world space.
- `common_layout(infos)`: the shared layout of the non-empty data sources: the
  properties present in every one, in the first non-empty source's order, each
  as `(name, type_name, size)` plus the per-source `src_offset` and the output
  `out_offset` (from the first source's offsets); also returns the dropped
  names in first-source order for the notification/confirmation. Errors before
  the run: all sources `-`, a source not starting with `x`,`y`,`z`, or a shared
  name with a different byte size (naming the property and both types).
- `rewrite_header(writer, template, count, keep)`: re-emits the template
  header unchanged except `element vertex <count>` and the `property` lines
  whose names are not in `keep` (passthrough keeps all names).
- `merge_voxel`: merges all non-empty inputs in one run into any `Write` target
  (temp or output). Mapping phase: one hashbrown map per input counts per-voxel
  density, keyed by `(x, y, z)` i32 voxel indices (`floor(p / voxel_size)`),
  with a disabled axis's component forced to 0 (see "Modes" — voxel); the
  all-axes case is unchanged. The
  per-voxel winner (max count, ties → earliest input) and the kept count (sum of
  the winning counts) are derived from the maps, so the output header can be
  written before any record: `rewrite_header` re-emits the first non-empty
  source's header with the final count on `element vertex` and drops the
  `property` lines whose names are not in the common layout (none dropped when
  the layouts are equal). Merging phase: for each input in list order, its
  winning records are written with the copy rule (a full-record copy when the
  source's layout equals the common layout, otherwise a per-property remap).
  Progress: `1 Mapping` and
  `2 Merging (voxel based)` phases per the "Progress" section; the phase
  start/finish detail lines go to the given log sink (see "Run log").
- `merge_scale`: single pass. Counts — then writes — the kept records: file 1
  kept on max world scale `< threshold`, file 2 kept on `>= threshold`, file 1's
  splats followed by file 2's, each record copied with the same rules as
  `merge_voxel`. Errors `No scale in Input 1/2` when a present
  source declares no scale property. Progress: only the `3 Cleaning (scale based)`
  phase; the phase detail lines (including `counting kept splats`) go to the given
  log sink (see "Run log").
- `Progress`: per-phase progress state (weights, per-file totals and processed
  counters). `advance(file, n, sink)` records splats and writes a line for
  each crossed million counter boundary; `finish(sink)` writes the final 100%
  line with the phase's total splats unless a 100% line was already printed. Every
  emitted line goes to the given sink only (progress lines never reach the run
  log — see "Run log"). Line format per the "Progress" section.
- `UserAborted` (merge.rs): error type returned when the cancel flag is set; message
  is `Aborted by user`.
- `merge_voxel` / `merge_scale`: take the cancel flag and check it at each record in
  every loop; stop with `UserAborted` and write no further output (see "Pipeline" —
  Cancellation).
- `pipeline::run`: CLI entry — locks stdout and delegates to `pipeline::run_with`
  with a never-set cancel flag.
- `pipeline::run_with`: orchestrates per the "Pipeline" section — parses the input
  list, maps each file (`-` = empty), validates, resolves the common layout (the `common_layout` entry: all-`-`,
  `x`,`y`,`z`-first and type-conflict are errors before the output file exists;
  a non-empty drop list writes the one notification line to the sink at run
  start), runs the single voxel step (directly to the output, or to
  `temp1` then re-map when the scale step follows), then the scale step with
  `--base`, or the single-input passthrough (no progress). Progress lines and the
  final `Success:` line go to the given sink; on `UserAborted` the partially written
  output file is removed (temp files always are) and the documented abort line is
  written to the sink. The run log file (see "Run log") is created at run
  start as `splat-merge.log` beside the executable (skipped when it is the
  same file as the output), receives the `Run started`, input/base and detail
  lines, and the terminal line
  (`Run finished` / `Run aborted` / `Run failed`); it is kept after failure or
  abort.

## Required test cases
Status: implemented as of 2026-10-25 — merge-level cases in `src/merge.rs`,
progress math in `src/progress.rs`, pipeline-level cases in `src/pipeline.rs`,
GUI helper cases in `src/gui.rs`.

- Empty input via `-` (either side; all-`-` data sources return the documented error)
- Voxel tie (equal counts per voxel): the earliest input in the list keeps its splats
- Voxel with 3 inputs: per-voxel winner is the earliest input at the max count;
  3+ inputs merge in a single run with kept splats in input-list order
- Voxel keys for negative coordinates
- Voxel axis subsets: a disabled axis is excluded from the cell key, so splats
  differing only in that axis share one cell (e.g. Y unchecked → cells keyed by
  X and Z only: splats differing only in Y collapse into one cell, while ones
  differing on X or Z stay separate; the same holds for any enabled subset);
  the all-axes case is byte-identical to the previous behavior
- Threshold boundary: `<` goes to file 1, `>=` to file 2
- Output header vertex count equals the number of written splats
- Kept splat records: each property's value is byte-identical to the input
  (the record may use the reordered common layout)
- Malformed input (truncated data, `len % stride != 0`, missing scale property in the
  scale step) returns an error
- Single input without `--base` yields a byte-identical passthrough copy
- 2+ inputs merge in a single voxel step, followed (with `--base`) by the scale step
- Temp files do not exist after the run (on success or on error)
- Mixed property layouts: the output carries exactly the properties present in
  every non-empty data source (inputs + `--base`), in the first non-empty
  input's order, with each property's value byte-identical to the source; a
  shared property sitting at different positions in different files is remapped
  per-property into the common layout
- Mixed property layouts: exactly one notification line naming the dropped
  properties, written to the sink before `Success:`, absent from the run log,
  and absent when all non-empty files share the same layout
- Mixed property layouts: a shared property declared with a different PLY type
  (byte size) in different files is an error before the run starts, naming the
  property and both types; no output file is left behind
- Mixed property layouts: a non-empty source whose first three properties are
  not `x`, `y`, `z` is an error before the run starts
- `--base` participates in the intersection: a property present in all inputs
  but missing in the `--base` file is dropped from the output; when the base
  drops a scale property the scale step fails with the documented
  `No scale in Input 1/2` error and the temp files are still removed
- Progress: a per-million line on each counter crossing; each phase ends with exactly
  one 100% line (no duplicate when the last boundary line already printed 100%)
- Progress: weighted sub-progress matches the worked example (1M of 5M splats in a
  10%-weighted file → 2%)
- Scale step reports only `3 Cleaning (scale based)` (no `1 Mapping` lines);
  passthrough prints no progress lines
- Abort: `merge_voxel` and `merge_scale` error with `Aborted by user` and write no
  output bytes when the cancel flag is pre-set
- Abort: a canceled pipeline run (two inputs + base) leaves neither `temp1` nor
  `temp2` nor the partial output file behind, and reports the documented abort line
  on the sink
- GUI: `is_ply` accepts a `.ply`/`.PLY` name and rejects other or missing
  extensions; `ChanWriter` forwards only complete lines, in order, keeping a
  trailing partial line until `flush`
- GUI: `start_run` is rejected with the documented message when no voxel axis
  is checked (and accepts the run when at least one is)
- No voxel axis enabled with 2+ inputs: the run errors before the output file
  is created, and the run log carries the documented `Run failed` line
- GUI: `parse_progress_line` maps the three progress line formats to
  (column, percentage) and returns `None` for `Success:` / abort / other lines
- Run log: a successful run writes `splat-merge.log` beside the executable with the
  `Run started` line, one line per input slot (and the `Base:` line when given),
  and a `Run finished` terminal line; every line carries a
  `[YYYY-MM-DD HH:MM:SS.mmm]` prefix; the log contains no progress lines
- Run log: when the output path is the same file as the log (case-insensitive),
  no log is written and the output is still written normally
- Run log: the log file is kept after a failed run (with a `Run failed` terminal
  line) and after an aborted run (with a `Run aborted` terminal line)
- Run log: a voxel-plus-scale run logs the step/phase lines in the documented
  order, including `Voxel step started`, the `1 Mapping` /
  `2 Merging (voxel based)` phase pairs, `Scale step started`,
  `counting kept splats`, and `started (<kept> of <total> splats kept)`
- Progress: every emitted line is written to the given sink only (progress
  lines never reach the run log)
- Run log: `LogFile` emits complete lines as they arrive (a trailing partial line
  is written only on drop, stamped); `format_utc`, `format_bytes`, `format_splats`
  and `format_duration` produce the documented forms
- Debug rendering: a minimal state JSON parses to the documented defaults; all
  fields accepted; `version` != 1 (or missing) rejected; each unknown field,
  each wrong type, and each out-of-range value (progress > 100, negative
  `voxel_size`, window dim < 100, `selected_input` out of range) rejected with
  an error naming the field and value
- Debug rendering: `GuiState` maps to the GUI's settings exactly (checkbox
  states, field texts, bar percentages, running/disabled state)
- Debug rendering: the default state renders to a valid non-empty JPEG at the
  documented default 1000x700; a custom `window` size is honored
- CLI: `--debug-render` combined with any pipeline flag (`-i`/`--base`/`-o`/`-v`/`-t`)
  fails at parse time

## Open questions
- README calls the threshold "1cm³ (0.01)", but the described comparison uses max axis
  length, not volume. Confirm intended semantics and fix the wording.
- "Keep only the small-scale splats of a single file" (a scale step with an empty
  file 2) cannot currently be expressed: `--base -` equals no base, so a single
  input passes through. Decide whether `--base -` should mean "scale step with an
  empty file 2" (keep only file 1's splats under the threshold).

## Changelog

- 2026-10-27 README: restructured for GitHub with three GUI screenshots
  (idle, configured, running) generated from state files committed in
  `docs/screenshots/` (see "Debug rendering" — state files); the "GUI" section
  now also documents the empty-list hint line; documentation only, no behavior
  changes.

- 2026-10-27 Merge of files with differing property layouts (task 6): the
  output keeps the properties present in every non-empty data source (inputs +
  `--base`), in the first non-empty input's order, dropping the rest (one
  notification line on the sink in the CLI; in the GUI a native Ok/Cancel
  confirmation before start, and the run does not re-show the notification
  line as a dialog). A shared
  property with a different PLY type, a source not starting with `x`,`y`,`z`,
  and all-`-` data sources are pre-run errors; equal layouts keep the
  byte-copy fast path and passthrough stays verbatim.
- 2026-10-26 GUI polish: the control row is split into two grouped rows (list
  actions Add/Remove; and the voxel axis checkboxes with the voxel-size
  chooser right-aligned), so nothing collides at the minimum window width; the
  progress bars are restyled as full-width tracks with a left-anchored accent
  fill and the percentage centered over the track (no more bare pills/dots at
  low values); the Run button is a prominent filled accent button and reads a
  filled red "Abort" while running. Layout/appearance only, no behavior
  changes.
- 2026-10-26 Debug rendering (task 5): new `--debug-render <state.json>` mode —
  renders a documented JSON GUI state (`version` 1; `window` size `>= 100` px,
  default 1000x700; `inputs`; `selected_input`; `voxel_axes`; `voxel_size`;
  `base`; `output`; `progress` 0–100; `running`) to `screenshot.jpg` (JPEG q90,
  CWD, overwritten) and exits, printing `Saved screenshot.jpg (<w>x<h>)` on
  success; strict validation rejects unknown fields, wrong types and
  out-of-range values, naming the offending field and value. The GUI now
  renders from a shared `GuiState` (see "Architecture"); `image` added for
  JPEG encoding, with `winit` 0.30 as a test-only dev-dependency (the e2e test
   runs the event loop on the test worker thread); interactive GUI behavior
   unchanged.
- 2026-10-25 GUI: "Voxels: X, Y, Z" checkboxes (all checked by default) select
  which axes define the voxel cells; an unchecked axis's key component is 0, so
  the cells are extruded along it (e.g. Y unchecked → cells keyed by X and Z).
  GUI-only — the CLI has no flag and always uses all three axes, whose keys and
  output are byte-identical to the previous behavior. The `Run started` log
  line gains a `voxel axes: <axes>` field (`XZ`, `Y`, …), and the GUI rejects a
  run with no axis checked.
- 2026-10-25 Run log: the log file is now always `splat-merge.log` beside the
  executable (previously `<output-stem>.log` beside the output); progress lines
  no longer appear in the log (only the phase start and finish lines do); line
  timestamps now carry millisecond precision
  (`[YYYY-MM-DD HH:MM:SS.mmm]`). Stdout output and GUI behavior are unchanged.
- 2026-10-25 Run log: every run (CLI and GUI) now writes a plain-text log file
  `<output-stem>.log` beside the output (no new flag or control). Each line is
  stamped with a UTC `[YYYY-MM-DD HH:MM:SS]` prefix and flushed as soon as it is
  complete; the log records the run start, per-input and base
  name/size/splat count, step and phase start/finish with durations (so the
  scale step's silent counting pass is visible), every progress line verbatim,
  and a terminal `Run finished` / `Run aborted` / `Run failed` line. The log is
  truncated at run start, kept after failure or abort, and not written when the
  output path itself ends in `.log`. Stdout output and GUI message dialogs are
  unchanged.
- 2026-10-25 GUI layout: the input file list is now a visible rounded box with
  a 1px border, inset 16px from the window edges; the egui panels' built-in
  separator lines are disabled, so no line touches the Run button and the
  spacing is purely the 8px margins. Appearance only, no behavior changes.
- 2026-10-25 GUI layout: the input file list fills all remaining window
  height (scrolling if it overflows), the progress table spans the full
  window width in equal-width columns, and the content is padded 16px
  against the window edges (8px between components). Layout only, no
  behavior changes.
- 2026-10-25 GUI progress table: the log area is replaced by a two-row,
  three-column table ("Mapping", "Merging", "Cleaning") with one progress bar
  per column and the percentage painted centered inside each bar; the bars
  reset on run start and keep their last value after the run ends.
- 2026-10-25 GUI messages: validation failures, rejected/duplicate files, run
  failures, and the pipeline's non-progress lines (`Success:`, the abort line,
  other errors) are now shown as native message dialogs instead of log lines.
- 2026-10-25 Progress labels renamed (GUI and CLI): the voxel step's merge
  phase is `2 Merging (voxel based)` and the scale step's phase is
  `3 Cleaning (scale based)`.
- 2026-10-25 Windows: linked with the GUI subsystem
  (`#![windows_subsystem = "windows"]`) so no console window opens when
  launching the GUI directly; CLI mode is unchanged (run from a console).
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