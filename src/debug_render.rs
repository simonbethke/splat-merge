//! Debug rendering mode (docs "Debug rendering"): render a documented GUI
//! state from a JSON file to `screenshot.jpg` (JPEG quality 90, current
//! working directory, overwritten on each run) and exit. It never starts the
//! pipeline, never writes the run log, and never opens native dialogs.
//!
//! egui 0.37 has no headless pixel API, so one real `eframe` window is
//! opened; the state is applied through `gui::GuiState`, the frame is
//! painted, and its pixels come back through egui's screenshot callback
//! (`Context::request_screenshot`, delivered asynchronously on a wgpu thread
//! and silently dropped on GPU error). The capture is self-corrected to the
//! requested `width x height` physical pixels via
//! `ViewportCommand::InnerSize`; a bounded deadline turns a dropped callback
//! into a timeout error instead of a hang.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use image::codecs::jpeg::JpegEncoder;

/// A screenshot frame delivered by the screenshot callback: `(width, height, pixels)`.
type Screenshot = (u64, u64, Vec<egui::Color32>);

/// The state to render, validated against the docs "Debug rendering" schema.
#[derive(Debug)]
struct Parsed {
    /// Screenshot width in physical pixels (documented default 1000).
    width: usize,
    /// Screenshot height in physical pixels (documented default 700).
    height: usize,
    /// The window state (a superset: `threshold` is not in the JSON).
    state: crate::gui::GuiState,
}

/// Acquire a mutex, recovering a poisoned guard instead of panicking. The
/// guards here are only ever held around simple reads of the shared
/// `Option` slots, so a guard left behind by a panic is safe to take over.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// `--debug-render <state.json>` (docs "Debug rendering"): load and validate
/// the state, render one window at the state's size, save
/// `screenshot.jpg` (quality 90) in the current working directory, and print
/// the success line. Any failure returns one clear message for `main` to
/// print to stderr.
pub fn run(state_path: &Path) -> Result<(), String> {
    let Parsed {
        width,
        height,
        state,
    } = load_state(state_path)?;

    let captured: Arc<Mutex<Option<Screenshot>>> = Arc::new(Mutex::new(None));
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_app = Arc::clone(&captured);
    let error_app = Arc::clone(&error);

    let mut options = eframe::NativeOptions::default();
    options.viewport.inner_size = Some(egui::vec2(width as f32, height as f32));
    // Under `cargo test` the event loop is created on a test worker thread,
    // which Windows only allows with the `any_thread` opt-in (docs "Required
    // test cases", the e2e case); a no-op in a normal `--debug-render` run.
    #[cfg(all(test, windows))]
    {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            builder.with_any_thread(true);
        }));
    }
    eframe::run_native(
        "splat-merge-debug-render",
        options,
        Box::new(move |cc| {
            crate::gui::apply_theme(cc);
            Ok(Box::new(RenderApp::new(
                crate::gui::Gui::new_with(state),
                (width, height),
                captured_app,
                error_app,
            )))
        }),
    )
    .map_err(|e| format!("cannot open the debug-render window: {e}"))?;

    if let Some(msg) = lock(&error).take() {
        return Err(msg);
    }
    println!("Saved screenshot.jpg ({width}x{height})");
    Ok(())
}

/// The debug-render window. Paints the state frame by frame until the
/// screenshot callback delivers a capture at exactly the requested physical
/// pixels, then encodes it as `screenshot.jpg` and closes the window;
/// failures land in the shared error slot so `run` reports them.
struct RenderApp {
    gui: crate::gui::Gui,
    /// Requested screenshot size in physical pixels (docs "Debug rendering").
    target: (usize, usize),
    /// Last captured frame (physical width, physical height, pixels); set by
    /// the screenshot callback on the wgpu thread.
    captured: Arc<Mutex<Option<Screenshot>>>,
    /// Failure message, if any; reported by `run` after the window closes.
    error: Arc<Mutex<Option<String>>>,
    /// Self-corrections applied to the window size after a mismatched
    /// capture (bounded; a dropped callback must not hang the process).
    attempts: u32,
    /// When this mode started (bounds the whole capture to a deadline).
    start: Instant,
}

