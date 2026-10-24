# splat-merge: Documentation

Single source of truth for project content. Agent behavior rules are in `AGENTS.md`.

## Purpose
Rust CLI to merge or filter 3D Gaussian Splatting PLY files. Built for multi-GB inputs on
consumer hardware (target: 64 GB RAM) using memory mapping (`memmap2`).

Motivation: a single training run is limited to ~20M splats (24 GB VRAM). A scene is trained
multiple times on different pose areas (each at 20M splats, SH1), then the results are merged
into one larger splat.

## Modes

### voxel (density merge)
- Space is divided into cubes of `--voxel-size` (0.5 = 50 cm).
- Per voxel, the splats of the input with more splats in that voxel are kept; the other
  input's splats in that voxel are dropped (XOR by density).
- Used to combine detail-area splats. Applied pairwise until one file remains.
- Side effect: floaters in areas occluded in one detail view (typically large splats).

### scale (size merge)
- Keep splats from file 1 whose max world-space scale is < `--threshold`.
- Keep splats from file 2 whose max world-space scale is >= `--threshold`.
- Used to remove floaters: file 1 = voxel-merged details, file 2 = base scene (all poses,
  20M splats, also used for LOD). Empirically good threshold: 0.01.

### Null input
Filename `-` in any input slot (input list entry or `--base`) means an empty file.
This allows use as a standalone filter (e.g. `--base -` keeps only the small-scale
splats of the input list). If every data source (all inputs, and the base if given)
is `-`, the run is rejected with an error (no file to supply stride and header).

## Binary format rules (must not break)
- PLY data is binary little-endian with a fixed per-splat stride
  (position, SH, opacity, scale, rotation).
- Kept splat records are copied byte-exact; output preserves the exact stride and property layout.
- Output header is reconstructed from the source header: all lines are emitted
  unchanged except `element vertex <count>`, which is rewritten with the final count.
- Scale properties are stored as log values. World-space scale = `exp(scale_n)`;
  "max scale" = maximum over the three axes.
- Inputs are never loaded fully into RAM: mmap for reading, buffered writer for output.

## CLI
| Flag | Meaning |
|---|---|
| `-i, --inputs <a,b,c>` | Comma-separated list of input PLY files; an entry may be `-` (empty) |
| `--base <file>` | Optional base PLY (`-` = empty); triggers the final scale step |
| `-o, --output <file>` | Output PLY |
| `-v, --voxel-size <f>` | Cube edge length for the voxel steps (default: `0.5`) |
| `-t, --threshold <f>` | Max-scale threshold for the scale step (default: `0.01`) |

Input list entries are trimmed of surrounding whitespace; an empty entry (e.g. `a,,b`)
is an error. `--mode` no longer exists: voxel steps always run on the input list, and
the scale step runs when `--base` is present (see "Pipeline").

## Typical workflow
```bash
# Merge detail areas, then combine with the base scene (one command):
cargo run --release -- -i detail_a.ply,detail_b.ply,detail_c.ply,detail_d.ply --base base.ply -o merged.ply

# Voxel-only merge of two detail files:
cargo run --release -- -i detail_a.ply,detail_b.ply -o detail_ab.ply

# Keep only the small-scale splats of one file (no base, base slot empty):
cargo run --release -- -i scene.ply --base - -o small.ply
```

## Pipeline
A run executes these steps in order:

1. **Voxel chain** (only with 2 or more input-list entries): the inputs are
   pairwise voxel-merged in list order (A+B→T1, T1+C→T2, T2+D→T1). Every step whose
   result is not the final output is written to a temporary file; a voxel step writes
   directly to the output file only when it is the last step of the run (i.e. no
   `--base` was given).
2. **Scale step** (only when `--base` is present): the accumulated source (the single
   input, or the chained voxel result) is file 1; `--base` is file 2. Kept splats
   (file 1: max scale `<` threshold; file 2: `>=`) are written to the output file.
3. **Passthrough** (exactly one input, no `--base`): the input is copied to the
   output verbatim (header count unchanged).

Temporary files:
- Named `<output-stem>.temp1.ply` and `<output-stem>.temp2.ply` beside the output
  file, alternating by step (first voxel step → temp1, second → temp2, third →
  temp1, ...).
- Always removed at the end of the run, on success or on error. A failed removal of
  a temp file produces a warning, not an error.

Errors:
- Empty input-list entry, or all data sources `-`: error (nothing to read).
- Mismatched stride between any non-empty file: error (property layouts differ;
  a byte-exact merge is impossible).

## Architecture
Single binary. Dependencies: `clap` (CLI), `memmap2` (input mapping),
`hashbrown` (voxel density maps).

Source layout:
- `src/main.rs`: clap-derived `Args`; `main` parses and calls `pipeline::run`.
- `src/ply.rs`: `PlyInfo` + `parse_ply_header`, `validate`, `get_pos`, `get_max_scale`,
  and `rewrite_header` (reconstructed output header).
- `src/merge.rs`: `MergeMode` (voxel / scale) + `merge_pair` — one pairwise merge,
  mode-agnostic, writes header then kept records. Operates on byte slices, not files.
- `src/pipeline.rs`: input-list parsing, file mapping/validation, stride check,
  voxel-chain orchestration with temp files, scale step, passthrough, cleanup.

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
- `merge_pair`: one pairwise merge for either mode into any `Write` target (temp or
  output). Voxel mode: first pass counts per-voxel density into a hashbrown map keyed by
  `(x, y, z)` i32 voxel indices (`floor(p / voxel_size)`); second pass applies the per-file
  keep rules (file 1 kept on `count1 >= count2`, file 2 kept on `count2 > count1`). Scale
  mode: per-splat max world scale compared against the threshold (`<` → file 1, `>=` → file 2).
  Writes with a `BufWriter`: the reconstructed header (all source header lines except
  `element vertex`, rewritten with the final count), then byte-exact copies of the kept
  records, file 1's splats followed by file 2's.
- `pipeline::run`: orchestrates per the "Pipeline" section — parses the input list,
  maps each file (`-` = empty), validates, requires equal strides across all non-empty
  files, runs the voxel chain with alternating temp files (always removed at the end),
  then the scale step with `--base`, or the single-input passthrough.

## Required test cases
Status: implemented as of 2026-10-24 — merge-level cases in `src/merge.rs`,
pipeline-level cases in `src/pipeline.rs`.

- Empty input via `-` (either side; all-`-` data sources return the documented error)
- Voxel tie (equal counts per voxel): file 1's splats are kept (file 1 uses `>=`, file 2 uses `>`)
- Voxel keys for negative coordinates
- Threshold boundary: `<` goes to file 1, `>=` to file 2
- Output header vertex count equals the number of written splats
- Kept splat records are byte-identical to the input
- Malformed input (truncated data, `len % stride != 0`, missing scale property in the
  scale step) returns an error
- Single input without `--base` yields a byte-identical passthrough copy
- 2+ inputs chain as voxel steps in list order, followed by the scale step with `--base`
- Temp files do not exist after the run (on success or on error)
- Mismatched stride between non-empty files returns an error

## Open questions
- README calls the threshold "1cm³ (0.01)", but the described comparison uses max axis
  length, not volume. Confirm intended semantics and fix the wording.

## Changelog
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