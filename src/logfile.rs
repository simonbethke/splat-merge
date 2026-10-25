use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Run-log sink: every complete line received is stamped with a UTC
/// `[YYYY-MM-DD HH:MM:SS.mmm]` prefix and flushed immediately, so a
/// crashed or aborted run leaves a complete record of everything before
/// the crash (see the "Run log" documentation section). I/O failures on
/// the already-open file are ignored — the log must never break the run.
pub struct LogFile {
    /// Line-buffered writer on the log file.
    writer: BufWriter<File>,
    /// Bytes of the current line since the last newline (unwritten).
    pending: Vec<u8>,
}

impl LogFile {
    /// Creates (truncating) the log file at `path`.
    pub fn new(path: &Path) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            pending: Vec::new(),
        })
    }
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let total = buf.len();
        self.pending.extend_from_slice(buf);
        // Emit each complete line, oldest newline first; the remainder
        // stays pending.
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let stamp = format_utc(now_ms());
            let _ = self.writer.write_all(format!("[{}] ", stamp).as_bytes());
            let _ = self.writer.write_all(&self.pending[..pos]);
            let _ = self.writer.write_all(b"\n");
            let _ = self.writer.flush();
            self.pending.drain(..=pos);
        }
        Ok(total)
    }

    /// Write failures on the open log are ignored (see the struct docs).
    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.writer.flush();
        Ok(())
    }
}

impl Drop for LogFile {
    /// Writes any unterminated trailing line (stamped) before closing.
    fn drop(&mut self) {
        if !self.pending.is_empty() {
            let _ = self
                .writer
                .write_all(format!("[{}] ", format_utc(now_ms())).as_bytes());
            let _ = self.writer.write_all(&self.pending);
            let _ = self.writer.write_all(b"\n");
            let _ = self.writer.flush();
        }
    }
}

/// Whole milliseconds since the Unix epoch, or 0 if the clock predates it.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Formats `ms` since the Unix epoch as UTC `YYYY-MM-DD HH:MM:SS.mmm`
/// (Hinnant civil-from-days; i64 math throughout).
pub fn format_utc(ms: u64) -> String {
    let secs = ms / 1_000;
    let milli = (ms % 1_000) as i64;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let mo = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = era * 400 + yoe + if mo <= 2 { 1 } else { 0 };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}.{milli:03}")
}

/// Formats a byte size base-1024 with one decimal (`512 B`, `1.5 KB`,
/// `12.3 MB`, `1.0 GB`, `2.0 TB`); below 1024 it is whole bytes.
pub fn format_bytes(size: u64) -> String {
    if size < 1024 {
        return format!("{size} B");
    }
    let (div, unit) = if size >= 1u64 << 40 {
        (1u64 << 40, "TB")
    } else if size >= 1u64 << 30 {
        (1u64 << 30, "GB")
    } else if size >= 1u64 << 20 {
        (1u64 << 20, "MB")
    } else {
        (1u64 << 10, "KB")
    };
    format!("{:.1} {}", size as f64 / div as f64, unit)
}

/// Formats a splat count: below 1000 plain, then thousands/millions/
/// billions with one decimal (`1.5k`, `12.3M`, `2.0B`). No unit suffix
/// other than k/M/B; callers add " splats".
pub fn format_splats(n: usize) -> String {
    let v = n as f64;
    if n < 1_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        return format!("{:.1}k", v / 1_000.0);
    }
    if n < 1_000_000_000 {
        return format!("{:.1}M", v / 1_000_000.0);
    }
    format!("{:.1}B", v / 1_000_000_000.0)
}

/// Formats a duration in whole seconds: `44s`, `5m 10s`, `1h 02m 03s`.
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!(
            "{}h {:02}m {:02}s",
            secs / 3_600,
            (secs / 60) % 60,
            secs % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strips the `[YYYY-MM-DD HH:MM:SS.mmm] ` prefix (asserting its shape).
    fn bare(line: &str) -> &str {
        assert!(
            line.len() >= 26
                && line.starts_with('[')
                && line.as_bytes()[20] == b'.'
                && line.as_bytes()[24..26] == b"] "[..]
        );
        &line[26..]
    }

    #[test]
    fn format_utc_vectors() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00.000");
        assert_eq!(format_utc(86_400_000), "1970-01-02 00:00:00.000");
        assert_eq!(format_utc(86_400_000 + 55_203), "1970-01-02 00:00:55.203");
        assert_eq!(format_utc(1_792_886_400_000), "2026-10-25 00:00:00.000");
        assert_eq!(
            format_utc(1_792_886_400_000 + 3_661_000),
            "2026-10-25 01:01:01.000"
        );
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1 << 20), "1.0 MB");
        assert_eq!(format_bytes(13_107_200), "12.5 MB");
        assert_eq!(format_bytes(5 << 30), "5.0 GB");
        assert_eq!(format_bytes(2 << 40), "2.0 TB");
    }

    #[test]
    fn format_splats_units() {
        assert_eq!(format_splats(0), "0");
        assert_eq!(format_splats(999), "999");
        assert_eq!(format_splats(1_000), "1.0k");
        assert_eq!(format_splats(1_500), "1.5k");
        assert_eq!(format_splats(1_000_000), "1.0M");
        assert_eq!(format_splats(12_300_000), "12.3M");
        assert_eq!(format_splats(2_000_000_000), "2.0B");
    }

    #[test]
    fn format_duration_units() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(44), "44s");
        assert_eq!(format_duration(75), "1m 15s");
        assert_eq!(format_duration(310), "5m 10s");
        assert_eq!(format_duration(3_600), "1h 00m 00s");
        assert_eq!(format_duration(3_723), "1h 02m 03s");
    }

    #[test]
    fn emits_complete_lines_as_they_arrive_and_partial_only_on_drop() {
        let p = std::env::temp_dir().join(format!(
            "splat-merge-logfile-test-{}.tmp",
            std::process::id()
        ));
        assert!(!p.exists());
        {
            let mut lf = LogFile::new(&p).unwrap();
            let _ = lf.write_all(b"abc"); // no newline: still pending...
            let _ = lf.write_all(b"def\n"); // completes to one line
            let _ = lf.flush();
            let text = std::fs::read_to_string(&p).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 1);
            assert_eq!(bare(lines[0]), "abcdef");
            let _ = lf.write_all(b"xyz"); // no newline: pending until drop
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(bare(lines[0]), "abcdef");
        assert_eq!(bare(lines[1]), "xyz");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn truncates_an_existing_log_at_creation() {
        let p = std::env::temp_dir().join(format!(
            "splat-merge-logfile-test-{}-trunc.tmp",
            std::process::id()
        ));
        std::fs::write(&p, "stale content from a previous run\n").unwrap();
        let mut lf = LogFile::new(&p).unwrap();
        let _ = lf.write_all(b"fresh\n");
        drop(lf);
        let text = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(bare(lines[0]), "fresh");
        let _ = std::fs::remove_file(&p);
    }
}
