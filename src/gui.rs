//! The GUI front end (docs section "GUI"): a small eframe/egui window
//! launched when the binary is run without arguments. All control logic
//! (`.ply` filtering, input validation, the line sink for the worker) is
//! here and unit-tested; the heavy work runs on a worker thread through
//! `pipeline::run_with`.
//!
//! The screen itself is a pure function of [`GuiState`], so the same
//! rendering is available to `crate::debug_render`, which renders a
//! documented GUI state from a JSON file (docs section "Debug rendering").

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;
use memmap2::Mmap;

use crate::ply::{common_layout, parse_ply_header, PlyInfo};

/// Launches the GUI window; runs until the window is closed.
pub fn launch() -> Result<(), Box<dyn std::error::Error>> {
    let mut options = eframe::NativeOptions::default();
    options.viewport.inner_size = Some(egui::vec2(640.0, 540.0));
    options.viewport.min_inner_size = Some(egui::vec2(480.0, 360.0));
    eframe::run_native(
        "splat-merge",
        options,
        Box::new(|cc| {
            apply_theme(cc);
            Ok(Box::new(Gui::default()))
        }),
    )?;
    Ok(())
}

/// Applies the window theme once at startup (docs "GUI"): dark theme, 8px
/// spacing between elements, and a visible 1px border around every
/// interactive widget (list rows, text inputs, buttons, choosers). Shared
/// with the debug-render window (docs "Debug rendering").
pub(crate) fn apply_theme(cc: &eframe::CreationContext<'_>) {
    let mut style = egui::Theme::Dark.default_style();
    let border = egui::Stroke::new(1.0, egui::Color32::from_gray(110));
    for visuals in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        visuals.bg_stroke = border;
    }
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
    cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
}

/// The screen state the GUI renders from (docs section "GUI" and "Debug
/// rendering"): the current settings plus the progress-table percentages and
/// the run flag. The interactive window builds one from
/// [`GuiState::default`] and edits it as the user interacts;
/// `crate::debug_render` builds one from a JSON state file. Both paths then
/// render through the same widgets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GuiState {
    /// Input file list, in order (drives `Args.inputs`).
    pub(crate) inputs: Vec<String>,
    /// Selected list entry, or `None` (reset when the list changes).
    pub(crate) selected: Option<usize>,
    /// Base file path text; empty means no base file.
    pub(crate) base: String,
    /// Cube edge length for the voxel steps (same default as the CLI, docs
    /// section "CLI").
    pub(crate) voxel_size: f32,
    /// Voxel-axis checkboxes "Voxels: X", "Y", "Z" (X, Y, Z state; checked by
    /// default; an unchecked axis is not part of the cell key — docs "GUI"
    /// layout 1 and "Modes — voxel").
    pub(crate) axes: [bool; 3],
    /// Max-scale threshold for the scale step (same default as the CLI, docs
    /// section "CLI").
    pub(crate) threshold: f32,
    /// Output PLY path text.
    pub(crate) output: String,
    /// Progress-table state: 0–100% per column (Mapping, Merging, Cleaning).
    pub(crate) progress: [u32; 3],
    /// A pipeline run is active: controls disabled, button reads "Abort".
    pub(crate) running: bool,
}

impl Default for GuiState {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            selected: None,
            base: String::new(),
            // Same defaults as the CLI (docs section "CLI").
            voxel_size: 0.5,
            // All axes on (docs "GUI" layout 1; "Modes — voxel").
            axes: [true; 3],
            threshold: 0.01,
            output: String::new(),
            progress: [0; 3],
            running: false,
        }
    }
}

/// The eframe application: owns the UI state and runs the pipeline on a
/// worker thread.
pub(crate) struct Gui {
    /// The screen state (settings, progress, run flag).
    state: GuiState,
    /// Abort was confirmed; the dialog must not be shown again this run.
    aborting: bool,
    /// Shared with the worker thread; set on a confirmed abort.
    cancel: Arc<AtomicBool>,
    /// The worker thread, if one was started (taken when it finishes).
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
    /// Incoming pipeline lines (progress and messages), if a worker was started.
    rx: Option<mpsc::Receiver<String>>,
}

impl GuiState {
    /// True when the "Remove" button should be enabled: a list entry is
    /// selected and that entry still exists.
    fn remove_enabled(&self) -> bool {
        self.selected.is_some_and(|i| i < self.inputs.len())
    }
}

impl Default for Gui {
    fn default() -> Self {
        Self {
            state: GuiState::default(),
            aborting: false,
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
            rx: None,
        }
    }
}

/// True if `path` is a `*.ply` file (case-insensitive extension, non-empty
/// stem; `a.ply.bak`, `txt.log` and extension-less names are not).
fn is_ply(path: &Path) -> bool {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case("ply"))
}

impl Gui {
    /// Builds the app around a given screen state (used by
    /// `crate::debug_render`); interactive widgets still behave as in the
    /// window, but the window never runs a pipeline in that mode.
    pub(crate) fn new_with(state: GuiState) -> Self {
        Self {
            state,
            ..Self::default()
        }
    }

