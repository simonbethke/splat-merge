# Intro
This is a tool, that I wanted to have to merge multiple splats. At some point, I found that 20 million splats were not representing the detail that I wanted, but tools like brush could not train much more on my 24GB VRAM. Thus I started to train the same scene multiple times. Always exporting different areas of poses from reality scan to colmap and training each set to 20M splats (at SH1).
While this allows to get a total of many more splats, it still requires an efficient way to merge the splats. Thats where this tool comes into play:

First I merge the detail-area splats with the voxel mode. With a size set to 0.5, the scene is cut into 50x50x50cm cubes. For each cube, it compares all input plys at once: per cube it will take all splats from the input that has the most splats in it (ties go to the earliest input). One pass over the whole input list, and I end up with one large splat.
Unfortunately, this large splat contains additional floaters. Those are typically occluded areas, that should contain no splats, but from one of the detail-views it was occluded, so the trainer didnt know better. The thing these floaters have in common is, that they are rather big.

To work around this, I do this: I train a base-scene (which I need for LOD purposes anyways). This scene were all poses trained at once to the 20M splats max. Now I use this tool, with the Scale mode. In this mode, it will take all splats from the first ply that are smaller than the given threshold and the ones bigger than the threshold from the second ply. Just throw in, the voxel-combined details first and the 20M-Base second and you end up with a clean and huge splat. The threshold that worked well for me was 1cm³ (0.01).

# Splat-Merge

> **PRODUCED ENTIRELY BY GEMINI (AI)**
> This is a **"Vibe-Coded"** product. The architecture, logic, and implementation were developed through interactive sessions between the user and Gemini.

A high-performance Rust utility designed to merge or filter 3D Gaussian Splatting PLY files. The tool utilizes memory mapping (`mmap`) to handle multi-gigabyte files efficiently on consumer hardware (64GB RAM optimized).

## 🛠 Features

* **Memory Efficiency**: Uses `memmap2` to interface with large files without loading them entirely into RAM.
* **Voxel Density Merge**: Merges all files in the input list in a single pass by comparing splat density within a grid; per cube, only the splats from the densest file are kept (ties go to the earliest input).
* **Scale Thresholding**: Splits or filters splats based on their maximum dimension (world-space scale), derived from the exponential of the logged scale properties.
* **Progress**: Prints `1 Mapping` and `2 Merging` progress lines to stdout while merging.
* **Null-Input Support**: Uses `-` as a filename to treat an input slot as empty.
* **Binary Accuracy**: Surgically reconstructs PLY headers and preserves the exact binary stride of Gaussian Splat properties (SH, rotation, scale, etc.).

## 🚀 Usage

### Voxel Mode (Density Merge)
Combines all inputs in the list in a single pass. For every 0.5 unit cube (the default size; change it with `-v`), it keeps the splats of the earliest input that has the most splats in that voxel.
```bash
cargo run --release -- -i file1.ply,file2.ply -o merged.ply
```

### Scale Mode (Size Merge)
Combines an input with a `--base` file. Uses all splats that are smaller than the threshold (default `0.01`) from the first input and the ones equal or bigger from the base.
```bash
cargo run --release -- -i file1.ply --base file2.ply -o merged.ply
```

### Typical workflow
All detail files can also be merged with the base in a single command:
```bash
cargo run --release -- -i detail_a.ply,detail_b.ply,detail_c.ply,detail_d.ply --base base.ply -o merged.ply
```
Or step by step:
```bash
cargo run --release -- -i detail_a.ply,detail_b.ply -o detail_ab.ply
cargo run --release -- -i detail_c.ply,detail_d.ply -o detail_cd.ply
cargo run --release -- -i detail_ab.ply,detail_cd.ply -o detail_abcd.ply

cargo run --release -- -i detail_abcd.ply --base base.ply -o merged.ply
```