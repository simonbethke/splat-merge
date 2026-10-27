use std::io::{BufRead, Cursor, Write};

/// One `property` of the vertex record, in the order the source header
/// declares them (`property list` lines and unrecognized types are skipped).
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// Property name as declared in the source header.
    pub name: String,
    /// PLY type name as declared (`float`, `uchar`, …).
    pub type_name: String,
    /// Byte size of the property value.
    pub size: usize,
    /// Byte offset of the property within one source record.
    pub offset: usize,
}

/// One segment of a record copy plan: the `size` bytes at offset `src` of
/// the source record go to offset `out` of the common-layout record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seg {
    /// Offset within the source record.
    pub src: usize,
    /// Offset within the common-layout record.
    pub out: usize,
    /// Number of bytes copied.
    pub size: usize,
}

/// The byte size of a PLY property type; `None` for types this tool does not
/// track (their header lines are skipped, as before).
pub fn type_size(type_name: &str) -> Option<usize> {
    match type_name {
        "float" | "int32" | "uint32" | "int" => Some(4),
        "double" | "int64" | "uint64" => Some(8),
        "char" | "uchar" | "int8" | "uint8" => Some(1),
        "short" | "ushort" | "int16" | "uint16" => Some(2),
        _ => None,
    }
}

/// Metadata about a mapped PLY file, extracted from its ASCII header.
pub struct PlyInfo {
    /// Byte offset where the binary vertex data starts.
    pub header_len: usize,
    /// Vertex count declared in the `element vertex` line.
    pub vertex_count: usize,
    /// Stride in bytes of one vertex record.
    pub stride: usize,
    /// The source header (used to re-emit the output header).
    pub raw_header: String,
    /// Byte offset of `scale_0` within a record, if declared.
    pub scale_offset: Option<usize>,
    /// The file's `property` lines, in declared order.
    pub properties: Vec<Property>,
}

/// Scans the ASCII PLY header in `mmap` and derives [`PlyInfo`].
pub fn parse_ply_header(mmap: &[u8]) -> PlyInfo {
    let mut header_len = 0;
    let mut vertex_count = 0;
    let mut stride = 0;
    let mut raw_header = String::new();
    let mut scale_offset = None;
    let mut properties = Vec::new();

    let reader = Cursor::new(mmap);
    for line in reader.lines() {
        let line = line.expect("Failed to read header line");
        let line_len = line.len();
        raw_header.push_str(&line);
        raw_header.push('\n');

        header_len = mmap[header_len..]
            .windows(line_len)
            .position(|w| w == line.as_bytes())
            .map(|p| header_len + p + line_len)
            .unwrap();

        while header_len < mmap.len() && (mmap[header_len] == b'\n' || mmap[header_len] == b'\r') {
            header_len += 1;
        }

        if line.starts_with("element vertex") {
            vertex_count = line.split_whitespace().last().unwrap().parse().unwrap();
        } else if line.starts_with("property") {
            // `property <type> <name>`; `property list …` lines and lines
            // with an unrecognized type carry no tracked bytes (as before).
            let mut parts = line.split_whitespace().skip(1);
            let ty = parts.next().unwrap_or_default();
            let name = parts.next();
            if ty != "list" {
                if let Some((name, size)) = name.zip(type_size(ty)) {
                    if name == "scale_0" {
                        scale_offset = Some(stride);
                    }
                    properties.push(Property {
                        name: name.to_owned(),
                        type_name: ty.to_owned(),
                        size,
                        offset: stride,
                    });
                    stride += size;
                }
            }
        } else if line == "end_header" {
            break;
        }
    }
    PlyInfo {
        header_len,
        vertex_count,
        stride,
        raw_header,
        scale_offset,
        properties,
    }
}

/// Rejects a non-empty file whose binary data section cannot hold the
/// declared records: zero stride, data length not a multiple of the stride,
/// or data truncated below the declared vertex count.
pub fn validate(data: &[u8], info: &PlyInfo) -> Result<(), Box<dyn std::error::Error>> {
    if info.stride == 0 {
        return Err("No vertex properties declared in the PLY header".into());
    }
    let data_len = data.len().saturating_sub(info.header_len);
    if !data_len.is_multiple_of(info.stride) {
        return Err(format!(
            "Data length {} is not a multiple of the record stride {}",
            data_len, info.stride
        )
        .into());
    }
    let actual = data_len / info.stride;
    if actual < info.vertex_count {
        return Err(format!(
            "File holds {} vertex records but the header declares {}",
            actual, info.vertex_count
        )
        .into());
    }
    Ok(())
}

