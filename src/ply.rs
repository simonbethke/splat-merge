use std::io::{BufRead, Cursor, Write};

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
}

/// Scans the ASCII PLY header in `mmap` and derives [`PlyInfo`].
pub fn parse_ply_header(mmap: &[u8]) -> PlyInfo {
    let mut header_len = 0;
    let mut vertex_count = 0;
    let mut stride = 0;
    let mut raw_header = String::new();
    let mut scale_offset = None;

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
            if line.ends_with("scale_0") {
                scale_offset = Some(stride);
            }
            if line.contains("float") {
                stride += 4;
            } else if line.contains("double") {
                stride += 8;
            } else if line.contains("uchar") || line.contains("uint8") {
                stride += 1;
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

/// Writes the reconstructed output header: every line of the template is
/// emitted unchanged except `element vertex <count>`, which is rewritten
/// with `count`.
pub fn rewrite_header<W: Write>(
    writer: &mut W,
    template: &str,
    count: usize,
) -> std::io::Result<()> {
    for line in template.lines() {
        if line.starts_with("element vertex") {
            writeln!(writer, "element vertex {}", count)?;
        } else {
            writeln!(writer, "{}", line)?;
        }
    }
    Ok(())
}