    /// Adds `path` to the input list; non-`.ply` files and duplicates
    /// (case-insensitive path comparison) are not added and yield the
    /// rejection message for the caller to show in a dialog.
    fn add_input(&mut self, path: &Path) -> Option<String> {
        let display = path.display().to_string();
        if !is_ply(path) {
            return Some(format!("Rejected (not a .ply file): {display}"));
        }
        let incoming = path.to_string_lossy().into_owned();
        if self
            .state
            .inputs
            .iter()
            .any(|stored| stored.eq_ignore_ascii_case(&incoming))
        {
            return Some(format!("Already added: {display}"));
        }
        self.state.inputs.push(incoming);
        None
    }

    /// Opens the multi-select native picker and adds every chosen file.
    fn pick_inputs(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("PLY files", &["ply"])
            .pick_files()
        {
            for path in paths {
                if let Some(reason) = self.add_input(&path) {
                    show_message(rfd::MessageLevel::Warning, &reason);
                }
            }
        }
    }

    /// Validates the settings, returning the first failed rule as the dialog
    /// message (`None` when valid). Split from `start_run` so the rejections
    /// are testable without spawning the worker thread.
    fn start_run_error(&self) -> Option<String> {
        if self.state.inputs.is_empty() {
            return Some("At least one input file is required.".to_owned());
        }
        if self.state.output.trim().is_empty() {
            return Some("An output file is required.".to_owned());
        }
        if !is_ply(Path::new(&self.state.output)) {
            return Some("The output file must have a .ply extension.".to_owned());
        }
        if self.state.voxel_size <= 0.0 || self.state.voxel_size.is_nan() {
            return Some("Voxel size must be greater than zero.".to_owned());
        }
        if self.state.threshold < 0.0 || self.state.threshold.is_nan() {
            return Some("The threshold must be zero or greater.".to_owned());
        }
        if !self.state.axes.iter().any(|&on| on) {
            return Some("At least one voxel axis (X, Y or Z) must be checked.".to_owned());
        }
        None
    }

    /// Validates the settings (returning the first failed rule as the dialog
    /// message) and, if valid, resets the progress table and starts the
    /// worker thread.
    fn start_run(&mut self) -> Option<String> {
        if let Some(reason) = self.start_run_error() {
            return Some(reason);
        }
        let base = self.state.base.trim().to_owned();
        let base_opt = if base.is_empty() { None } else { Some(base) };
        // Pre-start layout check (docs "GUI" — Run): a conflicting PLY type
        // or an invalid `x`,`y`,`z` order fails without starting; a
        // non-empty drop list asks a native Ok/Cancel question, and Cancel
        // means the run does not start.
        let inputs: Vec<&str> = self.state.inputs.iter().map(String::as_str).collect();
        match layout_check(&inputs, base_opt.as_deref()) {
            LayoutCheck::Equal => {}
            LayoutCheck::Drops(dropped) => {
                let confirmed = rfd::MessageDialog::new()
                    .set_level(rfd::MessageLevel::Info)
                    .set_title("Dropped properties")
                    .set_description(format!(
                        "Some properties are not present in every input file and will be dropped from the output: {}",
                        dropped.join(", ")
                    ))
                    .set_buttons(rfd::MessageButtons::OkCancel)
                    .show();
                if confirmed != rfd::MessageDialogResult::Ok {
                    return None;
                }
            }
            LayoutCheck::Error(msg) => return Some(msg),
        }
        let args = crate::Args {
            inputs: self.state.inputs.join(","),
            base: base_opt,
            output: self.state.output.trim().to_owned(),
            voxel_size: self.state.voxel_size,
            threshold: self.state.threshold,
            voxel_axes: crate::VoxelAxes::from_flags(
                self.state.axes[0],
                self.state.axes[1],
                self.state.axes[2],
            ),
        };
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.worker = Some(std::thread::spawn(move || {
            worker_run(args, ChanWriter::new(tx), &worker_cancel)
        }));
        self.rx = Some(rx);
        self.cancel = cancel;
        self.state.progress = [0; 3];
        self.state.running = true;
        self.aborting = false;
        self.state.selected = None;
        None
    }

    /// Drains the lines produced so far by the worker thread: progress lines
    /// update the progress table; the drop-notification line (already
    /// confirmed before the run started) is skipped; every other line is
    /// shown as a message dialog (level per `message_level`).
    fn drain_lines(&mut self) {
        if let Some(rx) = self.rx.as_ref() {
            while let Ok(line) = rx.try_recv() {
                if let Some((col, pct)) = parse_progress_line(&line) {
                    self.state.progress[col] = pct;
                } else if !is_drop_notification(&line) {
                    show_message(message_level(&line), &line);
                }
            }
        }
    }

    /// Finishes the run: resets the run state (settings are kept, so a
    /// rerun is immediate) and returns the failure message, if the worker
    /// reported one, for the caller to show in a dialog.
    fn finish_run(&mut self) -> Option<String> {
        let outcome = self.worker.take().unwrap().join();
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(msg)) => Some(format!("Run failed: {msg}")),
            Err(_) => Some("Run failed: the worker thread panicked.".to_owned()),
        };
        self.state.running = false;
        self.aborting = false;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.state.selected = None;
        self.rx = None;
        failure
    }
}