impl RenderApp {
    fn new(
        gui: crate::gui::Gui,
        target: (usize, usize),
        captured: Arc<Mutex<Option<Screenshot>>>,
        error: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            gui,
            target,
            captured,
            error,
            attempts: 0,
            start: Instant::now(),
        }
    }

    /// Records a failure in the shared slot.
    fn set_error(&self, msg: String) {
        lock(&self.error).replace(msg);
    }

    /// Closes the window so `run_native` returns (and reports the error, if
    /// one was recorded).
    fn close(&self, ctx: &egui::Context) {
        // The debug-render window is ours to close; on failure `run` exits
        // the process, so closing here cannot abort an interactive session.
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for RenderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // A screenshot callback that never fires (e.g. a GPU format the
        // wgpu capture path cannot read) must surface as a timeout, not a
        // hang; this mode repaints itself, so the deadline is always
        // re-checked (docs "Debug rendering" — bounded deadline).
        if self.start.elapsed() > DEADLINE
            && lock(&self.captured).is_none()
            && !lock(&self.error).is_some()
        {
            self.set_error(format!(
                "no frame was captured within {DEADLINE:?} (the screenshot callback never fired)"
            ));
            self.close(ui.ctx());
            return;
        }

        if let Some((width, height, pixels)) = lock(&self.captured).take() {
            let (tw, th) = self.target;
            if width == tw as u64 && height == th as u64 {
                if let Err(e) = write_jpeg(tw, th, &pixels) {
                    self.set_error(format!("cannot write screenshot.jpg: {e}"));
                }
                self.close(ui.ctx());
                return;
            }
            if self.attempts >= MAX_RESIZES || self.start.elapsed() > DEADLINE {
                self.set_error(format!(
                    "the rendered frame is {width}x{height} pixels, not the requested {tw}x{th}, \
after {} size corrections",
                    self.attempts
                ));
                self.close(ui.ctx());
                return;
            }
            // The capture is in physical pixels but `ViewportCommand::InnerSize`
            // is logical: rescale by the measured ratio so the next capture
            // is exactly the requested size (covers any DPI scale factor).
            self.attempts += 1;
            let (cx, cy) = ui
                .ctx()
                .input(|i| i.viewport().inner_rect)
                .map(|rect| (rect.width(), rect.height()))
                // Unknown until the first layout pass; the captured size then
                // makes the rescale a no-op for this attempt.
                .unwrap_or((width as f32, height as f32));
            let next = egui::vec2(
                (cx * tw as f32 / width as f32).max(100.0),
                (cy * th as f32 / height as f32).max(100.0),
            );
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::InnerSize(next));
        }

        // Paint the state. Debug rendering has no pointer input, so no
        // button can be clicked and no native dialog can open.
        self.gui.render(ui);

        // Request the painted frame's pixels again (a later request replaces
        // an earlier one, so only the latest delivery matters).
        if lock(&self.captured).is_none() && !lock(&self.error).is_some() {
            ui.ctx().request_repaint();
            let captured = Arc::clone(&self.captured);
            ui.ctx()
                .request_screenshot(move |image: Arc<egui::ColorImage>| {
                    // Delivered on a wgpu thread; the only effect is to publish
                    // the capture for the next frame (no UI state is touched).
                    *lock(&captured) = Some((
                        image.size[0] as u64,
                        image.size[1] as u64,
                        image.pixels.clone(),
                    ));
                });
        }
    }
}

/// Deadline for the capture loop (docs "Debug rendering" — bounded so a
/// dropped callback is an error, not a hang).
const DEADLINE: Duration = Duration::from_secs(10);

/// Max self-corrections after a mismatched capture before failing.
const MAX_RESIZES: u32 = 3;

/// Encodes the captured pixels as an RGB8 JPEG of quality 90 into
/// `screenshot.jpg` in the current working directory, overwriting any
/// existing file (docs "Debug rendering"). The window renders fully opaque,
/// so dropping the alpha channel loses nothing.
fn write_jpeg(width: usize, height: usize, pixels: &[egui::Color32]) -> std::io::Result<()> {
    let mut rgb = Vec::with_capacity(pixels.len() * 3);
    for pixel in pixels {
        rgb.push(pixel.r());
        rgb.push(pixel.g());
        rgb.push(pixel.b());
    }
    let file = File::create("screenshot.jpg")?;
    let mut encoder = JpegEncoder::new_with_quality(BufWriter::new(file), 90);
    encoder
        .encode(
            &rgb,
            width as u32,
            height as u32,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(std::io::Error::other)
}

/// Reads and parses the state file (docs "Debug rendering"): the failure
/// message names the offending field and value.
fn load_state(path: &Path) -> Result<Parsed, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("cannot read state file {}: {e}", path.display()))?;
    parse_state(&bytes)
}

