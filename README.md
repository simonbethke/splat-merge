# Intro
This is a tool, that I wanted to have to merge multiple splats. At some point, I found that 20 million splats were not representing the detail that I wanted, but tools like brush could not train much more on my 24GB VRAM. Thus I started to train the same scene multiple times. Always exporting different areas of poses from reality scan to colmap and training each set to 20M splats (at SH1).
While this allows to get a total of many more splats, it still requires an efficient way to merge the splats. Thats where this tool comes into play:

First I merge the detail-area splats with the voxel mode. With a size set to 0.5, the scene is cut into 50x50x50cm cubes. For each cube, it is comparing the two input plys, which one has more splats. Per cube it will take all splats either from the first or the second input. I do this with all inputs one after the other until I end up with one large splat.
Unfortunately, this large splat contains additional floaters. Those are typically occluded areas, that should contain no splats, but from one of the detail-views it was occluded, so the trainer didnt know better. The thing these floaters have in common is, that they are rather big.
To work around this, I do this: I train a base-scene (which I need for LOD purposes anyways). This scene were all poses trained at once to the 20M splats max. Now I use this tool, with the Scale mode. In this mode, it will take all splats from the first ply that are smaller than the given threshold and the ones bigger than the threshold from the second ply. Just throw in, the voxel-combined details first and the 20M-Base second and you end up with a clean and huge splat. The threshold that worked well for me was 1cm³ (0.01).

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

### Scale Mode (Size Merge)
Combines `file1` and `file2`. Uses all splats that are smaller than threshold from file1 and the ones equal or bigger from file2.
```bash
cargo run --release -- -i file1.ply -j file2.ply -o merged.ply --mode scale --threshold 0.01