/// Worker-thread body: runs the pipeline with its lines streamed over the
/// channel. Success and a confirmed abort both end `Ok`: the abort line was
/// already written to the sink by `pipeline::run_with`.
fn worker_run(args: crate::Args, mut sink: ChanWriter, cancel: &AtomicBool) -> Result<(), String> {
    match crate::pipeline::run_with(&args, &mut sink, cancel) {
        Ok(()) => Ok(()),
        Err(e) if e.downcast_ref::<crate::merge::UserAborted>().is_some() => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// An `io::Write` sink that forwards every complete line it receives to a
/// channel; a trailing partial line is sent by the final `flush()`.
struct ChanWriter {
    tx: Mutex<mpsc::Sender<String>>,
    buf: Vec<u8>,
}

impl ChanWriter {
    fn new(tx: mpsc::Sender<String>) -> Self {
        Self {
            tx: Mutex::new(tx),
            buf: Vec::new(),
        }
    }

    fn send_line(&self, line: String) {
        let sender = match self.tx.lock() {
            Ok(guard) => guard,
            // A poisoned lock means the GUI thread died; its inner sender
            // is still usable.
            Err(poisoned) => poisoned.into_inner(),
        };
        // Receiver gone (window closed): drop the line, never fail the run.
        let _ = sender.send(line);
    }
}

impl Write for ChanWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        loop {
            let pos = self.buf.iter().position(|b| *b == b'\n');
            let Some(nl) = pos else {
                break;
            };
            let line = String::from_utf8(self.buf[..nl].to_vec())
                .map_err(|_| std::io::Error::other("non-UTF-8 progress line"))?;
            self.buf.drain(..=nl);
            self.send_line(line);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            let line = std::str::from_utf8(&line)
                .map_err(|_| std::io::Error::other("non-UTF-8 progress line"))?;
            self.send_line(line.to_owned());
        }
        Ok(())
    }
}

impl eframe::App for Gui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl Gui {
    /// Paints the whole screen (docs section "GUI"): the Run/Abort bar, the
    /// settings block and the input file list. Shared with the debug
    /// rendering path (docs section "Debug rendering"), which renders the
    /// same widgets from a JSON state file.
    pub(crate) fn render(&mut self, ui: &mut egui::Ui) {
        self.drain_lines();
        if self.state.running && self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            if let Some(msg) = self.finish_run() {
                show_message(rfd::MessageLevel::Error, &msg);
            }
        }
        if self.state.running {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        // Files dropped onto the window are added through the same rules
        // as picked files.
        for dropped in ui
            .ctx()
            .input_mut(|st| std::mem::take(&mut st.raw.dropped_files))
        {
            if let Some(reason) = self.add_input(dropped.path()) {
                show_message(rfd::MessageLevel::Warning, &reason);
            }
        }

        // Run/Abort: a panel fixed to the bottom edge, aligned to the right.
        egui::Panel::bottom("run-button")
            .frame(
                egui::Frame::NONE.inner_margin(egui::Margin {
                    left: EDGE_PAD,
                    right: EDGE_PAD,
                    top: 0,
                    bottom: EDGE_PAD,
                }),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.state.running {
                        // Abort: confirm natively, then set the cancel flag.
                        // Filled danger red so it reads as a warning action.
                        if ui
                            .add_enabled(
                                !self.aborting,
                                egui::Button::new("Abort")
                                    .fill(DANGER)
                                    .corner_radius(6.0)
                                    .min_size(egui::vec2(96.0, 38.0)),
                            )
                            .clicked()
                        {
                            let result = rfd::MessageDialog::new()
                                .set_level(rfd::MessageLevel::Warning)
                                .set_title("Abort run")
                                .set_description(
                                    "Abort the run? Temporary and partial output files will be removed.",
                                )
                                .show();
                            if result == rfd::MessageDialogResult::Ok {
                                self.cancel.store(true, Ordering::Relaxed);
                                self.aborting = true;
                            }
                        }
                    } else if ui
                        .add(
                            egui::Button::new("Run")
                                .fill(ACCENT)
                                .corner_radius(6.0)
                                .min_size(egui::vec2(96.0, 38.0)),
                        )
                        .clicked()
                    {
                        if let Some(reason) = self.start_run() {
                            show_message(rfd::MessageLevel::Error, &reason);
                        }
                    }
                });
            });
        // The settings block (docs "GUI", layouts 1-3): fixed-height rows
        // pinned above the Run row; the input file list gets the rest of
        // the window.
        egui::Panel::bottom("settings")
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(EDGE_PAD, ROW_GAP)))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.add_enabled_ui(!self.state.running, |ui| self.settings_rows(ui));
                    // Layout 4: the 3x2 progress table (docs "GUI", layout 4).
                    // The space above it is the plain 8px item margin (docs
                    // "GUI" theme says no separator lines), so it reads as a
                    // clean gap, not a line.
                    self.progress_table(ui);
                });
            });
        // Layout 1 (docs "GUI"): the input file list fills all remaining
        // window height and scrolls if the entries do not fit.
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(EDGE_PAD)))
            .show(ui, |ui| {
                ui.add_enabled_ui(!self.state.running, |ui| self.file_list(ui));
            });
    }
}

/// Padding of all content against the window edges (docs "GUI", theme).
const EDGE_PAD: i8 = 16;

