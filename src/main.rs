// Link as a GUI program on Windows so double-clicking (no arguments) does
// not pop a console window; ignored on other platforms.
#![windows_subsystem = "windows"]

mod gui;
mod merge;
mod pipeline;
mod ply;
mod progress;

use clap::Parser;

/// Merge or filter 3D Gaussian Splatting PLY files.
#[derive(Parser)]
struct Args {
    /// Comma-separated list of input PLY files; an entry may be `-` (empty).
    #[arg(short = 'i', long)]
    inputs: String,
    /// Optional base PLY (final scale step); may be `-` (empty).
    #[arg(long)]
    base: Option<String>,
    /// Output PLY.
    #[arg(short, long)]
    output: String,
    /// Cube edge length for the voxel steps.
    #[arg(short, long, default_value_t = 0.5)]
    voxel_size: f32,
    /// Max-scale threshold for the scale step.
    #[arg(short = 't', long, default_value_t = 0.01)]
    threshold: f32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // No arguments: the GUI front end (see the "Invocation" section of the
    // documentation); otherwise CLI mode.
    if std::env::args().count() == 1 {
        gui::launch()
    } else {
        let args = Args::parse();
        pipeline::run(&args)
    }
}
