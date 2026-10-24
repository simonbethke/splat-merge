use hashbrown::HashMap;
use std::io::Write;

use crate::ply::{get_max_scale, get_pos, rewrite_header, PlyInfo};

/// How one pairwise merge decides which splats to keep.
#[derive(Clone, Copy)]
pub enum MergeMode {
    /// Voxel (density) merge: per voxel, the input with more splats wins.
    Voxel { voxel_size: f32 },
    /// Scale (size) merge: split by each splat's max world-space scale.
    Scale { threshold: f32 },
}

/// Voxel index key for a position: `floor(p / voxel_size)` per axis.
#[inline(always)]
fn voxel_key(p: [f32; 3], voxel_size: f32) -> (i32, i32, i32) {
    (
        (p[0] / voxel_size).floor() as i32,
        (p[1] / voxel_size).floor() as i32,
        (p[2] / voxel_size).floor() as i32,
    )
}

/// One pairwise merge of two (possibly empty) in-memory inputs.
///
/// Each source is a byte view of a full PLY file plus its parsed info.
/// Writes the reconstructed header (from `header`, rewritten with the final
/// count) followed by the kept records byte-exact, file 1's splats first.
/// Returns the number of splats written.
///
/// Keep rules (preserved semantics):
/// - Voxel: file 1 kept where `count1 >= count2`, file 2 kept where `count2 > count1`.
/// - Scale: file 1 kept where max scale `< threshold`, file 2 where `>= threshold`.
pub fn merge_pair(
    src1: Option<(&[u8], &PlyInfo)>,
    src2: Option<(&[u8], &PlyInfo)>,
    mode: MergeMode,
    header: &str,
    out: &mut impl Write,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut kept1 = Vec::new();
    let mut kept2 = Vec::new();

    match mode {
        MergeMode::Voxel { voxel_size } => {
            let mut dens1 = HashMap::new();
            let mut dens2 = HashMap::new();

            if let Some((m, i)) = src1 {
                for idx in 0..i.vertex_count {
                    let p = get_pos(&m[i.header_len + idx * i.stride..]);
                    *dens1.entry(voxel_key(p, voxel_size)).or_insert(0) += 1;
                }
            }
            if let Some((m, i)) = src2 {
                for idx in 0..i.vertex_count {
                    let p = get_pos(&m[i.header_len + idx * i.stride..]);
                    *dens2.entry(voxel_key(p, voxel_size)).or_insert(0) += 1;
                }
            }
            if let Some((m, i)) = src1 {
                for idx in 0..i.vertex_count {
                    let p = get_pos(&m[i.header_len + idx * i.stride..]);
                    let v = voxel_key(p, voxel_size);
                    if dens1[&v] >= *dens2.get(&v).unwrap_or(&0) {
                        kept1.push(idx);
                    }
                }
            }
            if let Some((m, i)) = src2 {
                for idx in 0..i.vertex_count {
                    let p = get_pos(&m[i.header_len + idx * i.stride..]);
                    let v = voxel_key(p, voxel_size);
                    if dens2[&v] > *dens1.get(&v).unwrap_or(&0) {
                        kept2.push(idx);
                    }
                }
            }
        }
        MergeMode::Scale { threshold } => {
            if let Some((m, i)) = src1 {
                let off = i.scale_offset.ok_or("No scale in Input 1")?;
                for idx in 0..i.vertex_count {
                    if get_max_scale(&m[i.header_len + idx * i.stride..], off) < threshold {
                        kept1.push(idx);
                    }
                }
            }
            if let Some((m, i)) = src2 {
                let off = i.scale_offset.ok_or("No scale in Input 2")?;
                for idx in 0..i.vertex_count {
                    if get_max_scale(&m[i.header_len + idx * i.stride..], off) >= threshold {
                        kept2.push(idx);
                    }
                }
            }
        }
    }

    let total = kept1.len() + kept2.len();
    rewrite_header(out, header, total)?;
    copy_records(out, src1, &kept1)?;
    copy_records(out, src2, &kept2)?;
    Ok(total)
}