/// Gap between the bottom-anchored panels (docs "GUI", theme: 8px spacing
/// between elements).
const ROW_GAP: i8 = 8;

/// The app's accent blue: fills the Run button and the progress bars
/// (docs "GUI", theme — the primary action).
const ACCENT: egui::Color32 = egui::Color32::from_rgb(48, 112, 205);

/// The danger red: fills the "Abort" button while a run is active
/// (docs "GUI", theme).
const DANGER: egui::Color32 = egui::Color32::from_rgb(205, 72, 72);

impl Gui {
    /// Layout 1 of the docs section "GUI": the input file list, one entry
    /// per line, filling the whole panel it is placed in (scrolls if the
    /// entries do not fit). The list sits in a visible group frame (1px
    /// border of the theme), inset from the window edges by the central
    /// panel's 16px margin.
    fn file_list(&mut self, ui: &mut egui::Ui) {
        let mut prepared = egui::Frame::group(ui.style()).inner_margin(8).begin(ui);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("input-list")
            .show(&mut prepared.content_ui, |ui| {
                if self.state.inputs.is_empty() {
                    ui.label("No input files. Use Add, or drag .ply files onto the window.");
                } else {
                    for (idx, input) in self.state.inputs.iter().enumerate() {
                        let selected = self.state.selected == Some(idx);
                        if ui.selectable_label(selected, input.as_str()).clicked() {
                            self.state.selected = Some(idx);
                        }
                    }
                }
            });
        prepared.end(ui);
    }

    /// The setting rows of the docs section "GUI", layouts 1-3: the
    /// Add/Remove control row, the voxel-settings row, then the base row,
    /// then the output row, each on its own full-width row.
    fn settings_rows(&mut self, ui: &mut egui::Ui) {
        // Layout 1, first control row: Add/Remove — the list actions.
        ui.horizontal(|ui| {
            if ui.button("Add").clicked() {
                self.pick_inputs();
            }
            let remove = ui.add_enabled(self.state.remove_enabled(), egui::Button::new("Remove"));
            if remove.clicked() {
                if let Some(idx) = self.state.selected.filter(|&i| i < self.state.inputs.len()) {
                    self.state.inputs.remove(idx);
                }
                self.state.selected = None;
            }
        });
        // Layout 1, second control row: the voxel settings — the axis
        // checkboxes (which define the voxel cells; an unchecked axis is
        // ignored, docs "GUI" layout 1 and "Modes — voxel") on the left,
        // and the voxel-size chooser right-aligned at the end of the row.
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.state.axes[0], "Voxels: X");
            ui.checkbox(&mut self.state.axes[1], "Voxels: Y");
            ui.checkbox(&mut self.state.axes[2], "Voxels: Z");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // In a right-to-left layout the first widget placed lands at
                // the right end, so add the value first and the label after.
                ui.add(egui::DragValue::new(&mut self.state.voxel_size).speed(0.1));
                ui.label("Voxel size:");
            });
        });
        // Layout 2: base field expanding to the remaining row width, with
        // "Browse" and the threshold chooser right-aligned. The trailing
        // group's width is measured first (in an invisible sizing-pass child
        // Ui, so nothing is painted) to size the field exactly.
        let base_trailing = measure_group_width(ui, |probe| {
            probe.add(egui::Button::new("Browse"));
            probe.label("Threshold:");
            probe.add(egui::DragValue::new(&mut self.state.threshold).speed(0.01));
        });
        ui.horizontal(|ui| {
            ui.label("Base:");
            let field_width =
                (ui.available_width() - base_trailing - ui.spacing().item_spacing.x).max(60.0);
            ui.add(egui::TextEdit::singleline(&mut self.state.base).desired_width(field_width));
            if ui.button("Browse").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PLY files", &["ply"])
                    .pick_file()
                {
                    self.state.base = path.to_string_lossy().into_owned();
                }
            }
            ui.label("Threshold:");
            ui.add(egui::DragValue::new(&mut self.state.threshold).speed(0.01));
        });
        // Layout 3: output field expanding to the remaining row width, with
        // "Browse" right-aligned (measured the same way).
        let output_trailing = measure_group_width(ui, |probe| {
            probe.add(egui::Button::new("Browse"));
        });
        ui.horizontal(|ui| {
            ui.label("Output:");
            let field_width =
                (ui.available_width() - output_trailing - ui.spacing().item_spacing.x).max(60.0);
            ui.add(egui::TextEdit::singleline(&mut self.state.output).desired_width(field_width));
            if ui.button("Browse").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PLY files", &["ply"])
                    .set_file_name("merged.ply")
                    .save_file()
                {
                    self.state.output = path.to_string_lossy().into_owned();
                }
            }
        });
    }
}

/// Measures the natural width of a widget group without painting it: the
/// group is laid out in a sizing-pass child `Ui`, then its `min_size` is
/// read. Used to size the expanding fields in `settings_rows`.
fn measure_group_width(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) -> f32 {
    let probe = ui.new_child(
        egui::UiBuilder::new()
            .sizing_pass()
            .invisible()
            .max_rect(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(10_000.0, 100.0),
            ))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let mut probe = probe;
    add(&mut probe);
    probe.min_size().x
}

