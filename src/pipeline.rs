use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Write};

use crate::merge::{self, MergeMode};
use crate::ply::parse_ply_header;
use crate::{Args, Mode};

/// Maps `path` for reading; `-` denotes an empty input.
fn map_file(path: &str) -> Result<Option<(Mmap, crate::ply::PlyInfo)>, Box<dyn std::error::Error>> {
    if path == "-" {
        return Ok(None);
    }
    let file = File::open(path)?;
    // SAFETY: memmap2 maps the file read-only; the mapping stays alive for the
    // duration of the return value.
    let mmap = unsafe { Mmap::map(&file)? };
    let info = parse_ply_header(&mmap);
    Ok(Some((mmap, info)))
}

/// Runs the full merge described by `args` and writes the output file.
pub fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let input1 = map_file(&args.input1)?;
    let input2 = map_file(&args.input2)?;

    // Determine stride and header from whichever file is available
    let (_, header_template) = match (&input1, &input2) {
        (Some((_, i1)), Some((_, i2))) => {
            if i1.stride != i2.stride {
                return Err("Stride mismatch".into());
            }
            (i1.stride, &i1.raw_header)
        }
        (Some((_, i1)), None) => (i1.stride, &i1.raw_header),
        (None, Some((_, i2))) => (i2.stride, &i2.raw_header),
        (None, None) => return Err("Both inputs cannot be '-'".into()),
    };

    let mode = match args.mode {
        Mode::Voxel => MergeMode::Voxel {
            voxel_size: args.voxel_size,
        },
        Mode::Scale => MergeMode::Scale {
            threshold: args.threshold,
        },
    };

    let mut writer = BufWriter::new(File::create(&args.output)?);
    let total = merge::merge_pair(
        input1.as_ref().map(|(m, i)| (m, i)),
        input2.as_ref().map(|(m, i)| (m, i)),
        mode,
        header_template,
        &mut writer,
    )?;
    writer.flush()?;

    println!("Success: {} splats saved to {}", total, args.output);
    Ok(())
}
