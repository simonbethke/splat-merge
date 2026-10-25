use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::logfile::{format_bytes, format_duration, format_splats, LogFile};
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

/// The run log sink for one run: either the real log file beside the
/// executable, or nothing at all (writes are accepted and dropped) when
/// the output is the log file itself.
enum LogSink {
    /// The log file; every line passes through.
    File(LogFile),
    /// No log file; accepted writes are dropped.
    Null,
}

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            LogSink::File(lf) => lf.write(buf),
            LogSink::Null => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            LogSink::File(lf) => lf.flush(),
            LogSink::Null => Ok(()),
        }
    }
}

/// The run log path: `splat-merge.log` beside the executable file,
/// per the "Run log" documentation section.
fn log_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()
        .map_err(|e| error_msg(&format!("Cannot locate the executable: {e}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| error_msg("Executable path has no parent directory"))?;
    Ok(dir.join("splat-merge.log"))
}

/// True when `output` names the same file as the run log (case-insensitive;
/// writing the log would then truncate the output, and vice versa). A
/// relative `output` is resolved against the current directory.
fn output_is_the_log(output: &str, log: &Path) -> bool {
    let out = Path::new(output);
    let resolved = if out.is_absolute() {
        out.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(out))
            .unwrap_or_else(|_| out.to_path_buf())
    };
    resolved
        .to_string_lossy()
        .eq_ignore_ascii_case(&log.to_string_lossy())
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
/// `Success:` line to `out`. Regardless of the sink, a run log file is
/// created (truncating) as `splat-merge.log` beside the executable, per
/// the "Run log" documentation section: it is skipped only when the output
/// is that same log file. It receives the `Run started` line, the
/// input/base lines, the step/phase detail lines, and a terminal
/// `Run finished` / `Run aborted` / `Run failed` line, which is written
/// before the sink's `Success:`/abort line. On a user abort the partially
/// written output file is removed (temp files by their guard); the log is
/// kept in every case. A failure to locate the executable or to create the
/// log file is a run error.
pub fn run_with(
    args: &Args,
    out: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let logp = log_path()?;
    let mut log = if output_is_the_log(&args.output, &logp) {
        LogSink::Null
    } else {
        LogSink::File(LogFile::new(&logp)?)
    };
    let inputs = args
        .inputs
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        log,
        "Run started (inputs: {}; base: {}; output: {}; voxel size: {}; threshold: {})",
        inputs,
        args.base.as_deref().unwrap_or("-"),
        args.output,
        args.voxel_size,
        args.threshold
    );
    match run_steps(args, out, &mut log, cancel) {
        Ok(n) => {
            let size = std::fs::metadata(&args.output)
                .map(|m| m.len())
                .unwrap_or(0);
            let _ = writeln!(
                log,
                "Run finished: {} splats written to {} ({}) in {}",
                format_splats(n),
                args.output,
                format_bytes(size),
                format_duration(started.elapsed().as_secs())
            );
            writeln!(out, "Success: {} splats saved to {}", n, args.output)?;
            Ok(())
        }
        Err(e) => {
            if e.downcast_ref::<UserAborted>().is_some() {
                remove_quiet(Path::new(&args.output));
                let _ = writeln!(
                    log,
                    "Run aborted after {}",
                    format_duration(started.elapsed().as_secs())
                );
                // A failed sink write must not shadow the abort error.
                let _ = writeln!(out, "Aborted; temporary and partial output files removed.");
            } else {
                let _ = writeln!(
                    log,
                    "Run failed after {}: {}",
                    format_duration(started.elapsed().as_secs()),
                    e
                );
            }
            Err(e)
        }
    }
}

/// The pipeline body (see the "Pipeline" section); progress lines go to
/// `prog`, run-log detail lines (input/base lines, step markers) to
/// `log` (see the "Run log" section). Returns the number of splats
/// written to the output file.
fn run_steps(
    args: &Args,
    prog: &mut impl Write,
    log: &mut impl Write,
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

    // Log the resolved inputs and the optional base.
    for (n, (name, inp)) in names.iter().zip(inputs.iter()).enumerate() {
        match inp {
            Some(inp) => {
                let _ = writeln!(
                    log,
                    "Input {}: {} ({}, {} splats)",
                    n + 1,
                    name,
                    format_bytes(inp.mmap.len() as u64),
                    format_splats(inp.info.vertex_count)
                );
            }
            None => {
                let _ = writeln!(log, "Input {}: - (empty)", n + 1);
            }
        }
    }
    if let Some(base_name) = &args.base {
        match base.as_ref() {
            Some(b) => {
                let _ = writeln!(
                    log,
                    "Base: {} ({}, {} splats)",
                    base_name,
                    format_bytes(b.mmap.len() as u64),
                    format_splats(b.info.vertex_count)
                );
            }
            None => {
                let _ = writeln!(log, "Base: - (empty)");
            }
        }
    }

    // 3. Shared stride and header template (errors on mismatch / all '-').
    let header = header_template(&inputs, base.as_ref())?;

    let (temp1, temp2) = temp_paths(&args.output)?;
    let _temp_guard = TempGuard::new(temp1.clone(), temp2);
    let mut writer = BufWriter::new(File::create(&args.output)?);

    let n: usize = if inputs.len() >= 2 {
        // 4a. Two or more inputs: one voxel step over the whole list.
        let _ = writeln!(log, "Voxel step started ({} inputs)", inputs.len());
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
                log,
                cancel,
            )?;
            temp_writer.flush()?;
            drop(temp_writer);
            // 4b. Scale step: voxel result (file 1) against the base (file 2).
            let voxel = map_file(&temp1)?;
            let _ = writeln!(
                log,
                "Scale step started (file 1: {} splats; file 2: {} splats; threshold {})",
                format_splats(voxel.as_ref().map_or(0, |i| i.info.vertex_count)),
                format_splats(base.info.vertex_count),
                args.threshold
            );
            merge::merge_scale(
                src_of(&voxel),
                Some((&base.mmap[..], &base.info)),
                args.threshold,
                &header,
                &mut writer,
                prog,
                log,
                cancel,
            )?
        } else {
            merge::merge_voxel(
                &sources,
                args.voxel_size,
                &header,
                &mut writer,
                prog,
                log,
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
            let _ = writeln!(
                log,
                "Scale step started (file 1: {} splats; file 2: {} splats; threshold {})",
                format_splats(only.as_ref().map_or(0, |i| i.info.vertex_count)),
                format_splats(base.info.vertex_count),
                args.threshold
            );
            merge::merge_scale(
                src_of(&only),
                Some((&base.mmap[..], &base.info)),
                args.threshold,
                &header,
                &mut writer,
                prog,
                log,
                cancel,
            )?
        } else {
            // Single input, no base: passthrough copy (no progress lines).
            let inp =
                only.ok_or_else(|| error_msg("Single input is '-' and no --base was given"))?;
            let _ = writeln!(log, "Passthrough started");
            let t = Instant::now();
            let n = passthrough(&inp, &mut writer)?;
            let _ = writeln!(
                log,
                "Passthrough finished after {}",
                format_duration(t.elapsed().as_secs())
            );
            n
        }
    };

    writer.flush()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ply::parse_ply_header;

    /// Serializes every test that runs the pipeline: they all truncate the
    /// shared `splat-merge.log` beside the test binary, and a concurrent
    /// open-for-write on it fails (sharing violation).
    static RUN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn shared_log_lock() -> std::sync::MutexGuard<'static, ()> {
        RUN_LOCK.lock().unwrap()
    }

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

    /// The run log path this test binary writes (see `log_path`).
    fn exe_log_path() -> PathBuf {
        log_path().unwrap()
    }

    #[test]
    fn passthrough_single_input_is_byte_identical() {
        let _lock = shared_log_lock();
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
        let _lock = shared_log_lock();
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
        let _lock = shared_log_lock();
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
        let _lock = shared_log_lock();
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
        let _lock = shared_log_lock();
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
        let _lock = shared_log_lock();
        let dir = TmpDir::new("slots");
        let a = write_ply(&dir, "a.ply", &[record(0.0, 0.0, 0.0, 0.1)]);
        let out = dir.path("out.ply");
        // `-` vs a: file 2 (a) wins every voxel -> output body equals a's body.
        run(&args(&format!("-,{}", a), None, &out)).unwrap();
        let data = std::fs::read(&out).unwrap();
        let i = parse_ply_header(&data);
        assert_eq!(i.vertex_count, 1);
        // The empty slot is logged as `Input 1: - (empty)` in the shared log.
        let rlog = std::fs::read_to_string(exe_log_path()).unwrap();
        assert!(rlog.contains("Input 1: - (empty)"));
        assert!(rlog.contains(&format!("Input 2: {a}")));
    }

    #[test]
    fn all_empty_inputs_error() {
        let _lock = shared_log_lock();
        let dir = TmpDir::new("all-empty");
        let out = dir.path("out.ply");
        assert!(run(&args("-", None, &out)).is_err());
        assert!(run(&args("-,-", Some("-"), &out)).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn empty_input_entry_errors() {
        let _lock = shared_log_lock();
        let dir = TmpDir::new("bad-entry");
        let a = write_ply(&dir, "a.ply", &[record(0.0, 0.0, 0.0, 0.1)]);
        let out = dir.path("out.ply");
        assert!(run(&args(&format!("{},", a), None, &out)).is_err()); // trailing comma
        assert!(run(&args("", None, &out)).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn stride_mismatch_errors() {
        let _lock = shared_log_lock();
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
        // The run log is kept, with the terminal failure line.
        let rlog = std::fs::read_to_string(exe_log_path()).unwrap();
        assert!(rlog.contains("Run failed after"));
        assert!(rlog.contains("Stride mismatch between input files (different property layouts)"));
        assert!(!rlog.contains("Run finished"));
    }

    #[test]
    fn truncated_data_errors() {
        let _lock = shared_log_lock();
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
        let _lock = shared_log_lock();
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
        let _lock = shared_log_lock();
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
        let _lock = shared_log_lock();
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
        // The run log is kept, with the terminal abort line.
        let rlog = std::fs::read_to_string(exe_log_path()).unwrap();
        assert!(rlog.contains("Run aborted after"));
        assert!(!rlog.contains("Run finished"));
    }

    #[test]
    fn run_with_success_line_goes_to_sink() {
        let _lock = shared_log_lock();
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

    /// Strips the `[YYYY-MM-DD HH:MM:SS.mmm] ` prefix (asserting its shape).
    fn bare(line: &str) -> &str {
        assert!(
            line.len() >= 26
                && line.starts_with('[')
                && line.as_bytes()[20] == b'.'
                && line.as_bytes()[24..26] == b"] "[..]
        );
        &line[26..]
    }

    #[test]
    fn successful_run_writes_run_log_beside_executable() {
        let _lock = shared_log_lock();
        let dir = TmpDir::new("runlog-passthrough");
        let recs = vec![record(0.0, 0.0, 0.0, 0.1), record(9.0, 9.0, 9.0, 0.2)];
        let a = write_ply(&dir, "a.ply", &recs);
        let out = dir.path("out.ply");
        let mut sink: Vec<u8> = Vec::new();
        run_with(&args(&a, None, &out), &mut sink, &NEVER).unwrap();

        let in_sz = format_bytes(std::fs::metadata(&a).unwrap().len());
        let out_sz = format_bytes(std::fs::metadata(&out).unwrap().len());
        let a_str = a.as_str();
        let out_str = out.to_string_lossy().into_owned();
        let expected = vec![
            format!(
                "Run started (inputs: {a_str}; base: -; output: {out_str}; voxel size: 0.5; threshold: 2.7182817)"
            ),
            format!("Input 1: {a_str} ({in_sz}, 2 splats)"),
            "Passthrough started".to_string(),
            "Passthrough finished after 0s".to_string(),
            format!("Run finished: 2 splats written to {out_str} ({out_sz}) in 0s"),
        ];
        let lines: Vec<String> = std::fs::read_to_string(exe_log_path())
            .unwrap()
            .lines()
            .map(bare)
            .map(str::to_string)
            .collect();
        assert_eq!(lines, expected);
        // Detail lines never reach the progress sink (stdout / GUI).
        let sink_str = String::from_utf8(sink).unwrap();
        assert!(sink_str.contains(&format!("Success: 2 splats saved to {out_str}")));
        assert!(!sink_str.contains("Run started"));
        assert!(!sink_str.contains("Input 1:"));
        assert!(!sink_str.contains("Passthrough"));
    }

    #[test]
    fn output_is_the_run_log_writes_no_run_log() {
        let _lock = shared_log_lock();
        let dir = TmpDir::new("log-out");
        let recs = vec![record(0.0, 0.0, 0.0, 0.1), record(9.0, 9.0, 9.0, 0.2)];
        let a = write_ply(&dir, "a.ply", &recs);
        // The output is the run log file itself: the guard must skip opening
        // the log (its truncation would destroy the output) and still write
        // the PLY into that very file.
        let out = exe_log_path();
        let mut sink: Vec<u8> = Vec::new();
        run_with(
            &crate::Args {
                inputs: a,
                base: None,
                output: out.to_string_lossy().into_owned(),
                voxel_size: 0.5,
                threshold: 1.0f32.exp(),
            },
            &mut sink,
            &NEVER,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            std::fs::read(dir.path("a.ply")).unwrap()
        );
        let sink_str = String::from_utf8(sink).unwrap();
        assert!(sink_str.contains(&format!(
            "Success: 2 splats saved to {}",
            out.to_string_lossy()
        )));
    }

    #[test]
    fn run_log_records_voxel_then_scale_steps_in_order() {
        let _lock = shared_log_lock();
        let dir = TmpDir::new("runlog-order");
        // Voxel step over a+b (size 0.5): a's two splats and b's (0.3, 0.4, 0.2)
        // share voxel (0,0,0) where a has more; b's (5,5,5) wins its own voxel
        // -> 3 kept. Scale step (threshold e = 2.7182817): the three kept have
        // world scale 1.65 / 1.82 / 2.23 (< e, kept), base keeps only 4.95
        // (>= e) -> 4 of 5 kept.
        let a = write_ply(
            &dir,
            "a.ply",
            &[record(0.0, 0.0, 0.0, 0.5), record(0.2, 0.1, 0.1, 0.6)],
        );
        let b = write_ply(
            &dir,
            "b.ply",
            &[record(0.3, 0.4, 0.2, 0.7), record(5.0, 5.0, 5.0, 0.8)],
        );
        let base = write_ply(
            &dir,
            "base.ply",
            &[record(0.0, 1.0, 0.0, 0.9), record(7.0, 7.0, 7.0, 1.6)],
        );
        let out = dir.path("merged.ply");
        let mut sink: Vec<u8> = Vec::new();
        run_with(
            &args(&format!("{},{}", a, b), Some(&base), &out),
            &mut sink,
            &NEVER,
        )
        .unwrap();
        assert_eq!(
            parse_ply_header(&std::fs::read(&out).unwrap()).vertex_count,
            4
        );

        let sz = |p: &str| format_bytes(std::fs::metadata(p).unwrap().len());
        let out_str = out.to_string_lossy().into_owned();
        let expected = vec![
            format!(
                "Run started (inputs: {a}, {b}; base: {base}; output: {out_str}; voxel size: 0.5; threshold: 2.7182817)"
            ),
            format!("Input 1: {a} ({}, 2 splats)", sz(&a)),
            format!("Input 2: {b} ({}, 2 splats)", sz(&b)),
            format!("Base: {base} ({}, 2 splats)", sz(&base)),
            "Voxel step started (2 inputs)".to_string(),
            "1 Mapping started".to_string(),
            "1 Mapping finished after 0s".to_string(),
            "2 Merging (voxel based) started".to_string(),
            "2 Merging (voxel based) finished after 0s (3 kept)".to_string(),
            "Scale step started (file 1: 3 splats; file 2: 2 splats; threshold 2.7182817)"
                .to_string(),
            "3 Cleaning (scale based): counting kept splats".to_string(),
            "3 Cleaning (scale based) started (4 of 5 splats kept)".to_string(),
            "3 Cleaning (scale based) finished after 0s".to_string(),
            format!("Run finished: 4 splats written to {out_str} ({}) in 0s", sz(&out_str)),
        ];
        let lines: Vec<String> = std::fs::read_to_string(exe_log_path())
            .unwrap()
            .lines()
            .map(bare)
            .map(str::to_string)
            .collect();
        assert_eq!(lines, expected);
        assert!(!lines.iter().any(|l| l.contains('%')));

        // Progress lines and Success reach the sink; detail lines do not.
        let sink_str = String::from_utf8(sink).unwrap();
        assert!(sink_str.contains("1 Mapping: 100% (0.0M splats)"));
        assert!(sink_str.contains(&format!("Success: 4 splats saved to {out_str}")));
        assert!(!sink_str.contains("1 Mapping started"));
        assert!(!sink_str.contains("Voxel step started"));
        assert!(!sink_str.contains("counting kept splats"));
        assert!(!sink_str.contains("Run started"));
    }
}