impl Gui {
    /// Layout 4 of the docs "GUI" section: a two-row, three-column progress
    /// table spanning the full available width in equal-width columns —
    /// bold centered headers ("Mapping", "Merging", "Cleaning") above one
    /// full-width track bar per column. Each bar is a track with the theme's
    /// 1px border and a left-anchored accent fill sized to its value; the
    /// percentage is painted centered over the whole track.
    fn progress_table(&mut self, ui: &mut egui::Ui) {
        let gap = ui.spacing().item_spacing.x;
        let col = (ui.available_width() - 2.0 * gap) / 3.0;
        let text_color = ui
            .visuals()
            .override_text_color
            .unwrap_or(ui.visuals().text_color());
        let track = egui::Color32::from_gray(50);
        let border = ui.visuals().widgets.inactive.bg_stroke;
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for name in ["Mapping", "Merging", "Cleaning"] {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(col, ui.text_style_height(&egui::TextStyle::Heading)),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        name,
                        egui::FontId::new(15.0, egui::FontFamily::Proportional),
                        text_color,
                    );
                }
            });
            ui.horizontal(|ui| {
                for &pct in self.state.progress.iter() {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(col, 22.0), egui::Sense::hover());
                    // Track: a full-width rounded rect, slightly lighter than
                    // the panel fill, framed by the theme's 1px border.
                    ui.painter().rect_filled(rect, 6.0, track);
                    // Accent fill, left-anchored and sized to the value
                    // (painted over the track, so a tiny value still shows).
                    if pct > 0 {
                        let width = (col * (pct as f32) / 100.0).max(1.0);
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(rect.min, egui::vec2(width, rect.height())),
                            6.0,
                            ACCENT,
                        );
                    }
                    ui.painter()
                        .rect_stroke(rect, 6.0, border, egui::epaint::StrokeKind::Middle);
                    // The percentage sits over the whole track, whether the
                    // fill reaches it or not.
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        format!("{pct}%"),
                        egui::FontId::new(13.0, egui::FontFamily::Proportional),
                        text_color,
                    );
                }
            });
        });
    }
}

/// Recognizes a progress line `<task>: <pct>% (...)` — task being
/// `1 Mapping`, `2 Merging (voxel based)`, or `3 Cleaning (scale based)` —
/// and returns (progress-table column, percentage). Any other line yields
/// `None`; such lines become message dialogs instead.
fn parse_progress_line(line: &str) -> Option<(usize, u32)> {
    let (task, rest) = line.split_once(':')?;
    let (num, name) = task.split_once(' ')?;
    let col = match num {
        "1" if name == "Mapping" => 0,
        "2" if name == "Merging (voxel based)" => 1,
        "3" if name == "Cleaning (scale based)" => 2,
        _ => return None,
    };
    let pct = rest.trim_start().split('%').next()?.parse::<u32>().ok()?;
    Some((col, pct))
}

/// Whether the line is the pipeline's drop-notification (docs "Pipeline"
/// — Errors): the user already confirmed the drops before the run started
/// (docs "GUI" — Run), so `drain_lines` skips it instead of re-showing it
/// as a dialog.
fn is_drop_notification(line: &str) -> bool {
    line.starts_with("Dropped properties not present in every file: ")
}

/// Maps a pipeline line to its dialog level: the `Success:` line is
/// information, the abort line a warning, everything else an error. (The
/// drop-notification line never reaches here: `drain_lines` skips it.)
fn message_level(line: &str) -> rfd::MessageLevel {
    if line.starts_with("Success:") {
        rfd::MessageLevel::Info
    } else if line.contains("Aborted") {
        rfd::MessageLevel::Warning
    } else {
        rfd::MessageLevel::Error
    }
}

/// Maps one PLY file to its parsed header, opening it read-only (docs
/// "GUI" — Run, layout check): an unreadable file, or a header with no
/// `end_header` line, is an error the start path shows in an error dialog.
fn probe_header(path: &str) -> Result<PlyInfo, String> {
    let file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mmap = unsafe {
        // SAFETY: `file` stays open for the lifetime of the mapping, so
        // the mapped bytes remain valid while the header is parsed.
        Mmap::map(&file).map_err(|e| format!("{path}: {e}"))?
    };
    // Only the first 64 KiB can hold a sane ASCII header; bounding the
    // scan also keeps a missing `end_header` from dragging the whole
    // binary body through the UTF-8 line scanner.
    let head = &mmap[..mmap.len().min(64 * 1024)];
    let stop = head
        .windows(11)
        .position(|w| w == b"end_header\n")
        .map(|p| p + 11)
        .or_else(|| {
            head.windows(12)
                .position(|w| w == b"end_header\r\n")
                .map(|p| p + 12)
        })
        .ok_or_else(|| {
            format!("{path}: the PLY header is incomplete or invalid (no `end_header` line)")
        })?;
    Ok(parse_ply_header(&head[..std::cmp::min(stop, head.len())]))
}

/// The `start_run` layout-check outcome (docs "GUI" — Run).
#[derive(Debug)]
enum LayoutCheck {
    /// Every non-empty source declares the same properties (nothing drops).
    Equal,
    /// These properties (first source's order) are absent from at least one
    /// non-empty source and would be dropped from the output.
    Drops(Vec<String>),
    /// The run must not start; the message is the documented error text,
    /// shown in an error dialog.
    Error(String),
}