/// Parses a state object (docs "Debug rendering") from raw JSON bytes.
fn parse_state(bytes: &[u8]) -> Result<Parsed, String> {
    let json = Parser::new(bytes).parse()?;
    walk(&json)
}

/// Validates the parsed JSON against the docs "Debug rendering" field table
/// and builds the window state (unknown fields, wrong types and out-of-range
/// values are errors naming the field and value).
fn walk(json: &Json) -> Result<Parsed, String> {
    let items = match json {
        Json::Obj(items) => items,
        other => {
            return Err(format!(
                "the state file must be a JSON object, got {}",
                describe(other)
            ))
        }
    };

    let version =
        find(items, "version").ok_or_else(|| "`version` is required (must be 1)".to_string())?;
    let version = match version {
        Json::Int(n) => *n,
        other => return Err(type_err("version", other, "an integer")),
    };
    if version != 1 {
        return Err(format!("`version` must be 1, got {version}"));
    }
    for key in items.iter().map(|(name, _)| name) {
        if !is_top_level_field(key) {
            return Err(unknown_err(key, "the top-level state object"));
        }
    }

    let mut parsed = Parsed {
        width: 1000,
        height: 700,
        state: crate::gui::GuiState::default(),
    };
    if let Some(value) = find(items, "window") {
        let (w, h) = window(value)?;
        parsed.width = w;
        parsed.height = h;
    }
    if let Some(value) = find(items, "inputs") {
        parsed.state.inputs = inputs(value)?;
    }
    if let Some(value) = find(items, "selected_input") {
        parsed.state.selected = selected_input(value, parsed.state.inputs.len())?;
    }
    if let Some(value) = find(items, "voxel_axes") {
        voxel_axes(value, &mut parsed.state.axes)?;
    }
    if let Some(value) = find(items, "voxel_size") {
        voxel_size(value, &mut parsed.state.voxel_size)?;
    }
    if let Some(value) = find(items, "base") {
        parsed.state.base = as_str(value, "base")?;
    }
    if let Some(value) = find(items, "output") {
        parsed.state.output = as_str(value, "output")?;
    }
    if let Some(value) = find(items, "progress") {
        progress(value, &mut parsed.state.progress)?;
    }
    if let Some(value) = find(items, "running") {
        parsed.state.running = as_bool(value, "running")?;
    }
    Ok(parsed)
}

/// The documented top-level fields (docs "Debug rendering").
fn is_top_level_field(name: &str) -> bool {
    matches!(
        name,
        "version"
            | "window"
            | "inputs"
            | "selected_input"
            | "voxel_axes"
            | "voxel_size"
            | "base"
            | "output"
            | "progress"
            | "running"
    )
}

/// `window`: both dimensions present (docs rule), integers, each >= 100.
fn window(value: &Json) -> Result<(usize, usize), String> {
    let items = as_obj(value, "window")?;
    check_no_unknown(items, &["width", "height"], "window")?;
    let width = int_field(items, "width", "window.width", "window")?;
    let height = int_field(items, "height", "window.height", "window")?;
    if width < 100 {
        return Err(format!("`window.width` must be at least 100, got {width}"));
    }
    if height < 100 {
        return Err(format!(
            "`window.height` must be at least 100, got {height}"
        ));
    }
    Ok((width as usize, height as usize))
}

/// `inputs`: an array of strings (`.ply` is not enforced in debug mode).
fn inputs(value: &Json) -> Result<Vec<String>, String> {
    match value {
        Json::Arr(vals) => vals
            .iter()
            .map(|v| as_str(v, "inputs"))
            .collect::<Result<Vec<_>, _>>(),
        other => Err(type_err("inputs", other, "an array of strings")),
    }
}

/// `selected_input`: `null` (default) or a 0-based index into `inputs`.
fn selected_input(value: &Json, len: usize) -> Result<Option<usize>, String> {
    match value {
        Json::Null => Ok(None),
        Json::Int(n) => {
            if *n < 0 || *n as u64 >= len as u64 {
                return Err(format!(
                    "`selected_input` must be between 0 and {} (a 0-based index into the \
{len} input), got {n}",
                    len.saturating_sub(1)
                ));
            }
            Ok(Some(*n as usize))
        }
        other => Err(type_err(
            "selected_input",
            other,
            "an integer index or null",
        )),
    }
}

