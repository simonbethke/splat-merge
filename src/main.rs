// Link as a GUI program on Windows so double-clicking (no arguments) does
// not pop a console window; ignored on other platforms.
#![windows_subsystem = "windows"]

mod debug_render;
mod gui;
mod logfile;
mod merge;
mod pipeline;
mod ply;
mod progress;

use clap::Parser;

/// The axes defining the voxel cells (see the "Modes — voxel" section of the
/// documentation).
///
/// A bit mask with the bits `X` = 1, `Y` = 2, `Z` = 4; the default has all
/// three axes enabled. In the CLI runs this is always the default (no flag);
/// the GUI checkboxes set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoxelAxes {
    mask: u8,
}

impl VoxelAxes {
    /// All three axes enabled.
    pub const fn all() -> Self {
        Self { mask: 0b111 }
    }

    /// Build the set from the X/Y/Z checkbox states (in that order).
    pub const fn from_flags(x: bool, y: bool, z: bool) -> Self {
        Self {
            mask: (x as u8) | ((y as u8) << 1) | ((z as u8) << 2),
        }
    }

    /// The bit mask (`X` = 1, `Y` = 2, `Z` = 4).
    pub const fn mask(&self) -> u8 {
        self.mask
    }

    /// Whether at least one axis is enabled.
    pub const fn any(&self) -> bool {
        self.mask != 0
    }

    /// The compact enabled-axis set for display and the `Run started`
    /// log line (`XYZ`, `XZ`, `Y`, …).
    pub const fn label(&self) -> &'static str {
        match self.mask {
            0b001 => "X",
            0b010 => "Y",
            0b100 => "Z",
            0b011 => "XY",
            0b101 => "XZ",
            0b110 => "YZ",
            0b111 => "XYZ",
            _ => "",
        }
    }
}

impl Default for VoxelAxes {
    fn default() -> Self {
        Self::all()
    }
}

/// Merge or filter 3D Gaussian Splatting PLY files (see the "CLI" section of
/// the documentation); `--debug-render` selects debug rendering (see "Debug
/// rendering") and conflicts with every pipeline flag.
///
/// The pipeline file arguments are `Option`: clap derive auto-marks
/// non-`Option` fields as required, which would override
/// `required_unless_present` and reject `--debug-render` alone.
#[derive(Parser, Debug)]
struct Cli {
    /// Comma-separated list of input PLY files; an entry may be `-` (empty).
    /// Required for a pipeline run; not for `--debug-render`.
    #[arg(short = 'i', long, required_unless_present = "debug_render")]
    inputs: Option<String>,
    /// Optional base PLY (final scale step); may be `-` (empty).
    #[arg(long)]
    base: Option<String>,
    /// Output PLY. Required for a pipeline run; not for `--debug-render`.
    #[arg(short, long, required_unless_present = "debug_render")]
    output: Option<String>,
    /// Cube edge length for the voxel steps.
    #[arg(short, long, default_value_t = 0.5)]
    voxel_size: f32,
    /// Max-scale threshold for the scale step.
    #[arg(short = 't', long, default_value_t = 0.01)]
    threshold: f32,
    /// GUI state to render to `screenshot.jpg` (see the "Debug rendering"
    /// section of the documentation); conflicts with every pipeline flag.
    #[arg(
        long,
        conflicts_with_all = ["inputs", "base", "output", "voxel_size", "threshold"]
    )]
    debug_render: Option<String>,
}

/// The pipeline settings (see "Architecture"): the plain struct the CLI
/// assembles from `Cli` and the GUI's run start builds from its checkboxes;
/// `voxel_axes` carries the `VoxelAxes` mask for the run.
struct Args {
    inputs: String,
    base: Option<String>,
    output: String,
    voxel_size: f32,
    threshold: f32,
    voxel_axes: VoxelAxes,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // No arguments: the GUI front end (see the "Invocation" section of the
    // documentation); otherwise parse the command line.
    if std::env::args().count() == 1 {
        return gui::launch();
    }
    let cli = Cli::parse();
    if let Some(state) = cli.debug_render {
        // Debug rendering (docs "Debug rendering"): one clear error line on
        // stderr and a non-zero exit on failure.
        if let Err(msg) = debug_render::run(std::path::Path::new(&state)) {
            eprintln!("{msg}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // `required_unless_present` makes both present in a pipeline run, so the
    // `None` arm is unreachable; still, error instead of panicking.
    let inputs = cli
        .inputs
        .ok_or("`--inputs` is required for a pipeline run")?;
    let output = cli
        .output
        .ok_or("`--output` is required for a pipeline run")?;
    pipeline::run(&Args {
        inputs,
        base: cli.base,
        output,
        voxel_size: cli.voxel_size,
        threshold: cli.threshold,
        // CLI runs always merge on all three axes (docs "GUI", "CLI").
        voxel_axes: VoxelAxes::all(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    /// `--debug-render` combined with a pipeline flag fails at parse time
    /// (docs "Required test cases").
    #[test]
    fn debug_render_conflicts_with_the_pipeline_flags() {
        for (flag, value) in [
            ("-i", "a.ply"),
            ("--base", "a.ply"),
            ("-o", "a.ply"),
            ("-v", "0.3"),
            ("-t", "0.02"),
        ] {
            let err = Cli::try_parse_from(["splat-merge", "--debug-render", "s.json", flag, value])
                .expect_err("--debug-render with a pipeline flag must fail at parse time");
            assert_eq!(
                err.kind(),
                ErrorKind::ArgumentConflict,
                "a conflict error is expected, got: {err}"
            );
        }
    }

    #[test]
    fn debug_render_alone_parses() {
        let cli = Cli::try_parse_from(["splat-merge", "--debug-render", "s.json"])
            .expect("--debug-render alone must parse");
        assert_eq!(cli.debug_render.as_deref(), Some("s.json"));
        assert!(cli.inputs.is_none());
        assert!(cli.base.is_none());
        assert!(cli.output.is_none());
    }

    #[test]
    fn the_pipeline_mode_requires_inputs_and_output() {
        let argvs: [&[&str]; 3] = [
            &["splat-merge"][..],
            &["splat-merge", "-i", "a.ply"][..],
            &["splat-merge", "-o", "b.ply"][..],
        ];
        for argv in argvs {
            let err = Cli::try_parse_from(argv).expect_err("a pipeline run must name both files");
            assert_eq!(
                err.kind(),
                ErrorKind::MissingRequiredArgument,
                "missing-required is expected, got: {err}"
            );
        }
    }
}