/// Decodes the first 12 bytes of a record as the little-endian f32 position.
/// Assumes position is the first property of the file.
#[inline(always)]
pub fn get_pos(data: &[u8]) -> [f32; 3] {
    [
        f32::from_le_bytes(data[0..4].try_into().unwrap()),
        f32::from_le_bytes(data[4..8].try_into().unwrap()),
        f32::from_le_bytes(data[8..12].try_into().unwrap()),
    ]
}

/// Decodes `scale_0..2` at `offset` and returns the max of the three
/// log scale values, exponentiated to world space.
#[inline(always)]
pub fn get_max_scale(data: &[u8], offset: usize) -> f32 {
    let s0 = f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let s1 = f32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
    let s2 = f32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap());
    s0.max(s1).max(s2).exp()
}

/// The common layout of the non-empty data sources: the properties present
/// in every one, in the first source's order, plus the dropped property
/// names in first-source order (for the notification / confirmation).
/// Errors, all before any output file may be created: no sources at all;
/// a source not declaring `x`, `y`, `z` as its first three properties; or a
/// property present in every source declared with a different byte size —
/// naming the property and both types.
pub fn common_layout(infos: &[&PlyInfo]) -> Result<(Vec<Property>, Vec<String>), String> {
    if infos.is_empty() {
        return Err(
            "All data sources are '-' (empty); provide at least one real PLY file".to_owned(),
        );
    }
    for i in infos {
        let first3: Vec<&str> = i
            .properties
            .iter()
            .take(3)
            .map(|p| p.name.as_str())
            .collect();
        if first3 != ["x", "y", "z"] {
            return Err("Every file must declare x, y, z as the first three properties".to_owned());
        }
    }
    let first = &infos[0];
    let mut common = Vec::with_capacity(first.properties.len());
    let mut dropped = Vec::new();
    // Offsets are re-derived cumulatively: a property dropped from an
    // earlier source would otherwise leave gaps at the source's offsets.
    let mut off = 0usize;
    for p in &first.properties {
        let mut in_all = true;
        let mut conflict: Option<(String, String)> = None;
        for i in infos {
            match i.properties.iter().find(|q| q.name == p.name) {
                None => in_all = false,
                Some(q) if q.size != p.size && conflict.is_none() => {
                    conflict = Some((p.type_name.clone(), q.type_name.clone()));
                }
                Some(_) => {}
            }
        }
        if in_all {
            if let Some((ta, tb)) = conflict {
                return Err(format!(
                    "Property '{}' has conflicting PLY types between files ({} vs {})",
                    p.name, ta, tb
                ));
            }
            let mut q = p.clone();
            q.offset = off;
            off += q.size;
            common.push(q);
        } else {
            dropped.push(p.name.clone());
        }
    }
    Ok((common, dropped))
}

/// The copy plan of one source onto the common layout: one segment per
/// common property (its offset in the source record and in the common
/// record). All common properties are present in every source by
/// construction, so the plan always covers the full common record.
pub fn map_layout(source: &PlyInfo, common: &[Property]) -> Vec<Seg> {
    let mut segs = Vec::with_capacity(common.len());
    for p in common {
        if let Some(q) = source.properties.iter().find(|q| q.name == p.name) {
            segs.push(Seg {
                src: q.offset,
                out: p.offset,
                size: p.size,
            });
        }
    }
    segs
}

/// Whether a copy plan is an identity (each segment at its output offset,
/// i.e. the source layout already equals the common layout).
pub fn is_identity(segs: &[Seg]) -> bool {
    segs.iter().all(|s| s.src == s.out)
}

/// Writes the reconstructed output header: every line of the template is
/// emitted unchanged except `element vertex <count>` (rewritten with
/// `count`) and — when `keep` is `Some` — the `property` lines whose name
/// (last whitespace token) is not in `keep`; `None` keeps every line
/// (passthrough).
pub fn rewrite_header<W: Write>(
    writer: &mut W,
    template: &str,
    count: usize,
    keep: Option<&[&str]>,
) -> std::io::Result<()> {
    for line in template.lines() {
        let dropped = line.starts_with("property")
            && keep.is_some_and(|k| {
                line.split_whitespace()
                    .last()
                    .is_some_and(|n| !k.contains(&n))
            });
        if line.starts_with("element vertex") {
            writeln!(writer, "element vertex {}", count)?;
        } else if dropped {
            // property line of a dropped property: removed from the output.
        } else {
            writeln!(writer, "{}", line)?;
        }
    }
    Ok(())
}