/// `voxel_axes`: x/y/z booleans (all true by default).
fn voxel_axes(value: &Json, axes: &mut [bool; 3]) -> Result<(), String> {
    let items = as_obj(value, "voxel_axes")?;
    check_no_unknown(items, &["x", "y", "z"], "voxel_axes")?;
    for (name, idx) in [("x", 0), ("y", 1), ("z", 2)] {
        if let Some(v) = find(items, name) {
            axes[idx] = as_bool(v, &format!("voxel_axes.{name}"))?;
        }
    }
    Ok(())
}

/// `voxel_size`: a number strictly greater than zero.
fn voxel_size(value: &Json, out: &mut f32) -> Result<(), String> {
    let v = match value {
        Json::Int(n) => *n as f32,
        Json::Real(f) => *f as f32,
        other => return Err(type_err("voxel_size", other, "a number")),
    };
    if v.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err(format!(
            "`voxel_size` must be greater than zero, got {}",
            describe(value)
        ));
    }
    *out = v;
    Ok(())
}

/// `progress`: mapping/merging/cleaning integers, each 0–100.
fn progress(value: &Json, out: &mut [u32; 3]) -> Result<(), String> {
    let items = as_obj(value, "progress")?;
    check_no_unknown(items, &["mapping", "merging", "cleaning"], "progress")?;
    for (name, idx) in [("mapping", 0), ("merging", 1), ("cleaning", 2)] {
        if let Some(v) = find(items, name) {
            let field = format!("progress.{name}");
            let n = match v {
                Json::Int(i) => *i,
                other => return Err(type_err(&field, other, "an integer")),
            };
            if !(0..=100).contains(&n) {
                return Err(format!("`{field}` must be between 0 and 100, got {n}"));
            }
            out[idx] = n as u32;
        }
    }
    Ok(())
}

/// The first value for `key` in `items` (duplicate keys: first wins).
fn find<'a>(items: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    items
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

/// Rejects names not in `known` with a message naming the field.
fn check_no_unknown(items: &[(String, Json)], known: &[&str], parent: &str) -> Result<(), String> {
    for (name, _) in items {
        if !known.contains(&name.as_str()) {
            return Err(unknown_err(name, parent));
        }
    }
    Ok(())
}

/// An object value, or a type error naming `field`.
fn as_obj<'a>(value: &'a Json, field: &str) -> Result<&'a [(String, Json)], String> {
    match value {
        Json::Obj(items) => Ok(items),
        other => Err(type_err(field, other, "an object")),
    }
}

/// A string value, or a type error naming `field`.
fn as_str(value: &Json, field: &str) -> Result<String, String> {
    match value {
        Json::Str(s) => Ok(s.clone()),
        other => Err(type_err(field, other, "a string")),
    }
}

/// A boolean value, or a type error naming `field`.
fn as_bool(value: &Json, field: &str) -> Result<bool, String> {
    match value {
        Json::Bool(b) => Ok(*b),
        other => Err(type_err(field, other, "a boolean")),
    }
}

/// An integer for `name` in `items`, required when `parent` is present.
fn int_field(
    items: &[(String, Json)],
    name: &str,
    field: &str,
    parent: &str,
) -> Result<i64, String> {
    let Some(value) = find(items, name) else {
        return Err(format!("`{field}` is required when `{parent}` is present"));
    };
    match value {
        Json::Int(n) => Ok(*n),
        other => Err(type_err(field, other, "an integer")),
    }
}

/// "`field` must be expected, got value".
fn type_err(field: &str, value: &Json, expected: &str) -> String {
    format!("`{field}` must be {expected}, got {}", describe(value))
}

/// "unknown field `name` in context".
fn unknown_err(name: &str, context: &str) -> String {
    format!("unknown field `{name}` in {context}")
}

/// A short human description of a JSON value for error messages.
fn describe(value: &Json) -> String {
    match value {
        Json::Null => "null".to_owned(),
        Json::Bool(b) => b.to_string(),
        Json::Int(n) => n.to_string(),
        Json::Real(f) => f.to_string(),
        Json::Str(s) => format!("\"{s}\""),
        Json::Arr(vals) if vals.is_empty() => "[]".to_owned(),
        Json::Arr(vals) => format!("[… {} items]", vals.len()),
        Json::Obj(items) if items.is_empty() => "{}".to_owned(),
        Json::Obj(items) => format!("{{… {} fields}}", items.len()),
    }
}

