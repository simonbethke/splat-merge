use hashbrown::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::logfile::{format_duration, format_splats};
use crate::ply::{get_max_scale, get_pos, rewrite_header, PlyInfo};
use crate::progress::Progress;

/// Voxel index key for a position: `floor(p / voxel_size)` per axis.
type Key = (i32, i32, i32);

#[inline(always)]
fn voxel_key(p: [f32; 3], voxel_size: f32) -> Key {
    (
        (p[0] / voxel_size).floor() as i32,
        (p[1] / voxel_size).floor() as i32,
        (p[2] / voxel_size).floor() as i32,
    )
}

/// Per-file byte-size shares for a phase: each file's share of the total
/// (all files weighted equally zero when the total size is zero).
pub fn file_weights(sizes: &[usize]) -> Vec<f64> {
    let total: usize = sizes.iter().sum();
    if total == 0 {
        return vec![0.0; sizes.len()];
    }
    sizes.iter().map(|s| *s as f64 / total as f64).collect()
}

/// Error returned by a merge step when its cancel flag is set
/// (the GUI's confirmed Abort).
#[derive(Debug)]
pub struct UserAborted;

impl std::fmt::Display for UserAborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Aborted by user")
    }
}

impl std::error::Error for UserAborted {}

/// Checks the shared cancel flag; returns the [`UserAborted`] error when it
/// is set. Called per record inside the hot loops (a single relaxed atomic
/// load; no allocation).
#[inline(always)]
fn check_cancel(cancel: &AtomicBool) -> Result<(), UserAborted> {
    if cancel.load(Ordering::Relaxed) {
        Err(UserAborted)
    } else {
        Ok(())
    }
}

