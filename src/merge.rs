use hashbrown::HashMap;
use memmap2::Mmap;
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

/// One pairwise merge of two (possibly empty) inputs.
///
/// Writes the reconstructed header (from `header`, rewritten with the final
/// count) followed by the kept records byte-exact, file 1's splats first.
/// Returns the number of splats written.
///
/// Keep rules (preserved semantics):
/// - Voxel: file 1 kept where `count1 >= count2`, file 2 kept where `count2 > count1`.
/// - Scale: file 1 kept where max scale `< threshold`, file 2 where `>= threshold`.
pub fn merge_pair(
    src1: Option<(&Mmap, &PlyInfo)>,
    src2: Option<(&Mmap, &PlyInfo)>,
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
    src: Option<(&Mmap, &PlyInfo)>,
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
