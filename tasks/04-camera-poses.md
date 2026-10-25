# 4) Load Camera Poses and Project Archiving (.smp)

In this task, the tool should implement loading, caching, and archiving of camera poses and intrinsics from COLMAP datasets, wrapped in a self-contained project format with the extension `.smp` (**Splat-Merge Project**). A COLMAP dataset consists of the files `cameras.txt`, `images.txt`, and `points3D.txt`.

## GUI Integration & Workflow
* In the GUI, every input PLY entry gets a directory picker to assign its corresponding COLMAP dataset.
* **Self-Contained Portability**: Projects must be fully independent of the original source datasets, allowing users to delete or move the source COLMAP folders later without breaking project execution or viewing.

## Project Storage Format (`.smp` Archive)
The project is saved as a structured ZIP archive containing two core components:
1. **`settings.json`**: Contains lightweight project metadata, output file paths, file paths to source PLY files, and references to the embedded camera cache.
2. **Binary Camera Cache (`cameras.bin` / serialized via `bincode`)**: Stores the fully parsed and processed camera poses, intrinsic parameters, and world-to-camera/camera-to-world transformation matrices in a fast, compact binary format.

When opening an existing project, the tool loads metadata from the JSON and reads the binary poses directly, falling back to parsing original COLMAP text files only if the binary cache is missing.

## OS Integration (Windows File Association)
* **Extension Registration**: The tool should support registering the `.smp` file extension in the Windows Registry (or via application installation/startup routines) so that `.smp` files are associated with `splat-merge`.
* **CLI / Launch Handling**: Double-clicking an `.smp` file in Windows Explorer must pass the file path as a command-line argument to the executable, automatically loading the project state and opening the GUI.

## Parsing Specifications (COLMAP Text Export)
To load camera poses and intrinsics from COLMAP's text export files (`cameras.txt` and `images.txt`), parse each file line by line while skipping comment lines starting with `#`.

* **`cameras.txt`**: Extract the `CAMERA_ID`, projection model type, sensor dimensions (`WIDTH`, `HEIGHT`), and intrinsic parameters (`PARAMS[]`).
* **`images.txt`**: Read alternating pairs of lines: the first line provides the `IMAGE_ID`, the world-to-camera rotation quaternion (`qw, qx, qy, qz`), translation vector (`tx, ty, tz`), associated `CAMERA_ID`, and image filename. (The second line containing 2D-3D point observations can be skipped). Convert the rotation quaternion and translation vector into your desired coordinate frame (e.g., inverting world-to-camera to get camera-to-world transformation matrices) and map each image to its corresponding intrinsics using the `CAMERA_ID`.