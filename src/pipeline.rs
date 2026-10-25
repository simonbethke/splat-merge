use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::merge::{self, UserAborted};
use crate::ply::{parse_ply_header, rewrite_header, validate, PlyInfo};
use crate::Args;

/// One mapped data source; an empty input (`-`) is represented by `None`.
struct Input {
    /// Read-only memory-mapped PLY file.
    mmap: Mmap,
    /// Parsed header metadata.
    info: PlyInfo,
}

/// Maps `path` for reading and validates it; `-` is the empty input.
fn map_file(path: &Path) -> Result<Option<Input>, Box<dyn std::error::Error>> {
    if path == Path::new("-") {
        return Ok(None);
    }
    let file = File::open(path)?;
    // SAFETY: memmap2 maps the file read-only; the mapping stays alive for
    // the duration of the returned value.
    let mmap = unsafe { Mmap::map(&file)? };
    let info = parse_ply_header(&mmap);
    validate(&mmap, &info)?;
    Ok(Some(Input { mmap, info }))
}

/// Splits a mapped input into the byte view plus metadata the merge functions need.
fn src_of(inp: &Option<Input>) -> Option<(&[u8], &PlyInfo)> {
    inp.as_ref().map(|i| (&i.mmap[..], &i.info))
}

/// Requires equal strides across all non-empty sources and returns the
/// header template (a copy of the first real file's raw header).
fn header_template(
    inputs: &[Option<Input>],
    base: Option<&Input>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut stride = None;
    let mut header = None;
    for inp in inputs.iter().flatten().chain(base) {
        match stride {
            None => {
                stride = Some(inp.info.stride);
                header = Some(inp.info.raw_header.clone());
            }
            Some(s) if s != inp.info.stride => {
                return Err(
                    "Stride mismatch between input files (different property layouts)".into(),
                );
            }
            Some(_) => {}
        }
    }
    header.ok_or_else(|| {
        "All data sources are '-' (empty); provide at least one real PLY file".into()
    })
}

/// The two alternating temp-file paths used by the voxel chain, beside the output.
fn temp_paths(output: &str) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let out = Path::new(output);
    let stem = out.file_stem().ok_or("Output path has no file name")?;
    let stem = stem.to_string_lossy();
    let parent = out.parent().unwrap_or(Path::new("."));
    Ok((
        parent.join(format!("{}.temp1.ply", stem)),
        parent.join(format!("{}.temp2.ply", stem)),
    ))
}

/// Removes a temp file; a failed removal is a warning, not an error.
fn remove_quiet(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "Warning: could not remove temporary file {}: {}",
                path.display(),
                e
            );
        }
    }
}

/// Removes the run's temp files when dropped, on success and error paths alike.
struct TempGuard {
    paths: [PathBuf; 2],
}

impl TempGuard {
    fn new(p1: PathBuf, p2: PathBuf) -> Self {
        Self { paths: [p1, p2] }
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        for p in self.paths.iter() {
            remove_quiet(p);
        }
    }
}

/// Builds the run's error type from a message.
fn error_msg(msg: &str) -> Box<dyn std::error::Error> {
    Box::<dyn std::error::Error>::from(msg.to_string())
}

/// Copies a single input to `out` verbatim (passthrough), count unchanged.
fn passthrough(inp: &Input, out: &mut impl Write) -> Result<usize, Box<dyn std::error::Error>> {
    rewrite_header(out, &inp.info.raw_header, inp.info.vertex_count)?;
    out.write_all(&inp.mmap[inp.info.header_len..])?;
    Ok(inp.info.vertex_count)
}

/// The cancel flag for CLI runs: never set (cancellation is only reachable
/// through the GUI).
static NEVER: AtomicBool = AtomicBool::new(false);

/// CLI entry: runs the full pipeline described by `args` and writes the
/// output file; progress lines and the final `Success:` line go to the
/// locked stdout.
pub fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = std::io::stdout().lock();
    run_with(args, &mut stdout, &NEVER)
}