/// A parsed JSON value. Numbers keep their literal shape: `Int` for integer
/// literals, `Real` for literals with a fraction or exponent — so an integer
/// field given `1.5` (or `1.0`) is a type error (docs "Debug rendering"
/// rules: integer fields accept integral numbers only).
#[derive(Debug)]
enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// A strict recursive-descent JSON parser over raw bytes (no dependency;
/// docs "Debug rendering"). Raw control characters in strings, invalid
/// escapes, and trailing content after the top-level value are errors.
struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { b: bytes, i: 0 }
    }

    /// Parses exactly one top-level value; trailing content is an error.
    fn parse(&mut self) -> Result<Json, String> {
        self.ws();
        let value = self.value()?;
        self.ws();
        if self.i != self.b.len() {
            return Err(format!(
                "invalid JSON: trailing content after the top-level value at byte {}",
                self.i
            ));
        }
        Ok(value)
    }

    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        let Some(&c) = self.b.get(self.i) else {
            return Err("invalid JSON: unexpected end of input".to_owned());
        };
        match c {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Json::Str(self.string()?)),
            b't' => self.literal("true", Json::Bool(true)),
            b'f' => self.literal("false", Json::Bool(false)),
            b'n' => self.literal("null", Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(format!(
                "invalid JSON: unexpected character {c} (0x{c:02x}) at byte {}",
                self.i
            )),
        }
    }

    /// Accepts a literal like `true` only when no letter follows it.
    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        let end = self.i + word.len();
        if self.b.len() >= end
            && &self.b[self.i..end] == word.as_bytes()
            && (end == self.b.len() || !self.b[end].is_ascii_alphabetic())
        {
            self.i = end;
            Ok(value)
        } else {
            Err(format!(
                "invalid JSON: expected {word:?} at byte {}",
                self.i
            ))
        }
    }

    /// Parses a number, keeping the literal shape (an exponent or fraction
    /// makes it `Real`, so `1.0` is not an `Int`).
    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        let mut is_real = false;
        let digits = |this: &mut Self| -> Result<(), String> {
            if this.b.get(this.i).is_none_or(|c| !c.is_ascii_digit()) {
                return Err("invalid JSON: expected a digit".to_owned());
            }
            while this.b.get(this.i).is_some_and(|c| c.is_ascii_digit()) {
                this.i += 1;
            }
            Ok(())
        };

        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        if self.b.get(self.i) == Some(&b'0') {
            // No leading zeros in JSON numbers.
            if self.b.get(self.i + 1).is_some_and(|c| c.is_ascii_digit()) {
                return Err(format!(
                    "invalid JSON: leading zeros are not allowed in a number at byte {}",
                    self.i
                ));
            }
            self.i += 1;
        } else {
            digits(self)?;
        }

        if let Some(&c) = self.b.get(self.i) {
            if c == b'.' {
                is_real = true;
                self.i += 1;
                digits(self)?;
            } else if c == b'e' || c == b'E' {
                is_real = true;
                self.i += 1;
                if self.b.get(self.i).is_some_and(|s| *s == b'+' || *s == b'-') {
                    self.i += 1;
                }
                digits(self)?;
            }
        }

        let text = std::str::from_utf8(&self.b[start..self.i])
            .map_err(|_| "invalid JSON: non-ASCII text in a number".to_owned())?;
        if is_real {
            text.parse::<f64>()
                .map(Json::Real)
                .map_err(|_| format!("invalid JSON: bad number {text}"))
        } else {
            text.parse::<i64>()
                .map(Json::Int)
                .map_err(|_| format!("invalid JSON: integer {text} is out of range"))
        }
    }

    /// Parses a string starting at the opening quote (consumer of it).
    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return Err("invalid JSON: unterminated string".to_owned());
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.b.get(self.i) else {
                        return Err("invalid JSON: unterminated escape sequence".to_owned());
                    };
                    let mut push = |this: &mut Self, ch: char| {
                        this.i += 1;
                        out.push(ch);
                    };
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{0008}',
                        b'f' => '\u{000C}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            self.i += 1;
                            self.unicode_escape()?
                        }
                        _ => {
                            return Err(format!(
                                "invalid JSON: bad escape '\\{}' at byte {}",
                                e as char, self.i
                            ));
                        }
                    };
                    push(self, ch);
                }
                c if (0x00..=0x1F).contains(&c) => {
                    return Err(format!(
                        "invalid JSON: raw control byte 0x{c:02x} in a string at byte {}",
                        self.i
                    ));
                }
                c if c < 0x80 => {
                    out.push(c as char);
                    self.i += 1;
                }
                _ => {
                    // Multi-byte UTF-8: decode it as text.
                    let rest = std::str::from_utf8(&self.b[self.i..]).map_err(|_| {
                        format!("invalid JSON: invalid UTF-8 in a string at byte {}", self.i)
                    })?;
                    let Some(ch) = rest.chars().next() else {
                        return Err(format!(
                            "invalid JSON: invalid UTF-8 in a string at byte {}",
                            self.i
                        ));
                    };
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }

    /// Parses one `\uXXXX` escape after consuming the `u`; a high surrogate
    /// must be followed by `\uXXXX` carrying the low half.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xDC00..0xE000).contains(&hi) {
            return Err(format!(
                "invalid JSON: unpaired low surrogate \\u{hi:04x} in a string"
            ));
        }
        if (0xD800..0xDC00).contains(&hi) {
            if self.b.get(self.i) != Some(&b'\\') || self.b.get(self.i + 1) != Some(&b'u') {
                return Err(format!(
                    "invalid JSON: unpaired high surrogate \\u{hi:04x} in a string"
                ));
            }
            self.i += 2;
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(format!(
                    "invalid JSON: bad surrogate pair \\u{hi:04x}\\u{lo:04x} in a string"
                ));
            }
            let c = (hi as u32 - 0xD800) << 10 | (lo as u32 - 0xDC00) | 0x10000;
            return char::from_u32(c)
                .ok_or_else(|| "invalid JSON: bad surrogate pair in a string".to_string());
        }
        char::from_u32(hi as u32)
            .ok_or_else(|| format!("invalid JSON: invalid code point \\u{hi:04x} in a string"))
    }

    /// Parses four hex digits of a `\uXXXX` escape (or an error).
    fn hex4(&mut self) -> Result<u16, String> {
        if self.i + 4 > self.b.len() {
            return Err("invalid JSON: truncated \\u escape".to_owned());
        }
        let mut v: u16 = 0;
        for &c in &self.b[self.i..self.i + 4] {
            let d = (c as char)
                .to_digit(16)
                .ok_or_else(|| format!("invalid JSON: bad \\u escape at byte {}", self.i))?;
            v = v.wrapping_mul(16).wrapping_add(d as u16);
        }
        self.i += 4;
        Ok(v)
    }

    /// Parses an object starting at `{` (consumer of it).
    fn object(&mut self) -> Result<Json, String> {
        self.i += 1; // {
        let mut items = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Json::Obj(items));
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err(format!(
                    "invalid JSON: expected a string key or '}}' at byte {}",
                    self.i
                ));
            }
            let key = self.string()?;
            self.ws();
            if self.b.get(self.i) != Some(&b':') {
                return Err(format!(
                    "invalid JSON: expected ':' after key \"{key}\" at byte {}",
                    self.i
                ));
            }
            self.i += 1;
            let value = self.value()?;
            items.push((key, value));
            self.ws();
            match self.b.get(self.i) {
                Some(&b',') => self.i += 1,
                Some(&b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(items));
                }
                _ => {
                    return Err(format!(
                        "invalid JSON: expected ',' or '}}' after a member at byte {}",
                        self.i
                    ))
                }
            }
        }
    }

    /// Parses an array starting at `[` (consumer of it).
    fn array(&mut self) -> Result<Json, String> {
        self.i += 1; // [
        let mut vals = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Json::Arr(vals));
        }
        loop {
            let value = self.value()?;
            vals.push(value);
            self.ws();
            match self.b.get(self.i) {
                Some(&b',') => self.i += 1,
                Some(&b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(vals));
                }
                _ => {
                    return Err(format!(
                        "invalid JSON: expected ',' or ']' after an element at byte {}",
                        self.i
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses and expects the documented defaults.
    fn defaults() -> (usize, usize, crate::gui::GuiState) {
        let Parsed {
            width,
            height,
            state,
        } = parse_state(br#"{"version":1}"#).expect("a minimal state must parse");
        (width, height, state)
    }

    fn expect_rejected(json: &str) -> String {
        parse_state(json.as_bytes()).expect_err("must be rejected")
    }

    #[test]
    fn a_minimal_state_uses_the_documented_defaults() {
        let (width, height, state) = defaults();
        assert_eq!(width, 1000);
        assert_eq!(height, 700);
        assert_eq!(state, crate::gui::GuiState::default());
    }

    #[test]
    fn all_fields_are_accepted_and_map_exactly() {
        let json = r#"{
            "version": 1,
            "window": { "width": 1000, "height": 700 },
            "inputs": ["detail_a.ply", "detail_b.ply"],
            "selected_input": 0,
            "voxel_axes": { "x": true, "y": true, "z": false },
            "voxel_size": 0.5,
            "base": "base.ply",
            "output": "merged.ply",
            "progress": { "mapping": 100, "merging": 20, "cleaning": 0 },
            "running": false
        }"#;
        let Parsed {
            width,
            height,
            state,
        } = parse_state(json.as_bytes()).expect("the documented example must parse");
        assert_eq!((width, height), (1000, 700));
        assert_eq!(
            state.inputs,
            vec!["detail_a.ply".to_owned(), "detail_b.ply".to_owned()]
        );
        assert_eq!(state.selected, Some(0));
        assert_eq!(state.axes, [true, true, false]);
        assert_eq!(state.voxel_size, 0.5);
        assert_eq!(state.base, "base.ply");
        assert_eq!(state.output, "merged.ply");
        assert_eq!(state.progress, [100, 20, 0]);
        assert!(!state.running);
        // threshold is a GUI-only field; it keeps its documented default.
        assert_eq!(state.threshold, 0.01);
    }

    #[test]
    fn selected_input_null_keeps_no_selection() {
        let Parsed { state, .. } =
            parse_state(br#"{"version":1,"inputs":["a.ply"],"selected_input":null}"#)
                .expect("an explicit null selection must be accepted");
        assert_eq!(state.selected, None);
    }

    #[test]
    fn version_must_be_one() {
        assert!(expect_rejected(r#"{"version":2}"#).contains("version"));
        assert!(expect_rejected(r#"{"version":0}"#).contains("version"));
        // A literal 1.0 is a real, so a type error, not the value 1.
        assert!(expect_rejected(r#"{"version":1.0}"#).contains("must be an integer"));
        assert!(
            expect_rejected(r#"{"version":"1"}"#).contains("must be an integer")
                && expect_rejected(r#"{"version":"1"}"#).contains("1")
        );
        assert!(expect_rejected(r#"{}"#).contains("version"));
    }

    #[test]
    fn the_top_level_value_must_be_an_object() {
        for json in [r#"[1]"#, r#""a""#, r#"5"#] {
            let err = expect_rejected(json);
            assert!(
                err.contains("object"),
                "{json} should be rejected as not an object, got: {err}"
            );
        }
    }

    #[test]
    fn unknown_fields_are_rejected_naming_the_field() {
        let top = expect_rejected(r#"{"version":1,"frobnicate":5}"#);
        assert!(top.contains("frobnicate"));

        let win =
            expect_rejected(r#"{"version":1,"window":{"width":1000,"height":700,"depth":1}}"#);
        assert!(win.contains("depth"));

        let axes =
            expect_rejected(r#"{"version":1,"voxel_axes":{"x":true,"y":true,"z":true,"w":true}}"#);
        assert!(axes.contains("w"));

        let prog = expect_rejected(
            r#"{"version":1,"progress":{"mapping":1,"merging":2,"cleaning":3,"damping":4}}"#,
        );
        assert!(prog.contains("damping"));
    }

    #[test]
    fn wrong_types_are_rejected_naming_the_field_and_value() {
        let win = expect_rejected(r#"{"version":1,"window":{"width":1000.5,"height":700}}"#);
        assert!(win.contains("width") && win.contains("1000.5"));

        let inputs = expect_rejected(r#"{"version":1,"inputs":"a.ply"}"#);
        assert!(inputs.contains("inputs") && inputs.contains("\"a.ply\""));

        let item = expect_rejected(r#"{"version":1,"inputs":["a.ply",5]}"#);
        assert!(item.contains("inputs") && item.contains("5"));

        let sel = expect_rejected(r#"{"version":1,"inputs":["a.ply"],"selected_input":"0"}"#);
        assert!(sel.contains("selected_input") && sel.contains("\"0\""));

        let axis = expect_rejected(r#"{"version":1,"voxel_axes":{"x":"true","y":true,"z":true}}"#);
        assert!(axis.contains("x") && axis.contains("\"true\""));

        let size = expect_rejected(r#"{"version":1,"voxel_size":"0.5"}"#);
        assert!(size.contains("voxel_size") && size.contains("\"0.5\""));

        let base = expect_rejected(r#"{"version":1,"base":5}"#);
        assert!(base.contains("base") && base.contains("5"));

        let prog = expect_rejected(
            r#"{"version":1,"progress":{"mapping":50.5,"merging":0,"cleaning":0}}"#,
        );
        assert!(prog.contains("mapping") && prog.contains("50.5"));

        let run = expect_rejected(r#"{"version":1,"running":"true"}"#);
        assert!(run.contains("running") && run.contains("\"true\""));
    }

    #[test]
    fn out_of_range_values_are_rejected_naming_the_field_and_value() {
        let prog =
            expect_rejected(r#"{"version":1,"progress":{"mapping":101,"merging":0,"cleaning":0}}"#);
        assert!(prog.contains("mapping") && prog.contains("101"));

        let neg_prog =
            expect_rejected(r#"{"version":1,"progress":{"mapping":-1,"merging":0,"cleaning":0}}"#);
        assert!(neg_prog.contains("mapping") && neg_prog.contains("-1"));

        let zero_size = expect_rejected(r#"{"version":1,"voxel_size":0}"#);
        assert!(zero_size.contains("voxel_size") && zero_size.contains("0"));

        let neg_size = expect_rejected(r#"{"version":1,"voxel_size":-0.5}"#);
        assert!(neg_size.contains("voxel_size") && neg_size.contains("-0.5"));

        let w = expect_rejected(r#"{"version":1,"window":{"width":50,"height":700}}"#);
        assert!(w.contains("width") && w.contains("50"));

        let h = expect_rejected(r#"{"version":1,"window":{"width":1000,"height":99}}"#);
        assert!(h.contains("height") && h.contains("99"));

        let empty = expect_rejected(r#"{"version":1,"selected_input":0}"#);
        assert!(empty.contains("selected_input") && empty.contains("0"));

        let oob = expect_rejected(r#"{"version":1,"inputs":["a.ply","b.ply"],"selected_input":5}"#);
        assert!(oob.contains("selected_input") && oob.contains("5"));

        let neg = expect_rejected(r#"{"version":1,"inputs":["a.ply"],"selected_input":-1}"#);
        assert!(neg.contains("selected_input") && neg.contains("-1"));
    }

    #[test]
    fn the_window_dimensions_are_required_together() {
        let w = expect_rejected(r#"{"version":1,"window":{"height":700}}"#);
        assert!(w.contains("width"));
        let h = expect_rejected(r#"{"version":1,"window":{"width":1000}}"#);
        assert!(h.contains("height"));
    }

    #[test]
    fn malformed_json_is_rejected() {
        for json in [
            r#"{"version"#,            // truncated
            r#"{version:1}"#,          // unquoted key
            r#"{"version":1 } extra"#, // trailing content
            r#"{"version":1,"x":}"#,   // missing value
            r#"{"s":"\q"}"#,           // bad escape
            r#"{"s":"a
b"}"#,           // raw control byte in a string
        ] {
            let err = expect_rejected(json);
            assert!(err.starts_with("invalid JSON"), "{json}: {err}");
        }
    }

    /// The end-to-end contract (docs "Required test cases"): the default
    /// state renders to a valid non-empty JPEG at 1000x700, and a custom
    /// `window` size is honored. One test by design: the two renders both
    /// overwrite `screenshot.jpg` in the working directory, so they must stay
    /// serial (cargo test runs tests in parallel).
    #[test]
    fn debug_render_saves_a_valid_jpeg_at_the_requested_size() {
        use image::ImageDecoder;

        let state_file = std::env::temp_dir().join("splat-merge-debug-render-test.json");
        fn write_state(contents: &str) {
            std::fs::write(
                std::env::temp_dir().join("splat-merge-debug-render-test.json"),
                contents,
            )
            .expect("writing the test state file must succeed");
        }
        fn screenshot_dims() -> (u32, u32) {
            let file = std::fs::File::open("screenshot.jpg").expect("screenshot.jpg must exist");
            let decoder = image::codecs::jpeg::JpegDecoder::new(std::io::BufReader::new(file))
                .expect("screenshot.jpg must be a JPEG file");
            decoder.dimensions()
        }

        // The documented default state: 1000x700, non-empty.
        write_state(r#"{"version":1}"#);
        run(&state_file).expect("debug rendering of the default state must succeed");
        assert_eq!(screenshot_dims(), (1000, 700));
        let len = std::fs::metadata("screenshot.jpg")
            .expect("screenshot.jpg must exist")
            .len();
        assert!(
            len > 10_000,
            "a 1000x700 dark screenshot must be non-empty, got {len} bytes"
        );

        // A custom size is honored (and overwrites the previous screenshot).
        write_state(r#"{"version":1,"window":{"width":400,"height":300}}"#);
        run(&state_file).expect("debug rendering of a custom size must succeed");
        assert_eq!(screenshot_dims(), (400, 300));

        let _ = std::fs::remove_file(&state_file);
    }
}