/// Compares the property layouts of the non-empty data sources (`inputs`
/// plus the optional `base`), probing each file read-only: an unreadable
/// file, an invalid `x`,`y`,`z` order, or a conflicting PLY type is an
/// error naming its cause; otherwise the names that would be dropped (in
/// first-source order) are listed, or `Equal` when no property drops.
fn layout_check(inputs: &[&str], base: Option<&str>) -> LayoutCheck {
    let mut infos = Vec::new();
    for s in inputs.iter().copied().chain(base) {
        let t = s.trim();
        if t.is_empty() || t == "-" {
            continue;
        }
        match probe_header(t) {
            Ok(i) => infos.push(i),
            Err(msg) => return LayoutCheck::Error(msg),
        }
    }
    if infos.is_empty() {
        // No real files to compare: `start_run_error` requires at least one
        // input, and the pipeline reports the all-empty error afterwards.
        return LayoutCheck::Equal;
    }
    let refs: Vec<&PlyInfo> = infos.iter().collect();
    match common_layout(&refs) {
        Ok((_, dropped)) if dropped.is_empty() => LayoutCheck::Equal,
        Ok((_, dropped)) => LayoutCheck::Drops(dropped),
        Err(msg) => LayoutCheck::Error(msg),
    }
}

/// Shows `text` in a native message dialog at the given level.
fn show_message(level: rfd::MessageLevel, text: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(level)
        .set_title("splat-merge")
        .set_description(text)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ply_extension_rule() {
        assert!(is_ply(Path::new("a.ply")));
        assert!(is_ply(Path::new("a.PLY")));
        assert!(is_ply(Path::new("C:\\models\\merged.ply")));
        assert!(!is_ply(Path::new("a.txt")));
        assert!(!is_ply(Path::new("a.ply.bak")));
        assert!(!is_ply(Path::new("noext")));
        assert!(!is_ply(Path::new(".ply")));
    }

    #[test]
    fn chan_writer_forwards_complete_lines_in_order() {
        let (tx, rx) = mpsc::channel::<String>();
        let mut sink = ChanWriter::new(tx);
        sink.write_all(b"one\n").unwrap();
        assert_eq!(rx.try_recv(), Ok("one".to_owned()));
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        // A partial line stays buffered until `flush()`.
        sink.write_all(b"two").unwrap();
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        sink.flush().unwrap();
        assert_eq!(rx.try_recv(), Ok("two".to_owned()));
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[test]
    fn add_input_rejects_non_ply_and_duplicates() {
        let mut gui = Gui::default();
        let msg = gui.add_input(Path::new("a.txt")).expect("rejected");
        assert!(msg.to_ascii_lowercase().contains("not a .ply file"));
        assert!(gui.add_input(Path::new("a.ply")).is_none());
        let msg = gui.add_input(Path::new("A.PLY")).expect("duplicate");
        assert!(msg.to_ascii_lowercase().contains("already added"));
        assert_eq!(gui.state.inputs, vec!["a.ply".to_owned()]);
    }

    #[test]
    fn start_run_requires_at_least_one_voxel_axis() {
        let mut gui = Gui::default();
        gui.state.inputs.push("a.ply".to_owned());
        gui.state.output = "out.ply".to_owned();
        let expected = "At least one voxel axis (X, Y or Z) must be checked.".to_owned();
        // All three boxes unchecked: the documented rejection.
        gui.state.axes = [false; 3];
        assert_eq!(gui.start_run_error(), Some(expected));
        // Any single axis on its own is accepted (and so is the default,
        // all three checked).
        for axes in [
            [true, false, false],
            [false, true, false],
            [false, false, true],
            [true, true, true],
        ] {
            gui.state.axes = axes;
            assert_eq!(gui.start_run_error(), None, "axes {axes:?}");
        }
    }

    /// `GuiState::default` is exactly the GUI startup defaults (docs "GUI"
    /// layout 1-3 and section "CLI"): no files, no selection, empty base and
    /// output, the CLI defaults for the numeric settings, all axes on, empty
    /// progress table, not running.
    #[test]
    fn gui_state_defaults_match_docs() {
        let s = GuiState::default();
        assert!(s.inputs.is_empty());
        assert_eq!(s.selected, None);
        assert_eq!(s.base, "");
        assert_eq!(s.voxel_size, 0.5);
        assert_eq!(s.axes, [true, true, true]);
        assert_eq!(s.threshold, 0.01);
        assert_eq!(s.output, "");
        assert_eq!(s.progress, [0, 0, 0]);
        assert!(!s.running);
    }

    /// `Gui::default` and `Gui::new_with` both expose the state exactly as
    /// given, so the debug-rendered screen starts from the documented state.
    #[test]
    fn gui_state_is_identity_and_removable() {
        assert_eq!(Gui::default().state, GuiState::default());

        let state = GuiState {
            inputs: vec!["a.ply".to_owned(), "b.ply".to_owned()],
            selected: Some(1),
            base: "base.ply".to_owned(),
            voxel_size: 1.0,
            axes: [true, false, true],
            threshold: 0.5,
            output: "out.ply".to_owned(),
            progress: [10, 20, 30],
            running: true,
        };
        let gui = Gui::new_with(state.clone());
        assert_eq!(gui.state, state);
        assert!(gui.state.remove_enabled());
        assert!(!GuiState {
            selected: Some(2),
            ..state.clone()
        }
        .remove_enabled());
        assert!(!GuiState {
            selected: None,
            ..state
        }
        .remove_enabled());
    }

    /// Painting the full screen (all three layouts) with a non-default state
    /// must not panic and must leave the state untouched — the debug render
    /// relies on `render` being a pure function of the state.
    #[test]
    fn render_is_pure_with_a_full_state() {
        let state = GuiState {
            inputs: vec!["a.ply".to_owned(), "b.ply".to_owned(), "c.ply".to_owned()],
            selected: Some(0),
            base: "base.ply".to_owned(),
            voxel_size: 0.75,
            axes: [true, true, false],
            threshold: 0.02,
            output: "out.ply".to_owned(),
            progress: [25, 50, 75],
            running: false,
        };
        let mut gui = Gui::new_with(state.clone());
        egui::__run_test_ui(|ui| {
            gui.render(ui);
        });
        assert_eq!(gui.state, state);
    }

    #[test]
    fn parse_progress_line_maps_all_three_tasks() {
        assert_eq!(
            parse_progress_line("1 Mapping: 2% (1.0M splats)"),
            Some((0, 2))
        );
        assert_eq!(
            parse_progress_line("2 Merging (voxel based): 43% (12.3M splats)"),
            Some((1, 43))
        );
        assert_eq!(
            parse_progress_line("3 Cleaning (scale based): 100% (2.0M splats)"),
            Some((2, 100))
        );
    }

    #[test]
    fn parse_progress_line_rejects_other_lines() {
        assert_eq!(
            parse_progress_line("Success: 5 splats saved to a.ply"),
            None
        );
        assert_eq!(
            parse_progress_line("Aborted; temporary and partial output files removed."),
            None
        );
        assert_eq!(
            parse_progress_line("Run failed: the worker thread panicked."),
            None
        );
        // The pre-rename labels are not progress lines either.
        assert_eq!(parse_progress_line("2 Merging: 43% (1.0M splats)"), None);
        assert_eq!(parse_progress_line("not a progress line"), None);
    }

    /// The central panel's frame margin must inset its content by `EDGE_PAD`
    /// on every side (the file list must not touch the window edges).
    #[test]
    fn central_panel_frame_insets_content() {
        let mut outer = egui::Rect::NOTHING;
        let mut inner = egui::Rect::NOTHING;
        egui::__run_test_ui(|ui| {
            outer = ui.max_rect();
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(EDGE_PAD)))
                .show(ui, |ui| {
                    inner = ui.max_rect();
                });
        });
        let m = f32::from(EDGE_PAD);
        assert!(
            (inner.min.x - (outer.min.x + m)).abs() <= 0.5,
            "{inner:?} vs {outer:?}"
        );
        assert!(
            (inner.min.y - (outer.min.y + m)).abs() <= 0.5,
            "{inner:?} vs {outer:?}"
        );
        assert!(
            (inner.max.x - (outer.max.x - m)).abs() <= 0.5,
            "{inner:?} vs {outer:?}"
        );
        assert!(
            (inner.max.y - (outer.max.y - m)).abs() <= 0.5,
            "{inner:?} vs {outer:?}"
        );
    }

    /// One header line in the test PLY files (shared by the layout-check
    /// cases below).
    const HDR_OPA: &str = "PLY\nformat binary_little_endian 1.0\nelement vertex 1\n\
                          property float x\nproperty float y\nproperty float z\n\
                          property float opacity\nend_header\n";
    const HDR_OPA_DETAIL: &str = "PLY\nformat binary_little_endian 1.0\nelement vertex 1\n\
                                 property float x\nproperty float y\nproperty float z\n\
                                 property float opacity\nproperty float detail\n\
                                 property float scale_0\nend_header\n";
    const HDR_SCALE: &str = "PLY\nformat binary_little_endian 1.0\nelement vertex 1\n\
                            property float x\nproperty float y\nproperty float z\n\
                            property float scale_0\nend_header\n";
    const HDR_OPA_CHAR: &str = "PLY\nformat binary_little_endian 1.0\nelement vertex 1\n\
                               property float x\nproperty float y\nproperty float z\n\
                               property uchar opacity\nend_header\n";
    const HDR_BAD_ORDER: &str = "PLY\nformat binary_little_endian 1.0\nelement vertex 1\n\
                                property float opacity\nproperty float x\nproperty float y\n\
                                property float z\nend_header\n";

    /// Writes a small PLY (header + body bytes) into a shared test
    /// directory and returns its path — the layout-check tests never need
    /// real data files.
    fn tmp_ply(name: &str, header: &str, body: &[u8]) -> String {
        let dir = std::env::temp_dir().join("splat-merge-gui-layout-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let mut buf = header.as_bytes().to_vec();
        buf.extend_from_slice(body);
        std::fs::write(&p, buf).unwrap();
        p.to_string_lossy().into_owned()
    }

    /// The test bodies: little-endian float bytes (the layout check only
    /// reads the header, the values just size the records).
    fn float_body(vals: &[f32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn probe_header_reads_valid_and_rejects_bad_files() {
        let body = float_body(&[0.0, 0.0, 0.0, 0.5]);
        let ok = tmp_ply("gui-probe-valid.ply", HDR_OPA, &body);
        let i = probe_header(&ok).expect("valid file");
        assert_eq!(i.vertex_count, 1);
        assert_eq!(i.stride, 16);
        assert_eq!(
            i.properties
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["x", "y", "z", "opacity"]
        );

        let incomplete = tmp_ply(
            "gui-probe-incomplete.ply",
            "PLY\nformat binary_little_endian 1.0\nelement vertex 1\nproperty float x\n",
            &[0u8; 32],
        );
        let err = probe_header(&incomplete).err().expect("incomplete header");
        assert!(
            err.contains("end_header"),
            "expected the end_header error, got {err}"
        );

        let missing = std::env::temp_dir()
            .join("splat-merge-gui-layout-tests")
            .join("definitely-missing.ply")
            .to_string_lossy()
            .into_owned();
        assert!(probe_header(&missing).is_err());
    }

    #[test]
    fn layout_check_equal_layouts() {
        let body = float_body(&[0.0, 0.0, 0.0, 0.5]);
        let a = tmp_ply("gui-layout-eq-a.ply", HDR_OPA, &body);
        let b = tmp_ply("gui-layout-eq-b.ply", HDR_OPA, &body);
        assert!(matches!(
            layout_check(&[a.as_str(), b.as_str()], None),
            LayoutCheck::Equal
        ));
        // The base takes part in the comparison too.
        assert!(matches!(
            layout_check(&[a.as_str(), b.as_str()], Some(a.as_str())),
            LayoutCheck::Equal
        ));
    }

    #[test]
    fn layout_check_drops_listed_in_first_source_order() {
        let body = float_body(&[0.0, 0.0, 0.0, 0.5]);
        // a: x, y, z, opacity, detail, scale_0; b: x, y, z, scale_0.
        let a = tmp_ply("gui-layout-drop-a.ply", HDR_OPA_DETAIL, &body);
        let b = tmp_ply("gui-layout-drop-b.ply", HDR_SCALE, &body);
        match layout_check(&[a.as_str(), b.as_str()], None) {
            LayoutCheck::Drops(dropped) => {
                assert_eq!(dropped, ["opacity".to_owned(), "detail".to_owned()])
            }
            other => panic!("expected a drop list, got {other:?}"),
        }
        // `-` and empty slots are skipped, and the base is compared too.
        match layout_check(&["-", a.as_str(), b.as_str()], None) {
            LayoutCheck::Drops(dropped) => {
                assert_eq!(dropped, ["opacity".to_owned(), "detail".to_owned()])
            }
            other => panic!("expected a drop list, got {other:?}"),
        }
        assert!(matches!(
            layout_check(&[a.as_str(), b.as_str()], Some("")),
            LayoutCheck::Drops(_)
        ));
    }

    #[test]
    fn layout_check_conflicting_type_is_an_error() {
        let body = float_body(&[0.0, 0.0, 0.0, 0.5]);
        let a = tmp_ply("gui-layout-conflict-a.ply", HDR_OPA, &body);
        // b's `opacity` is a uchar byte instead of a float.
        let mut bbody = float_body(&[0.0, 0.0, 0.0]);
        bbody.push(200u8);
        let b = tmp_ply("gui-layout-conflict-b.ply", HDR_OPA_CHAR, &bbody);
        match layout_check(&[a.as_str(), b.as_str()], None) {
            LayoutCheck::Error(msg) => {
                assert_eq!(
                    msg,
                    "Property 'opacity' has conflicting PLY types between files (float vs uchar)"
                );
            }
            other => panic!("expected the conflict error, got {other:?}"),
        }
    }

    #[test]
    fn layout_check_bad_xyz_order_is_an_error() {
        let body = float_body(&[0.0, 0.0, 0.0, 0.5]);
        let a = tmp_ply("gui-layout-order-a.ply", HDR_OPA, &body);
        let b = tmp_ply("gui-layout-order-b.ply", HDR_BAD_ORDER, &body);
        match layout_check(&[a.as_str(), b.as_str()], None) {
            LayoutCheck::Error(msg) => assert_eq!(
                msg,
                "Every file must declare x, y, z as the first three properties"
            ),
            other => panic!("expected the order error, got {other:?}"),
        }
    }

    #[test]
    fn message_level_maps_pipeline_lines() {
        assert!(matches!(
            message_level("Success: 5 splats saved to a.ply"),
            rfd::MessageLevel::Info
        ));
        // The drop-notification is no dialog at all — it is skipped.
        assert!(is_drop_notification(
            "Dropped properties not present in every file: opacity, detail"
        ));
        assert!(!is_drop_notification(
            "Dropped properties present in every file: opacity"
        ));
        assert!(matches!(
            message_level("Aborted; temporary and partial output files removed."),
            rfd::MessageLevel::Warning
        ));
        assert!(matches!(
            message_level("Run failed after 3 Cleaning (scale based): 100% (2.0M splats): boom"),
            rfd::MessageLevel::Error
        ));
    }
}
