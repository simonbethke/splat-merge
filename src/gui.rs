//! The GUI front end (docs section "GUI"): a small eframe/egui window
//! launched when the binary is run without arguments. All control logic
//! (`.ply` filtering, input validation, the line sink for the worker) is
//! here and unit-tested; the heavy work runs on a worker thread through
//! `pipeline::run_with`.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;

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
/// interactive widget (list rows, text inputs, buttons, choosers).
fn apply_theme(cc: &eframe::CreationContext<'_>) {
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

/// The window state: current settings (kept across runs), the log lines,
/// and the run/abort state of the active worker thread.
struct Gui {
    /// Input file list, in order (drives `Args.inputs`).
    inputs: Vec<String>,
    /// Index into `inputs` of the currently selected list entry.
    selected: usize,
    /// Whether `selected` refers to an entry (reset when the list changes).
    has_selection: bool,
    /// Base file path text; empty means no base file.
    base: String,
    /// Cube edge length for the voxel steps.
    voxel: f32,
    /// Max-scale threshold for the scale step.
    thresh: f32,
    /// Output PLY path text.
    output: String,
    /// Log lines, oldest first (validation messages and pipeline output).
    log: Vec<String>,
    /// A worker thread is running.
    running: bool,
    /// Abort was confirmed; the dialog must not be shown again this run.
    aborting: bool,
    /// Shared with the worker thread; set on a confirmed abort.
    cancel: Arc<AtomicBool>,
    /// The worker thread, if one was started (taken when it finishes).
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
    /// Incoming pipeline log lines, if a worker was started.
    rx: Option<mpsc::Receiver<String>>,
}

impl Default for Gui {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            selected: 0,
            has_selection: false,
            base: String::new(),
            // Same defaults as the CLI (docs section "CLI").
            voxel: 0.5,
            thresh: 0.01,
            output: String::new(),
            log: Vec::new(),
            running: false,
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
    /// Adds `path` to the input list; non-`.ply` files and duplicates are
    /// logged (case-insensitive path comparison) and not added.
    fn add_input(&mut self, path: &Path) {
        let display = path.display().to_string();
        if !is_ply(path) {
            self.log
                .push(format!("Rejected (not a .ply file): {display}"));
            return;
        }
        let incoming = path.to_string_lossy().into_owned();
        if self
            .inputs
            .iter()
            .any(|stored| stored.eq_ignore_ascii_case(&incoming))
        {
            self.log.push(format!("Already added: {display}"));
            return;
        }
        self.inputs.push(incoming);
    }

    /// Opens the multi-select native picker and adds every chosen file.
    fn pick_inputs(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("PLY files", &["ply"])
            .pick_files()
        {
            for path in paths {
                self.add_input(&path);
            }
        }
    }

    /// Validates the settings (logging the first failed rule) and, if valid,
    /// starts the worker thread.
    fn start_run(&mut self) {
        if self.inputs.is_empty() {
            self.log
                .push("At least one input file is required.".to_owned());
            return;
        }
        if self.output.trim().is_empty() {
            self.log.push("An output file is required.".to_owned());
            return;
        }
        if !is_ply(Path::new(&self.output)) {
            self.log
                .push("The output file must have a .ply extension.".to_owned());
            return;
        }
        if self.voxel <= 0.0 || self.voxel.is_nan() {
            self.log
                .push("Voxel size must be greater than zero.".to_owned());
            return;
        }
        if self.thresh < 0.0 || self.thresh.is_nan() {
            self.log
                .push("The threshold must be zero or greater.".to_owned());
            return;
        }
        let base = self.base.trim().to_owned();
        let args = crate::Args {
            inputs: self.inputs.join(","),
            base: if base.is_empty() { None } else { Some(base) },
            output: self.output.trim().to_owned(),
            voxel_size: self.voxel,
            threshold: self.thresh,
        };
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.worker = Some(std::thread::spawn(move || {
            worker_run(args, ChanWriter::new(tx), &worker_cancel)
        }));
        self.rx = Some(rx);
        self.cancel = cancel;
        self.running = true;
        self.aborting = false;
        self.selected = 0;
        self.has_selection = false;
    }

    /// Drains the log lines produced so far by the worker thread.
    fn drain_log(&mut self) {
        if let Some(rx) = self.rx.as_ref() {
            while let Ok(line) = rx.try_recv() {
                self.log.push(line);
            }
        }
    }

    /// Finishes the run: reports a failure if the worker reported one and
    /// resets the run state (settings are kept, so a rerun is immediate).
    fn finish_run(&mut self) {
        let outcome = self.worker.take().unwrap().join();
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => self.log.push(format!("Run failed: {msg}")),
            Err(_) => self
                .log
                .push("Run failed: the worker thread panicked.".to_owned()),
        }
        self.running = false;
        self.aborting = false;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.selected = 0;
        self.has_selection = false;
        self.rx = None;
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
        self.drain_log();
        if self.running && self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            self.finish_run();
        }
        if self.running {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        // Files dropped onto the window are added through the same rules
        // as picked files.
        for dropped in ui
            .ctx()
            .input_mut(|st| std::mem::take(&mut st.raw.dropped_files))
        {
            self.add_input(dropped.path());
        }

        // Run/Abort: a panel fixed to the bottom edge, aligned to the right.
        egui::Panel::bottom("run-button").show(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.running {
                    // Abort: confirm natively, then set the cancel flag.
                    if ui
                        .add_enabled(!self.aborting, egui::Button::new("Abort"))
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
                } else if ui.button("Run").clicked() {
                    self.start_run();
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_enabled_ui(!self.running, |ui| self.controls(ui));
            ui.separator();
            // Layout 4: the log area fills the remaining window height, with
            // the newest lines at the bottom.
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.label(egui::RichText::new(line.as_str()).monospace());
                    }
                });
        });
    }
}