/// Runs the pipeline for `args`, writing progress lines and the final
/// `Success:` line to `out`. On a user abort it removes the partially
/// written output file, streams the documented abort line to the sink
/// (temp files are removed by their guard on every path), and returns the
/// `UserAborted` error; other errors leave the output file in place.
pub fn run_with(
    args: &Args,
    out: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    match run_steps(args, out, cancel) {
        Ok(n) => {
            writeln!(out, "Success: {} splats saved to {}", n, args.output)?;
            Ok(())
        }
        Err(e) => {
            if e.downcast_ref::<UserAborted>().is_some() {
                remove_quiet(Path::new(&args.output));
                // A failed sink write must not shadow the abort error.
                let _ = writeln!(out, "Aborted; temporary and partial output files removed.");
            }
            Err(e)
        }
    }
}

/// The pipeline body (see the "Pipeline" section); progress lines go to
/// `prog`. Returns the number of splats written to the output file.
fn run_steps(
    args: &Args,
    prog: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<usize, Box<dyn std::error::Error>> {
    // 1. Parse the comma-separated input list.
    let names: Vec<&str> = args.inputs.split(',').map(str::trim).collect();
    if names.iter().any(|n| n.is_empty()) {
        return Err("Input list is empty or has an empty entry; use `-` for empty slots".into());
    }

    // 2. Map and validate every data source (inputs and optional base).
    let mut inputs = Vec::with_capacity(names.len());
    for name in &names {
        inputs.push(map_file(Path::new(name))?);
    }
    let base: Option<Input> = if let Some(b) = args.base.as_deref() {
        map_file(Path::new(b))?
    } else {
        None
    };

    // 3. Shared stride and header template (errors on mismatch / all '-').
    let header = header_template(&inputs, base.as_ref())?;

    let (temp1, temp2) = temp_paths(&args.output)?;
    let _temp_guard = TempGuard::new(temp1.clone(), temp2);
    let mut writer = BufWriter::new(File::create(&args.output)?);

    let n: usize = if inputs.len() >= 2 {
        // 4a. Two or more inputs: one voxel step over the whole list.
        let sources: Vec<Option<(&[u8], &PlyInfo)>> = inputs.iter().map(src_of).collect();
        if let Some(base) = base {
            // Base present: the step must write to temp1, which the scale
            // step re-maps as file 1 (see the "Pipeline" section).
            let mut temp_writer = BufWriter::new(File::create(&temp1)?);
            merge::merge_voxel(
                &sources,
                args.voxel_size,
                &header,
                &mut temp_writer,
                prog,
                cancel,
            )?;
            temp_writer.flush()?;
            drop(temp_writer);
            // 4b. Scale step: voxel result (file 1) against the base (file 2).
            let voxel = map_file(&temp1)?;
            merge::merge_scale(
                src_of(&voxel),
                Some((&base.mmap[..], &base.info)),
                args.threshold,
                &header,
                &mut writer,
                prog,
                cancel,
            )?
        } else {
            merge::merge_voxel(
                &sources,
                args.voxel_size,
                &header,
                &mut writer,
                prog,
                cancel,
            )?
        }
    } else {
        // 4c. Single input slot.
        let only = inputs
            .into_iter()
            .next()
            .ok_or_else(|| error_msg("No input to process"))?;
        if let Some(base) = base {
            // Single input + base: one scale step straight to the output.
            merge::merge_scale(
                src_of(&only),
                Some((&base.mmap[..], &base.info)),
                args.threshold,
                &header,
                &mut writer,
                prog,
                cancel,
            )?
        } else {
            // Single input, no base: passthrough copy (no progress lines).
            let inp =
                only.ok_or_else(|| error_msg("Single input is '-' and no --base was given"))?;
            passthrough(&inp, &mut writer)?
        }
    };

    writer.flush()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ply::parse_ply_header;

    /// Temp directory, unique per test, removed when the value is dropped.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "splat-merge-test-{}-{}",
                std::process::id(),
                name
            ));
            std::fs::create_dir_all(&d).unwrap();
            TmpDir(d)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Property block of an 11-float record (same layout as real fixtures).
    const PROPS: &str = "property float x\nproperty float y\nproperty float z\n\
                         property float opacity\nproperty float scale_0\nproperty float scale_1\n\
                         property float scale_2\nproperty float rot_0\nproperty float rot_1\n\
                         property float rot_2\nproperty float rot_3\n";

    /// One vertex record: 11 little-endian f32, equal log-scale on all axes.
    fn record(x: f32, y: f32, z: f32, log_scale: f32) -> Vec<u8> {
        let mut b = Vec::with_capacity(44);
        for v in [
            x, y, z, 1.0, log_scale, log_scale, log_scale, 1.0, 0.0, 0.0, 0.0,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    /// Write a synthetic PLY with the given records; returns its path as a String.
    fn write_ply(dir: &TmpDir, name: &str, recs: &[Vec<u8>]) -> String {
        let body: Vec<u8> = recs.iter().flatten().copied().collect();
        let mut buf = format!(
            "ply\nformat binary_little_endian 1.0\nelement vertex {}\n{}end_header\n",
            recs.len(),
            PROPS
        )
        .into_bytes();
        buf.extend_from_slice(&body);
        let p = dir.path(name);
        std::fs::write(&p, buf).unwrap();
        p.to_string_lossy().into_owned()
    }

    /// Args for tests; the threshold is `e` so the log-scale 1.0 boundary is exact.
    fn args(inputs: &str, base: Option<&str>, output: &Path) -> crate::Args {
        crate::Args {
            inputs: inputs.to_string(),
            base: base.map(str::to_string),
            output: output.to_string_lossy().into_owned(),
            voxel_size: 0.5,
            threshold: 1.0f32.exp(),
        }
    }

    /// The body of a written PLY (everything after the parsed header).
    fn body_of(data: &[u8]) -> &[u8] {
        let i = parse_ply_header(data);
        &data[i.header_len..]
    }

    #[test]
    fn passthrough_single_input_is_byte_identical() {
        let dir = TmpDir::new("passthrough");
        let recs = vec![record(0.0, 0.0, 0.0, 0.1), record(9.0, 9.0, 9.0, 0.2)];
        let a = write_ply(&dir, "a.ply", &recs);
        let out = dir.path("out.ply");
        run(&args(&a, None, &out)).unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            std::fs::read(dir.path("a.ply")).unwrap()
        );
    }

    #[test]
    fn single_input_with_base_runs_scale_step() {
        let dir = TmpDir::new("one-base");
        let a_small = record(0.0, 0.0, 0.0, 0.9); // exp(0.9) < e -> file 1 keeps
        let a_big = record(1.0, 1.0, 1.0, 1.5); // exp(1.5) > e -> file 1 drops
        let b_keep = record(2.0, 2.0, 2.0, 1.6); // exp(1.6) > e -> file 2 keeps
        let a = write_ply(&dir, "a.ply", &[a_small.clone(), a_big]);
        let b = write_ply(&dir, "b.ply", std::slice::from_ref(&b_keep));
        let out = dir.path("out.ply");
        run(&args(&a, Some(&b), &out)).unwrap();
        let data = std::fs::read(&out).unwrap();
        let i = parse_ply_header(&data);
        assert_eq!(i.vertex_count, 2);
        let mut expected = a_small;
        expected.extend(b_keep);
        assert_eq!(body_of(&data), expected.as_slice());
        assert!(!dir.path("out.temp1.ply").exists());
        assert!(!dir.path("out.temp2.ply").exists());
    }

    #[test]
    fn two_inputs_voxel_then_scale_with_base() {
        let dir = TmpDir::new("two-base");
        // Voxel step (size 0.5): vox(0,0,0) and vox(10,10,10).
        let a1 = record(0.0, 0.0, 0.0, 0.5); // 2-1 win in vox0
        let a2 = record(0.2, 0.1, 0.1, 0.6); // -> chain result [a1, a2, b_keep]
        let b_drop = record(0.3, 0.2, 0.1, 0.7);
        let b_keep = record(5.0, 5.0, 5.0, 0.8);
        // Scale step: threshold = e.
        let base_small = record(1.0, 0.0, 0.0, 0.9); // exp(0.9) < e -> file 2 drops
        let base_keep = record(2.0, 0.0, 5.0, 1.6); // exp(1.6) >= e -> file 2 keeps
        let a = write_ply(&dir, "a.ply", &[a1.clone(), a2.clone()]);
        let b = write_ply(&dir, "b.ply", &[b_drop, b_keep.clone()]);
        let base = write_ply(&dir, "base.ply", &[base_small, base_keep.clone()]);
        let out = dir.path("merged.ply");
        run(&args(&format!("{},{}", a, b), Some(&base), &out)).unwrap();
        let data = std::fs::read(&out).unwrap();
        let i = parse_ply_header(&data);
        assert_eq!(i.vertex_count, 4);
        let mut expected = a1;
        expected.extend(a2);
        expected.extend(b_keep);
        expected.extend(base_keep);
        assert_eq!(body_of(&data), expected.as_slice());
        assert!(!dir.path("merged.temp1.ply").exists());
        assert!(!dir.path("merged.temp2.ply").exists());
    }

    #[test]
    fn three_inputs_voxel_chain_without_base() {
        let dir = TmpDir::new("three-chain");
        // Step 1 (A+B): vox0 2-1 win -> a1, a2; vox10 0-1 win -> b_out.
        let a1 = record(0.0, 0.0, 0.0, 0.1);
        let a2 = record(0.1, 0.2, 0.3, 0.2);
        let _b_in = record(0.4, 0.1, 0.2, 0.3); // loses in vox0
        let b_out = record(5.0, 5.0, 5.0, 0.3);
        // Step 2 (T1+C): vox10 tie 1-1 -> file 1 (b_out) kept.
        let _c_in = record(5.1, 5.1, 5.1, 0.4);
        let a = write_ply(&dir, "a.ply", &[a1.clone(), a2.clone()]);
        let b = write_ply(&dir, "b.ply", &[_b_in, b_out.clone()]);
        let c = write_ply(&dir, "c.ply", &[_c_in]);
        let out = dir.path("out3.ply");
        run(&args(&format!("{},{},{}", a, b, c), None, &out)).unwrap();
        let data = std::fs::read(&out).unwrap();
        let i = parse_ply_header(&data);
        assert_eq!(i.vertex_count, 3);
        let mut expected = a1;
        expected.extend(a2);
        expected.extend(b_out);
        assert_eq!(body_of(&data), expected.as_slice());
        assert!(!dir.path("out3.temp1.ply").exists());
        assert!(!dir.path("out3.temp2.ply").exists());
    }

    #[test]
    fn three_inputs_with_base() {
        let dir = TmpDir::new("three-base");
        // Voxel step (size 0.5): vox0 a/c tie 1-1 -> a wins; vox10 b/c tie
        // 1-1 -> b wins. Kept: a1, b_out (input-list order).
        let a1 = record(0.0, 0.0, 0.0, 0.5); // kept in voxel; exp(0.5) < e -> kept in scale
        let c0 = record(0.1, 0.1, 0.1, 0.3); // loses tie in vox0
        let b_out = record(5.0, 5.0, 5.0, 1.8); // wins tie in vox10; exp(1.8) >= e -> dropped
        let c10 = record(5.1, 5.1, 5.1, 0.9); // loses tie in vox10
                                              // Scale step: threshold = e.
        let base_small = record(1.0, 0.0, 0.0, 0.6); // exp(0.6) < e -> file 2 drops
        let base_keep = record(2.0, 0.0, 5.0, 1.7); // exp(1.7) >= e -> file 2 keeps
        let a = write_ply(&dir, "a.ply", std::slice::from_ref(&a1));
        let b = write_ply(&dir, "b.ply", std::slice::from_ref(&b_out));
        let c = write_ply(&dir, "c.ply", &[c0, c10]);
        let base = write_ply(&dir, "base.ply", &[base_small, base_keep.clone()]);
        let out = dir.path("out3b.ply");
        run(&args(&format!("{},{},{}", a, b, c), Some(&base), &out)).unwrap();
        let data = std::fs::read(&out).unwrap();
        let i = parse_ply_header(&data);
        assert_eq!(i.vertex_count, 2);
        let mut expected = a1;
        expected.extend(base_keep);
        assert_eq!(body_of(&data), expected.as_slice());
        assert!(!dir.path("out3b.temp1.ply").exists());
        assert!(!dir.path("out3b.temp2.ply").exists());
    }

    #[test]
    fn empty_slot_in_list_is_allowed() {
        let dir = TmpDir::new("slots");
        let a = write_ply(&dir, "a.ply", &[record(0.0, 0.0, 0.0, 0.1)]);
        let out = dir.path("out.ply");
        // `-` vs a: file 2 (a) wins every voxel -> output body equals a's body.
        run(&args(&format!("-,{}", a), None, &out)).unwrap();
        let data = std::fs::read(&out).unwrap();
        let i = parse_ply_header(&data);
        assert_eq!(i.vertex_count, 1);
    }

    #[test]
    fn all_empty_inputs_error() {
        let dir = TmpDir::new("all-empty");
        let out = dir.path("out.ply");
        assert!(run(&args("-", None, &out)).is_err());
        assert!(run(&args("-,-", Some("-"), &out)).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn empty_input_entry_errors() {
        let dir = TmpDir::new("bad-entry");
        let a = write_ply(&dir, "a.ply", &[record(0.0, 0.0, 0.0, 0.1)]);
        let out = dir.path("out.ply");
        assert!(run(&args(&format!("{},", a), None, &out)).is_err()); // trailing comma
        assert!(run(&args("", None, &out)).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn stride_mismatch_errors() {
        let dir = TmpDir::new("stride");
        let a = write_ply(&dir, "a.ply", &[record(0.0, 0.0, 0.0, 0.1)]); // stride 44
                                                                         // 8-float records -> stride 32.
        let mut body = Vec::new();
        for _ in 0..8 {
            body.extend_from_slice(&1f32.to_le_bytes());
        }
        let mut b = "ply\nformat binary_little_endian 1.0\nelement vertex 1\n\
            property float x\nproperty float y\nproperty float z\nproperty float opacity\n\
            property float rot_0\nproperty float rot_1\nproperty float rot_2\nproperty float rot_3\n\
            end_header\n"
            .as_bytes()
            .to_vec();
        b.extend_from_slice(&body);
        let p = dir.path("b8.ply");
        std::fs::write(&p, b).unwrap();
        let bs = p.to_string_lossy().into_owned();
        let out = dir.path("out.ply");
        assert!(run(&args(&format!("{},{}", a, bs), None, &out)).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn truncated_data_errors() {
        let dir = TmpDir::new("truncated");
        // Header declares 2 records, file holds 1.
        let rec = record(0.0, 0.0, 0.0, 0.1);
        let mut buf = format!(
            "ply\nformat binary_little_endian 1.0\nelement vertex 2\n{}end_header\n",
            PROPS
        )
        .into_bytes();
        buf.extend_from_slice(&rec);
        let p = dir.path("bad.ply");
        std::fs::write(&p, buf).unwrap();
        let s = p.to_string_lossy().into_owned();
        let out = dir.path("out.ply");
        assert!(run(&args(&s, None, &out)).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn non_divisible_data_errors() {
        let dir = TmpDir::new("nondiv");
        // One full record (44 bytes) plus 5 stray bytes -> 49 % 44 != 0.
        let mut rec = record(0.0, 0.0, 0.0, 0.1);
        rec.extend_from_slice(&[7u8; 5]);
        let mut buf = format!(
            "ply\nformat binary_little_endian 1.0\nelement vertex 1\n{}end_header\n",
            PROPS
        )
        .into_bytes();
        buf.extend_from_slice(&rec);
        let p = dir.path("bad.ply");
        std::fs::write(&p, buf).unwrap();
        let s = p.to_string_lossy().into_owned();
        let out = dir.path("out.ply");
        assert!(run(&args(&s, None, &out)).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn temp_files_removed_after_failure() {
        let dir = TmpDir::new("cleanup");
        // 8-float records (no scale property) with equal strides on all sources:
        // the voxel step succeeds into a temp file, the scale step then fails;
        // the temp file must be removed despite the error.
        let mut body8 = Vec::new();
        for _ in 0..8 {
            body8.extend_from_slice(&1f32.to_le_bytes());
        }
        let head = "ply\nformat binary_little_endian 1.0\nelement vertex 1\n\
                    property float x\nproperty float y\nproperty float z\nproperty float opacity\n\
                    property float rot_0\nproperty float rot_1\nproperty float rot_2\nproperty float rot_3\n\
                    end_header\n";
        for name in ["a8.ply", "b8.ply", "base8.ply"] {
            let mut buf = head.to_string().into_bytes();
            buf.extend_from_slice(&body8);
            let p = dir.path(name);
            std::fs::write(&p, buf).unwrap();
        }
        let a = dir.path("a8.ply").to_string_lossy().into_owned();
        let b = dir.path("b8.ply").to_string_lossy().into_owned();
        let base = dir.path("base8.ply").to_string_lossy().into_owned();
        let out = dir.path("merged.ply");
        assert!(run(&args(&format!("{},{}", a, b), Some(&base), &out)).is_err());
        assert!(!dir.path("merged.temp1.ply").exists());
        assert!(!dir.path("merged.temp2.ply").exists());
    }

    #[test]
    fn aborted_run_removes_temp_and_output_and_reports() {
        let dir = TmpDir::new("abort");
        let a = write_ply(&dir, "a.ply", &[record(0.0, 0.0, 0.0, 0.5)]);
        let b = write_ply(&dir, "b.ply", &[record(5.0, 5.0, 5.0, 0.8)]);
        let base = write_ply(&dir, "base.ply", &[record(2.0, 0.0, 5.0, 1.6)]);
        let out = dir.path("merged.ply");
        let cancel = AtomicBool::new(true);
        let mut sink: Vec<u8> = Vec::new();
        let res = run_with(
            &args(&format!("{},{}", a, b), Some(&base), &out),
            &mut sink,
            &cancel,
        );
        let err = res.unwrap_err();
        assert_eq!(err.to_string(), "Aborted by user");
        assert!(err.downcast_ref::<UserAborted>().is_some());
        assert!(!dir.path("merged.temp1.ply").exists());
        assert!(!dir.path("merged.temp2.ply").exists());
        assert!(!out.exists());
        let log = String::from_utf8(sink).unwrap();
        assert!(log.contains("Aborted; temporary and partial output files removed."));
        assert!(!log.contains("Success:"));
    }

    #[test]
    fn run_with_success_line_goes_to_sink() {
        let dir = TmpDir::new("sink");
        let recs = vec![record(0.0, 0.0, 0.0, 0.1), record(9.0, 9.0, 9.0, 0.2)];
        let a = write_ply(&dir, "a.ply", &recs);
        let out = dir.path("out.ply");
        let mut sink: Vec<u8> = Vec::new();
        run_with(&args(&a, None, &out), &mut sink, &NEVER).unwrap();
        let log = String::from_utf8(sink).unwrap();
        let expected = format!("Success: 2 splats saved to {}\n", out.to_string_lossy());
        assert!(log.contains(&expected));
    }
}
