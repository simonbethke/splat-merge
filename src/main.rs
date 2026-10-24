mod merge;
mod pipeline;
mod ply;

use clap::{Parser, ValueEnum};

/// Merge or filter 3D Gaussian Splatting PLY files.
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    pipeline::run(&args)
}