impl Gui {
    /// The setting rows of the docs section "GUI", layouts 1-3.
    fn controls(&mut self, ui: &mut egui::Ui) {
        // Layout 1: the input list, full width, one entry per line, with a
        // static height for 7 rows (scrolls inside the fixed box beyond
        // that; docs "GUI", layout 1).
        const LIST_ROWS: usize = 7;
        let row_h =
            ui.text_style_height(&egui::TextStyle::Body) + 2.0 * ui.spacing().button_padding.y;
        let list_h =
            LIST_ROWS as f32 * row_h + (LIST_ROWS - 1) as f32 * ui.spacing().item_spacing.y;
        egui::ScrollArea::vertical()
            .max_height(list_h)
            .min_scrolled_height(list_h)
            .show(ui, |ui| {
                if self.inputs.is_empty() {
                    ui.label("No input files. Use Add, or drag .ply files onto the window.");
                } else {
                    for (idx, input) in self.inputs.iter().enumerate() {
                        let selected = self.has_selection && self.selected == idx;
                        if ui.selectable_label(selected, input.as_str()).clicked() {
                            self.selected = idx;
                            self.has_selection = true;
                        }
                    }
                }
            });
        // Layout 1, control row: Add/Remove on the left, the voxel-size
        // chooser on the right.
        ui.horizontal(|ui| {
            if ui.button("Add").clicked() {
                self.pick_inputs();
            }
            let remove = ui.add_enabled(
                self.has_selection && self.selected < self.inputs.len(),
                egui::Button::new("Remove"),
            );
            if remove.clicked() {
                self.inputs.remove(self.selected);
                self.selected = 0;
                self.has_selection = false;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // In a right-to-left layout the first widget placed lands at
                // the right end, so add the value first and the label after.
                ui.add(egui::DragValue::new(&mut self.voxel).speed(0.1));
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
            probe.add(egui::DragValue::new(&mut self.thresh).speed(0.01));
        });
        ui.horizontal(|ui| {
            ui.label("Base:");
            let field_width =
                (ui.available_width() - base_trailing - ui.spacing().item_spacing.x).max(60.0);
            ui.add(egui::TextEdit::singleline(&mut self.base).desired_width(field_width));
            if ui.button("Browse").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PLY files", &["ply"])
                    .pick_file()
                {
                    self.base = path.to_string_lossy().into_owned();
                }
            }
            ui.label("Threshold:");
            ui.add(egui::DragValue::new(&mut self.thresh).speed(0.01));
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
            ui.add(egui::TextEdit::singleline(&mut self.output).desired_width(field_width));
            if ui.button("Browse").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PLY files", &["ply"])
                    .set_file_name("merged.ply")
                    .save_file()
                {
                    self.output = path.to_string_lossy().into_owned();
                }
            }
        });
    }
}

/// Measures the natural width of a widget group without painting it: the
/// group is laid out in a sizing-pass child `Ui`, then its `min_size` is
/// read. Used to size the expanding fields in `controls`.
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
        gui.add_input(Path::new("a.txt"));
        gui.add_input(Path::new("a.ply"));
        gui.add_input(Path::new("A.PLY"));
        assert_eq!(gui.inputs, vec!["a.ply".to_owned()]);
        let log = gui.log.join("\n");
        assert!(log.to_ascii_lowercase().contains("not a .ply file"));
        assert!(log.to_ascii_lowercase().contains("already added"));
        assert_eq!(gui.log.len(), 2);
    }
}
