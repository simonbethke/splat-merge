use clap::{Parser, ValueEnum};
use memmap2::Mmap;
use std::fs::File;
use std::io::{Write, BufWriter, BufRead, Cursor};
use hashbrown::HashMap;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Mode {
    Voxel,
    Scale,
}

#[derive(Parser)]
struct Args {
    #[arg(short = 'i', long)]
    input1: String,
    #[arg(short = 'j', long)]
    input2: String,
    #[arg(short, long)]
    output: String,
    #[arg(short, long, value_enum, default_value_t = Mode::Voxel)]
    mode: Mode,
    #[arg(short, long, default_value_t = 0.5)]
    voxel_size: f32,
    #[arg(short = 't', long, default_value_t = 0.1)]
    threshold: f32,
}

struct PlyInfo {
    header_len: usize,
    vertex_count: usize,
    stride: usize,
    raw_header: String,
    scale_offset: Option<usize>,
}

fn parse_ply_header(mmap: &[u8]) -> PlyInfo {
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
            if line.ends_with("scale_0") { scale_offset = Some(stride); }
            if line.contains("float") { stride += 4; }
            else if line.contains("double") { stride += 8; }
            else if line.contains("uchar") || line.contains("uint8") { stride += 1; }
        } else if line == "end_header" { break; }
    }
    PlyInfo { header_len, vertex_count, stride, raw_header, scale_offset }
}

#[inline(always)]
fn get_pos(data: &[u8]) -> [f32; 3] {
    [
        f32::from_le_bytes(data[0..4].try_into().unwrap()),
        f32::from_le_bytes(data[4..8].try_into().unwrap()),
        f32::from_le_bytes(data[8..12].try_into().unwrap()),
    ]
}

#[inline(always)]
fn get_max_scale(data: &[u8], offset: usize) -> f32 {
    let s0 = f32::from_le_bytes(data[offset..offset+4].try_into().unwrap());
    let s1 = f32::from_le_bytes(data[offset+4..offset+8].try_into().unwrap());
    let s2 = f32::from_le_bytes(data[offset+8..offset+12].try_into().unwrap());
    s0.max(s1).max(s2).exp()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Helper to map file or return None if path is "-"
    let map_file = |path: &str| -> Result<Option<(Mmap, PlyInfo)>, Box<dyn std::error::Error>> {
        if path == "-" { return Ok(None); }
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let info = parse_ply_header(&mmap);
        Ok(Some((mmap, info)))
    };

    let input1 = map_file(&args.input1)?;
    let input2 = map_file(&args.input2)?;

    // Determine stride and header from whichever file is available
    let (stride, header_template) = match (&input1, &input2) {
        (Some((_, i1)), Some((_, i2))) => {
            if i1.stride != i2.stride { return Err("Stride mismatch".into()); }
            (i1.stride, &i1.raw_header)
        }
        (Some((_, i1)), None) => (i1.stride, &i1.raw_header),
        (None, Some((_, i2))) => (i2.stride, &i2.raw_header),
        (None, None) => return Err("Both inputs cannot be '-'".into()),
    };

    let mut kept1 = Vec::new();
    let mut kept2 = Vec::new();

    match args.mode {
        Mode::Voxel => {
            let mut dens1 = HashMap::new();
            let mut dens2 = HashMap::new();

            if let Some((m, i)) = &input1 {
                for idx in 0..i.vertex_count {
                    let p = get_pos(&m[i.header_len + (idx * stride)..]);
                    *dens1.entry(((p[0]/args.voxel_size).floor() as i32, (p[1]/args.voxel_size).floor() as i32, (p[2]/args.voxel_size).floor() as i32)).or_insert(0) += 1;
                }
            }
            if let Some((m, i)) = &input2 {
                for idx in 0..i.vertex_count {
                    let p = get_pos(&m[i.header_len + (idx * stride)..]);
                    *dens2.entry(((p[0]/args.voxel_size).floor() as i32, (p[1]/args.voxel_size).floor() as i32, (p[2]/args.voxel_size).floor() as i32)).or_insert(0) += 1;
                }
            }
            if let Some((m, i)) = &input1 {
                for idx in 0..i.vertex_count {
                    let p = get_pos(&m[i.header_len + (idx * stride)..]);
                    let v = ((p[0]/args.voxel_size).floor() as i32, (p[1]/args.voxel_size).floor() as i32, (p[2]/args.voxel_size).floor() as i32);
                    if dens1[&v] >= *dens2.get(&v).unwrap_or(&0) { kept1.push(idx); }
                }
            }
            if let Some((m, i)) = &input2 {
                for idx in 0..i.vertex_count {
                    let p = get_pos(&m[i.header_len + (idx * stride)..]);
                    let v = ((p[0]/args.voxel_size).floor() as i32, (p[1]/args.voxel_size).floor() as i32, (p[2]/args.voxel_size).floor() as i32);
                    if dens2[&v] > *dens1.get(&v).unwrap_or(&0) { kept2.push(idx); }
                }
            }
        }
        Mode::Scale => {
            let threshold = args.threshold;
            if let Some((m, i)) = &input1 {
                let off = i.scale_offset.ok_or("No scale in Input 1")?;
                for idx in 0..i.vertex_count {
                    if get_max_scale(&m[i.header_len + (idx * stride)..], off) < threshold { kept1.push(idx); }
                }
            }
            if let Some((m, i)) = &input2 {
                let off = i.scale_offset.ok_or("No scale in Input 2")?;
                for idx in 0..i.vertex_count {
                    if get_max_scale(&m[i.header_len + (idx * stride)..], off) >= threshold { kept2.push(idx); }
                }
            }
        }
    }

    let total = kept1.len() + kept2.len();
    let mut writer = BufWriter::new(File::create(&args.output)?);
    for line in header_template.lines() {
        if line.starts_with("element vertex") { writeln!(writer, "element vertex {}", total)?; }
        else { writeln!(writer, "{}", line)?; }
    }

    if let Some((m, i)) = &input1 {
        for idx in kept1 { writer.write_all(&m[i.header_len + (idx * stride)..i.header_len + (idx * stride) + stride])?; }
    }
    if let Some((m, i)) = &input2 {
        for idx in kept2 { writer.write_all(&m[i.header_len + (idx * stride)..i.header_len + (idx * stride) + stride])?; }
    }

    println!("Success: {} splats saved to {}", total, args.output);
    Ok(())
}