/// One voxel (density) merge of all (possibly empty) sources in a single
/// run: one density hashmap per non-empty source, then a per-voxel winner
/// pass. The winner per voxel is the earliest input (list order) with the
/// most splats there; kept splats are written in input-list order, record
/// order within each file.
///
/// Writes the reconstructed header (from `header`, rewritten with the
/// final count) followed by the kept records byte-exact. Progress phases
/// `1 Mapping` (hashmaps filled) and `2 Merging (voxel based)` (selection and writing),
/// per the "Progress" documentation section; the phase start/finish
/// detail lines (with durations) go to `log`, per the "Run log" section.
/// The `cancel` flag is checked per record/file (`UserAborted` error).
/// Returns the number of splats written.
pub fn merge_voxel(
    sources: &[Option<(&[u8], &PlyInfo)>],
    voxel_size: f32,
    header: &str,
    out: &mut impl Write,
    prog: &mut impl Write,
    log: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<usize, Box<dyn std::error::Error>> {
    let sizes: Vec<usize> = sources
        .iter()
        .map(|s| s.map_or(0, |(m, _)| m.len()))
        .collect();
    let weights = file_weights(&sizes);
    let totals: Vec<usize> = sources
        .iter()
        .map(|s| s.map_or(0, |(_, i)| i.vertex_count))
        .collect();

    // `1 Mapping`: fill one density hashmap per source.
    let _ = writeln!(log, "1 Mapping started");
    let t1 = Instant::now();
    let mut maps: Vec<HashMap<Key, usize>> = sources.iter().map(|_| HashMap::new()).collect();
    let mut prog1 = Progress::new("1 Mapping", weights.clone(), totals.clone());
    for (f, src) in sources.iter().enumerate() {
        if let Some((m, i)) = *src {
            for idx in 0..i.vertex_count {
                check_cancel(cancel)?;
                let p = get_pos(&m[i.header_len + idx * i.stride..]);
                *maps[f].entry(voxel_key(p, voxel_size)).or_insert(0) += 1;
                prog1.advance(f, 1, prog);
            }
        }
    }
    prog1.finish(prog);
    let _ = writeln!(
        log,
        "1 Mapping finished after {}",
        format_duration(t1.elapsed().as_secs())
    );

    // `2 Merging`: selection and copy of the winning records in input-list
    // order (this phase's duration covers both passes).
    let _ = writeln!(log, "2 Merging (voxel based) started");
    let t2 = Instant::now();

    // Per-voxel winner: max count, ties keep the earliest input, so files
    // processed in order only replace the winner on a strictly higher count.
    let mut winner: HashMap<Key, (usize, usize)> = HashMap::new();
    let mut kept = 0usize;
    for (f, map) in maps.iter().enumerate() {
        check_cancel(cancel)?;
        for (&v, &c) in map.iter() {
            if winner.get(&v).is_none_or(|(_, wc)| c > *wc) {
                match winner.insert(v, (f, c)) {
                    Some((_, wc)) => kept = kept - wc + c,
                    None => kept += c,
                }
            }
        }
    }

    let mut prog2 = Progress::new("2 Merging (voxel based)", weights, totals);
    rewrite_header(out, header, kept)?;
    for (f, src) in sources.iter().enumerate() {
        if let Some((m, i)) = *src {
            if maps[f].is_empty() {
                continue;
            }
            for idx in 0..i.vertex_count {
                check_cancel(cancel)?;
                if winner
                    .get(&voxel_key(
                        get_pos(&m[i.header_len + idx * i.stride..]),
                        voxel_size,
                    ))
                    .is_some_and(|wf| wf.0 == f)
                {
                    let start = i.header_len + idx * i.stride;
                    out.write_all(&m[start..start + i.stride])?;
                }
                prog2.advance(f, 1, prog);
            }
        }
    }
    prog2.finish(prog);
    let _ = writeln!(
        log,
        "2 Merging (voxel based) finished after {} ({} kept)",
        format_duration(t2.elapsed().as_secs()),
        format_splats(kept)
    );
    Ok(kept)
}

/// True when the splat at `idx` in `m` passes the scale keep rule of its
/// file: file 1 (`below = true`) keeps max world scale `< threshold`,
/// file 2 keeps `>= threshold`.
#[inline(always)]
fn scale_passes(
    m: &[u8],
    i: &PlyInfo,
    idx: usize,
    off: usize,
    below: bool,
    threshold: f32,
) -> bool {
    let s = get_max_scale(&m[i.header_len + idx * i.stride..], off);
    if below {
        s < threshold
    } else {
        s >= threshold
    }
}

/// Counts the splats of `src` passing the keep rule (`below`: `< threshold`,
/// else `>= threshold`), so the header count is known before streaming.
/// Checks `cancel` per record.
fn count_kept(
    src: Option<(&[u8], &PlyInfo)>,
    below: bool,
    threshold: f32,
    cancel: &AtomicBool,
) -> Result<usize, Box<dyn std::error::Error>> {
    if let Some((m, i)) = src {
        if let Some(off) = i.scale_offset {
            let mut n = 0usize;
            for idx in 0..i.vertex_count {
                check_cancel(cancel)?;
                if scale_passes(m, i, idx, off, below, threshold) {
                    n += 1;
                }
            }
            Ok(n)
        } else {
            Ok(0)
        }
    } else {
        Ok(0)
    }
}

/// One scale (size) merge as a single pass: file 1 keeps splats with max
/// world scale `< threshold`, file 2 `>= threshold`; file 1's kept splats
/// are written before file 2's. Errors `No scale in Input 1/2` when a
/// present source declares no scale property.
///
/// Writes the reconstructed header followed by the kept records
/// byte-exact. Progress: only the `3 Cleaning (scale based)` phase; the
/// phase detail lines (counting marker, kept count, duration) go to `log`,
/// per the "Run log" documentation section. The `cancel` flag is
/// checked per record (`UserAborted` error). Returns the number of splats
/// written.
// Two sources + threshold + header + out/prog/log sinks + cancel make 8
// parameters; a struct would obscure a positional-only call site.
#[allow(clippy::too_many_arguments)]
pub fn merge_scale(
    src1: Option<(&[u8], &PlyInfo)>,
    src2: Option<(&[u8], &PlyInfo)>,
    threshold: f32,
    header: &str,
    out: &mut impl Write,
    prog: &mut impl Write,
    log: &mut impl Write,
    cancel: &AtomicBool,
) -> Result<usize, Box<dyn std::error::Error>> {
    // Absent sources are fine (nothing to validate); present sources must
    // declare a scale property.
    if let Some((_, i)) = src1 {
        if i.scale_offset.is_none() {
            return Err("No scale in Input 1".into());
        }
    }
    if let Some((_, i)) = src2 {
        if i.scale_offset.is_none() {
            return Err("No scale in Input 2".into());
        }
    }

    let sizes = [
        src1.map_or(0, |(m, _)| m.len()),
        src2.map_or(0, |(m, _)| m.len()),
    ];
    let weights = file_weights(&sizes);
    let totals = [
        src1.map_or(0, |(_, i)| i.vertex_count),
        src2.map_or(0, |(_, i)| i.vertex_count),
    ];

    // Count pass (the log marks it; the started line carries the result).
    let _ = writeln!(log, "3 Cleaning (scale based): counting kept splats");
    let total =
        count_kept(src1, true, threshold, cancel)? + count_kept(src2, false, threshold, cancel)?;
    let all = totals[0] + totals[1];
    let _ = writeln!(
        log,
        "3 Cleaning (scale based) started ({} of {} splats kept)",
        format_splats(total),
        format_splats(all)
    );
    let t2 = Instant::now();
    rewrite_header(out, header, total)?;

    let mut prog2 = Progress::new("3 Cleaning (scale based)", weights, totals.to_vec());
    for (f, src) in [src1, src2].into_iter().enumerate() {
        if let Some((m, i)) = src {
            if let Some(off) = i.scale_offset {
                let below = f == 0;
                for idx in 0..i.vertex_count {
                    check_cancel(cancel)?;
                    if scale_passes(m, i, idx, off, below, threshold) {
                        let start = i.header_len + idx * i.stride;
                        out.write_all(&m[start..start + i.stride])?;
                    }
                    prog2.advance(f, 1, prog);
                }
            }
        }
    }
    prog2.finish(prog);
    let _ = writeln!(
        log,
        "3 Cleaning (scale based) finished after {}",
        format_duration(t2.elapsed().as_secs())
    );
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ply::parse_ply_header;

    /// Property block of an 11-float record (x, y, z, opacity,
    /// scale_0..2, rot_0..3), matching the project's fixture layout.
    const PROPS: &str = "property float x\nproperty float y\nproperty float z\n\
                         property float opacity\nproperty float scale_0\nproperty float scale_1\n\
                         property float scale_2\nproperty float rot_0\nproperty float rot_1\n\
                         property float rot_2\nproperty float rot_3\n";

    /// One vertex record: 11 little-endian f32 with equal log-scale on all axes.
    fn record(x: f32, y: f32, z: f32, log_scale: f32) -> Vec<u8> {
        let mut b = Vec::with_capacity(44);
        for v in [
            x, y, z, 1.0, log_scale, log_scale, log_scale, 1.0, 0.0, 0.0, 0.0,
        ] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    /// Build a complete synthetic PLY in memory with `count` records and `body`.
    fn ply(count: usize, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "ply\nformat binary_little_endian 1.0\nelement vertex {}\n{}end_header\n",
            count, PROPS
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// The body of a merge output (everything after the parsed header).
    fn body_of(out: &[u8]) -> Vec<u8> {
        let i = parse_ply_header(out);
        out[i.header_len..].to_vec()
    }

    /// The result of the `run_voxel` / `run_scale` helpers.
    type RunOut = Result<(Vec<u8>, usize, String, String), Box<dyn std::error::Error>>;

    /// Runs one voxel merge over in-memory files (`None` = empty slot) with
    /// 11-float records; returns output bytes, splat count, the progress
    /// lines in order, and the run-log lines in order.
    fn run_voxel(files: &[Option<Vec<Vec<u8>>>]) -> RunOut {
        // Build (buffer, info) pairs in file order; empty slots stay `None`
        // in `sources`, aligned 1:1 with `files`.
        let mut pairs: Vec<(Vec<u8>, PlyInfo)> = Vec::new();
        for recs in files.iter().flatten() {
            let body: Vec<u8> = recs.iter().flatten().copied().collect();
            let b = ply(recs.len(), &body);
            let info = parse_ply_header(&b);
            pairs.push((b, info));
        }
        let mut bi = 0usize;
        let mut sources: Vec<Option<(&[u8], &PlyInfo)>> = Vec::new();
        for f in files {
            if f.is_some() {
                let (b, i) = &pairs[bi];
                sources.push(Some((b.as_slice(), i)));
                bi += 1;
            } else {
                sources.push(None);
            }
        }
        let header = pairs
            .first()
            .map(|(_, i)| i.raw_header.clone())
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut prog = Vec::new();
        let mut log = Vec::new();
        let nc = AtomicBool::new(false);
        let n = merge_voxel(&sources, 0.5, &header, &mut out, &mut prog, &mut log, &nc)?;
        Ok((
            out,
            n,
            String::from_utf8_lossy(&prog).into_owned(),
            String::from_utf8_lossy(&log).into_owned(),
        ))
    }

    /// Runs one scale merge over in-memory files; returns output bytes,
    /// splat count, the progress lines in order, and the run-log lines in
    /// order.
    fn run_scale(recs1: &[Vec<u8>], recs2: Option<&[Vec<u8>]>) -> RunOut {
        let body1: Vec<u8> = recs1.iter().flatten().copied().collect();
        let b1 = ply(recs1.len(), &body1);
        let i1 = parse_ply_header(&b1);
        let pair2: Option<(Vec<u8>, PlyInfo)> = recs2.map(|recs| {
            let body: Vec<u8> = recs.iter().flatten().copied().collect();
            let b = ply(recs.len(), &body);
            let info = parse_ply_header(&b);
            (b, info)
        });
        let mut out = Vec::new();
        let mut prog = Vec::new();
        let mut log = Vec::new();
        let src1: Option<(&[u8], &PlyInfo)> = Some((&b1[..], &i1));
        let src2: Option<(&[u8], &PlyInfo)> = pair2.as_ref().map(|(b, i)| (b.as_slice(), i));
        let nc = AtomicBool::new(false);
        let n = merge_scale(
            src1,
            src2,
            1.0f32.exp(),
            &i1.raw_header,
            &mut out,
            &mut prog,
            &mut log,
            &nc,
        )?;
        Ok((
            out,
            n,
            String::from_utf8_lossy(&prog).into_owned(),
            String::from_utf8_lossy(&log).into_owned(),
        ))
    }

    #[test]
    fn voxel_tie_keeps_earliest_file() {
        let a1 = record(0.0, 0.0, 0.0, 0.1);
        let b1 = record(0.4, 0.3, 0.2, 0.2); // same voxel (0,0,0) as a1
        let (out, n, lines, log) = run_voxel(&[Some(vec![a1.clone()]), Some(vec![b1])]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(body_of(&out), a1);
        assert_eq!(
            lines,
            "1 Mapping: 100% (0.0M splats)\n2 Merging (voxel based): 100% (0.0M splats)\n"
        );
        // Progress lines only: no detail lines leak into the progress sink.
        assert!(!lines.contains("started") && !lines.contains("finished"));
        // Log carries the phase sequence with durations and the kept count,
        // but no progress lines.
        assert_eq!(
            log,
            "1 Mapping started\n1 Mapping finished after 0s\n\
             2 Merging (voxel based) started\n\
             2 Merging (voxel based) finished after 0s (1 kept)\n"
        );
        assert!(!log.contains('%'));
    }

    #[test]
    fn voxel_negative_coordinates_and_density() {
        // floor(-5.x / 0.5) == -11 on every axis for all three records.
        let a1 = record(-5.4, -5.3, -5.2, 0.1);
        let a2 = record(-5.1, -5.2, -5.4, 0.2);
        let b1 = record(-5.45, -5.45, -5.45, 0.3);
        let (out, n, _, _) =
            run_voxel(&[Some(vec![a1.clone(), a2.clone()]), Some(vec![b1])]).unwrap();
        assert_eq!(n, 2);
        let mut expected = a1;
        expected.extend(a2);
        assert_eq!(body_of(&out), expected);
    }

    #[test]
    fn voxel_three_files_earliest_max_wins() {
        // Voxy(0,0,0): counts 1/2/0 -> file b wins; vox(10,10,10): only c.
        let a1 = record(0.0, 0.0, 0.0, 0.1);
        let b1 = record(0.1, 0.1, 0.1, 0.2);
        let b2 = record(0.2, 0.0, 0.0, 0.3);
        let c1 = record(5.0, 5.0, 5.0, 0.4);
        let (out, n, _, _) = run_voxel(&[
            Some(vec![a1]),
            Some(vec![b1.clone(), b2.clone()]),
            Some(vec![c1.clone()]),
        ])
        .unwrap();
        assert_eq!(n, 3);
        let mut expected = b1;
        expected.extend(b2);
        expected.extend(c1);
        assert_eq!(body_of(&out), expected);
    }

    #[test]
    fn voxel_zero_splat_file_counts_as_fully_processed() {
        // A present-but-empty file still gets a byte-size weight; its zero
        // splats count as fully processed, so both phases close at 100%.
        let a1 = record(0.0, 0.0, 0.0, 0.1);
        let (out, n, lines, _) = run_voxel(&[Some(vec![]), Some(vec![a1.clone()])]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(body_of(&out), a1);
        assert_eq!(
            lines,
            "1 Mapping: 100% (0.0M splats)\n2 Merging (voxel based): 100% (0.0M splats)\n"
        );
    }

    #[test]
    fn output_header_count_matches_written_splats() {
        let a1 = record(0.0, 0.0, 0.0, 0.1);
        let a2 = record(9.0, 9.0, 9.0, 0.2);
        let b1 = record(0.4, 0.2, 0.1, 0.3); // same voxel (0,0,0) as a1
        let (out, n, _, _) = run_voxel(&[Some(vec![a1, a2]), Some(vec![b1])]).unwrap();
        assert_eq!(n, 2);
        let info = parse_ply_header(&out);
        assert_eq!(info.vertex_count, n);
    }

    #[test]
    fn scale_threshold_boundary() {
        // threshold = e, so log-scale 1.0 is the exact `<` vs `>=` boundary.
        let a_small = record(0.0, 0.0, 0.0, 0.9); // exp(0.9) < e -> file 1 keeps
        let a_eq = record(1.0, 0.0, 0.0, 1.0); // exp(1.0) == e -> file 1 drops
        let b_eq = record(2.0, 0.0, 0.0, 1.0); // exp(1.0) == e -> file 2 keeps
        let b_small = record(3.0, 0.0, 0.0, 0.9); // exp(0.9) < e -> file 2 drops
        let (out, n, lines, log) =
            run_scale(&[a_small.clone(), a_eq], Some(&[b_eq.clone(), b_small])).unwrap();
        assert_eq!(n, 2);
        let mut expected = a_small;
        expected.extend(b_eq);
        assert_eq!(body_of(&out), expected);
        // Scale step reports only `3 Cleaning (scale based)` (no `1 Mapping` line).
        assert_eq!(lines, "3 Cleaning (scale based): 100% (0.0M splats)\n");
        // Log: counting marker, kept-count started line, duration — and no
        // progress lines.
        assert_eq!(
            log,
            "3 Cleaning (scale based): counting kept splats\n\
             3 Cleaning (scale based) started (2 of 4 splats kept)\n\
             3 Cleaning (scale based) finished after 0s\n"
        );
        assert!(!log.contains('%'));
    }

    #[test]
    fn scale_empty_file2() {
        let a_small = record(0.0, 0.0, 0.0, 0.9);
        let a_big = record(1.5, 1.5, 1.5, 1.5);
        let (out, n, lines, log) = run_scale(&[a_small.clone(), a_big], None).unwrap();
        assert_eq!(n, 1);
        assert_eq!(body_of(&out), a_small);
        assert_eq!(lines, "3 Cleaning (scale based): 100% (0.0M splats)\n");
        assert!(log.contains("3 Cleaning (scale based) started (1 of 2 splats kept)"));
        assert!(!log.contains('%'));
    }

    #[test]
    fn scale_missing_scale_property_errors() {
        // File 1 declares no scale property -> scale step must fail, not panic.
        let mut body1 = Vec::new();
        for _ in 0..8 {
            body1.extend_from_slice(&0f32.to_le_bytes());
        }
        let mut b1 = "ply\nformat binary_little_endian 1.0\nelement vertex 1\n\
            property float x\nproperty float y\nproperty float z\nproperty float opacity\n\
            property float rot_0\nproperty float rot_1\nproperty float rot_2\nproperty float rot_3\n\
            end_header\n"
            .as_bytes()
            .to_vec();
        b1.extend_from_slice(&body1);
        let i1 = parse_ply_header(&b1);
        assert!(i1.scale_offset.is_none());
        assert_eq!(i1.stride, 32);

        let r2 = record(0.0, 0.0, 0.0, 1.0);
        let b2 = ply(1, &r2);
        let i2 = parse_ply_header(&b2);

        let nc = AtomicBool::new(false);
        let res = merge_scale(
            Some((&b1, &i1)),
            Some((&b2, &i2)),
            0.01,
            &i2.raw_header,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            &nc,
        );
        let err = res.unwrap_err();
        assert_eq!(err.to_string(), "No scale in Input 1");
    }

    #[test]
    fn voxel_abort_writes_nothing() {
        // A pre-set cancel flag must abort before any output byte is written.
        let a1 = record(0.0, 0.0, 0.0, 0.1);
        let b1 = record(0.4, 0.3, 0.2, 0.2);
        let ba = ply(1, &a1);
        let bb = ply(1, &b1);
        let ia = parse_ply_header(&ba);
        let ib = parse_ply_header(&bb);
        let sources: [Option<(&[u8], &PlyInfo)>; 2] = [Some((&ba[..], &ia)), Some((&bb[..], &ib))];
        let cancel = AtomicBool::new(true);
        let mut out = Vec::new();
        let mut prog = Vec::new();
        let mut log = Vec::new();
        let res = merge_voxel(
            &sources,
            0.5,
            &ia.raw_header,
            &mut out,
            &mut prog,
            &mut log,
            &cancel,
        );
        let err = res.unwrap_err();
        assert_eq!(err.to_string(), "Aborted by user");
        assert!(err.downcast_ref::<UserAborted>().is_some());
        assert!(out.is_empty());
        assert!(prog.is_empty());
        // The `1 Mapping started` line is logged before the first record check.
        let logtext = String::from_utf8(log).unwrap();
        assert!(logtext.contains("1 Mapping started"));
        assert!(!logtext.contains("1 Mapping finished"));
    }

    #[test]
    fn scale_abort_writes_nothing() {
        // A pre-set cancel flag must abort during the count, before any
        // output byte is written.
        let a_small = record(0.0, 0.0, 0.0, 0.9);
        let b_big = record(2.0, 2.0, 2.0, 1.5);
        let ba = ply(1, &a_small);
        let bb = ply(1, &b_big);
        let ia = parse_ply_header(&ba);
        let ib = parse_ply_header(&bb);
        let cancel = AtomicBool::new(true);
        let mut out = Vec::new();
        let mut prog = Vec::new();
        let mut log = Vec::new();
        let res = merge_scale(
            Some((&ba[..], &ia)),
            Some((&bb[..], &ib)),
            1.0f32.exp(),
            &ia.raw_header,
            &mut out,
            &mut prog,
            &mut log,
            &cancel,
        );
        let err = res.unwrap_err();
        assert_eq!(err.to_string(), "Aborted by user");
        assert!(out.is_empty());
        assert!(prog.is_empty());
        // The counting marker is logged before the first record check; the
        // started line (which carries the kept count) never comes.
        let logtext = String::from_utf8(log).unwrap();
        assert!(logtext.contains("counting kept splats"));
        assert!(!logtext.contains("started"));
    }
}
