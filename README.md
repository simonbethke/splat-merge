# Splat-Merge

> **PRODUCED ENTIRELY BY GEMINI (AI)**
> This is a **"Vibe-Coded"** product. The architecture, logic, and implementation were developed through interactive sessions between the user and Gemini.

A high-performance Rust utility designed to merge or filter 3D Gaussian Splatting PLY files. The tool utilizes memory mapping (`mmap`) to handle multi-gigabyte files efficiently on consumer hardware (64GB RAM optimized).

## 🛠 Features

* **Memory Efficiency**: Uses `memmap2` to interface with large files without loading them entirely into RAM.
* **Voxel Density XOR**: Merges two files by comparing splat density within a grid; in case of conflict, only the splats from the denser file are kept.
* **Scale Thresholding**: Splits or filters splats based on their maximum dimension (world-space scale), derived from the exponential of the logged scale properties.
* **Null-Input Support**: Uses `-` as a filename to treat an input as empty, allowing the tool to function as a standalone filter/clipper.
* **Binary Accuracy**: Surgically reconstructs PLY headers and preserves the exact binary stride of Gaussian Splat properties (SH, rotation, scale, etc.).

## 🚀 Usage

### Voxel Mode (Density Merge)
Combines `file1` and `file2`. For every 0.5 unit cube, it keeps splats only from the file that has more splats in that specific voxel.
```bash
cargo run --release -- -i file1.ply -j file2.ply -o merged.ply --mode voxel --voxel-size 0.5