/// Byte-exact copy of the kept records of one source into `out`.
fn copy_records(
    out: &mut impl Write,
    src: Option<(&[u8], &PlyInfo)>,
    kept: &[usize],
) -> std::io::Result<()> {
    if let Some((m, i)) = src {
        for idx in kept {
            let start = i.header_len + idx * i.stride;
            out.write_all(&m[start..start + i.stride])?;
        }
    }
    Ok(())
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

    /// Run one pairwise merge over in-memory inputs; returns output bytes and count.
    fn run_pair(
        recs1: &[Vec<u8>],
        recs2: &[Vec<u8>],
        mode: MergeMode,
    ) -> Result<(Vec<u8>, usize), Box<dyn std::error::Error>> {
        let body1: Vec<u8> = recs1.iter().flatten().copied().collect();
        let body2: Vec<u8> = recs2.iter().flatten().copied().collect();
        let b1 = ply(recs1.len(), &body1);
        let b2 = ply(recs2.len(), &body2);
        let i1 = parse_ply_header(&b1);
        let i2 = parse_ply_header(&b2);
        let mut out = Vec::new();
        let n = merge_pair(
            Some((&b1, &i1)),
            Some((&b2, &i2)),
            mode,
            &i1.raw_header,
            &mut out,
        )?;
        Ok((out, n))
    }

    /// The body of a merge output (everything after the parsed header).
    fn body_of(out: &[u8]) -> Vec<u8> {
        let i = parse_ply_header(out);
        out[i.header_len..].to_vec()
    }

    #[test]
    fn voxel_tie_keeps_file1() {
        let a1 = record(0.0, 0.0, 0.0, 0.1);
        let b1 = record(0.4, 0.3, 0.2, 0.2); // same voxel (0,0,0) as a1
        let (out, n) = run_pair(
            std::slice::from_ref(&a1),
            std::slice::from_ref(&b1),
            MergeMode::Voxel { voxel_size: 0.5 },
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(body_of(&out), a1);
    }

    #[test]
    fn voxel_negative_coordinates_and_density() {
        // floor(-5.x / 0.5) == -11 on every axis for all three records.
        let a1 = record(-5.4, -5.3, -5.2, 0.1);
        let a2 = record(-5.1, -5.2, -5.4, 0.2);
        let b1 = record(-5.45, -5.45, -5.45, 0.3);
        let (out, n) = run_pair(
            &[a1.clone(), a2.clone()],
            std::slice::from_ref(&b1),
            MergeMode::Voxel { voxel_size: 0.5 },
        )
        .unwrap();
        assert_eq!(n, 2);
        let mut expected = a1;
        expected.extend(a2);
        assert_eq!(body_of(&out), expected);
    }

    #[test]
    fn scale_threshold_boundary() {
        // threshold = e, so log-scale 1.0 is the exact `<` vs `>=` boundary.
        let t = 1.0f32.exp();
        let a_small = record(0.0, 0.0, 0.0, 0.9); // exp(0.9) < e -> file 1 keeps
        let a_eq = record(1.0, 0.0, 0.0, 1.0); // exp(1.0) == e -> file 1 drops
        let b_eq = record(2.0, 0.0, 0.0, 1.0); // exp(1.0) == e -> file 2 keeps
        let b_small = record(3.0, 0.0, 0.0, 0.9); // exp(0.9) < e -> file 2 drops
        let (out, n) = run_pair(
            &[a_small.clone(), a_eq.clone()],
            &[b_eq.clone(), b_small.clone()],
            MergeMode::Scale { threshold: t },
        )
        .unwrap();
        assert_eq!(n, 2);
        let mut expected = a_small;
        expected.extend(b_eq);
        assert_eq!(body_of(&out), expected);
    }

    #[test]
    fn scale_empty_file2() {
        let t = 1.0f32.exp();
        let a_small = record(0.0, 0.0, 0.0, 0.9);
        let a_big = record(1.5, 1.5, 1.5, 1.5);
        let (out, n) = run_pair(
            &[a_small.clone(), a_big.clone()],
            &[],
            MergeMode::Scale { threshold: t },
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(body_of(&out), a_small);
    }

    #[test]
    fn output_header_count_matches_written_splats() {
        let a1 = record(0.0, 0.0, 0.0, 0.1);
        let a2 = record(9.0, 9.0, 9.0, 0.2);
        let b1 = record(0.4, 0.2, 0.1, 0.3); // same voxel (0,0,0) as a1
        let (out, n) = run_pair(
            &[a1.clone(), a2.clone()],
            std::slice::from_ref(&b1),
            MergeMode::Voxel { voxel_size: 0.5 },
        )
        .unwrap();
        assert_eq!(n, 2);
        let info = parse_ply_header(&out);
        assert_eq!(info.vertex_count, n);
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

        let res = merge_pair(
            Some((&b1, &i1)),
            Some((&b2, &i2)),
            MergeMode::Scale { threshold: 0.01 },
            &i2.raw_header,
            &mut Vec::new(),
        );
        assert!(res.is_err());
    }
}
