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
Filename `-` as an input means an empty file. This allows use as a standalone filter/clipper.
Both inputs `-` is rejected with an error (no input to supply stride and header).

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
| `-i, --input1 <file>` | First input PLY (`-` = empty) |
| `-j, --input2 <file>` | Second input PLY (`-` = empty) |
| `-o, --output <file>` | Output PLY |
| `-m, --mode voxel\|scale` | Merge mode (default: `voxel`) |
| `-v, --voxel-size <f>` | Cube edge length (voxel mode, default: `0.5`) |
| `-t, --threshold <f>` | Max-scale threshold (scale mode, default: `0.1`) |

## Typical workflow
```bash
cargo run --release -- -i detail_a.ply -j detail_b.ply -o detail_ab.ply --mode voxel --voxel-size 0.5
cargo run --release -- -i detail_c.ply -j detail_d.ply -o detail_cd.ply --mode voxel --voxel-size 0.5
cargo run --release -- -i detail_ab.ply -j detail_cd.ply -o detail_abcd.ply --mode voxel --voxel-size 0.5
cargo run --release -- -i detail_abcd.ply -j base.ply -o merged.ply --mode scale --threshold 0.01
```

## Architecture
Single binary. Dependencies: `clap` (CLI), `memmap2` (input mapping),
`hashbrown` (voxel density maps).

Source layout:
- `src/main.rs`: `Mode` + clap-derived `Args`; `main` parses and calls `pipeline::run`.
- `src/ply.rs`: `PlyInfo` + `parse_ply_header`, `get_pos`, `get_max_scale`, and
  `rewrite_header` (reconstructed output header).
- `src/merge.rs`: `MergeMode` (voxel / scale) + `merge_pair` — one pairwise merge,
  mode-agnostic, writes header then kept records.
- `src/pipeline.rs`: input mapping (`-` = empty; both empty = error), stride check,
  mode dispatch, output writing.

Responsibilities by item:
- `Args`: clap-derived CLI (flags in the "CLI" section).
- `PlyInfo` + `parse_ply_header`: scans the ASCII header; computes the per-splat byte
  stride from the `property` declarations (`float` = 4, `double` = 8, `uchar`/`uint8` = 1
  byte), stores the vertex count, records the byte offset of `scale_0`, and keeps the raw
  header used to re-emit the output header.
- `get_pos(record)`: decodes the first 12 bytes of a record as the little-endian f32
  position (assumes position is the first property; used for voxel keys).
- `get_max_scale(record, offset)`: decodes `scale_0..2` and returns the max of the three
  log values, exponentiated to world space.
- `main`:
  - maps each input with `memmap2` (`-` = empty; both empty = error); requires equal
    strides across all inputs.
  - Voxel mode: first pass counts per-voxel density into a hashbrown map keyed by
    `(x, y, z)` i32 voxel indices (`floor(p / voxel_size)`); second pass applies the
    per-file keep rules (file 1 kept on `count1 >= count2`, file 2 kept on `count2 > count1`).
  - Scale mode: per-splat max world scale compared against the threshold.
  - Writes with a `BufWriter`: the reconstructed header (all source header lines except
    `element vertex`, rewritten with the final count), then byte-exact copies of the kept
    records, file 1's splats followed by file 2's.

## Required test cases
Status: no tests exist yet; this is the required coverage set to meet when touching
related logic.

- Empty input via `-` (either side; both `-` returns the documented error)
- Voxel tie (equal counts per voxel): file 1's splats are kept (file 1 uses `>=`, file 2 uses `>`)
- Voxel keys for negative coordinates
- Threshold boundary: `<` goes to file 1, `>=` to file 2
- Output header vertex count equals the number of written splats
- Kept splat records are byte-identical to the input
- Malformed input (truncated data, `len % stride != 0`, missing properties) returns an error

## Open questions
- README calls the threshold "1cm³ (0.01)", but the described comparison uses max axis
  length, not volume. Confirm intended semantics and fix the wording.
- Code default threshold is `0.1` while the README recommends `0.01`. Confirm the
  intended default.

## Changelog
- 2026-10-24 Code split: `src/main.rs` refactored into `main.rs` (CLI), `ply.rs`
  (format), `merge.rs` (pairwise merge), `pipeline.rs` (run orchestration); behavior unchanged.
- 2026-10-24 Docs sync: completed "Architecture"; added CLI short flags and defaults
  (`-m` default `voxel`, `-v` default `0.5`, `-t` default `0.1`); recorded voxel-tie
  behavior (file 1 kept), both-`-` input error, and header re-emission rule; marked the
  "Required test cases" list as not yet implemented; noted the 0.1 vs 0.01 default
  question.
- 2026-10-24 Initial documentation.