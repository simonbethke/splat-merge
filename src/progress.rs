use std::io::Write;

/// Weighted phase-progress state for a run's progress lines.
///
/// Tracks the per-file processed splat counters of one phase. A status line
/// is printed whenever the phase's cumulative counter crosses a multiple of
/// one million, and the phase closes with a single final 100% line.
/// Line format per the "Progress" documentation section.
pub struct Progress {
    /// Task label, e.g. `1 Mapping` or `2 Merging`.
    label: String,
    /// Share of the phase's 100% contributed by each file (sums to 1.0 when
    /// any file has a nonzero weight).
    weights: Vec<f64>,
    /// Total splats to process in each file.
    totals: Vec<usize>,
    /// Splats processed so far in each file.
    processed: Vec<usize>,
    /// Cumulative processed splats across all files of the phase.
    counter: usize,
    /// Next million boundary about to be crossed.
    next: usize,
    /// Whether a 100% line has already been printed (dedupes the final line).
    hundred_printed: bool,
}

impl Progress {
    /// Progress state for one phase over `weights.len()` files.
    /// `weights[f]` is the share of the phase's 100% contributed by file `f`;
    /// `totals[f]` is the splat count to process in file `f`.
    pub fn new(label: &str, weights: Vec<f64>, totals: Vec<usize>) -> Self {
        let n = weights.len();
        Self {
            label: label.to_string(),
            weights,
            totals,
            processed: vec![0; n],
            counter: 0,
            next: 1_000_000,
            hundred_printed: false,
        }
    }

    /// Records `n` newly processed splats of file `f` to the counters and
    /// prints a status line for every million-counter boundary crossed.
    /// I/O failures on the line are ignored.
    pub fn advance(&mut self, f: usize, n: usize, out: &mut impl Write) {
        self.processed[f] = self.processed[f].saturating_add(n);
        self.counter = self.counter.saturating_add(n);
        while self.counter >= self.next {
            let pct = self.percent();
            let _ = writeln!(
                out,
                "{}: {}% ({:.1}M splats)",
                self.label,
                pct,
                self.next as f64 / 1_000_000.0
            );
            if pct >= 100 {
                self.hundred_printed = true;
            }
            self.next += 1_000_000;
        }
    }

    /// Closes the phase: prints the final 100% line with the phase's total
    /// splat count, unless a 100% line was already printed. I/O failures on
    /// the line are ignored.
    pub fn finish(&mut self, out: &mut impl Write) {
        if !self.hundred_printed {
            let total: usize = self.totals.iter().sum();
            let _ = writeln!(
                out,
                "{}: 100% ({:.1}M splats)",
                self.label,
                total as f64 / 1_000_000.0
            );
        }
    }

    /// Weighted phase progress as an integer percentage (0–100, rounded).
    /// A zero-splat file counts as fully processed (sub-progress 1.0).
    fn percent(&self) -> u32 {
        let v: f64 = self
            .processed
            .iter()
            .zip(self.weights.iter().zip(self.totals.iter()))
            .map(|(p, (w, t))| {
                if *t == 0 {
                    *w
                } else {
                    *w * (*p as f64) / (*t as f64)
                }
            })
            .sum();
        (v * 100.0).round().clamp(0.0, 100.0) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_million_lines_and_final_100() {
        let mut p = Progress::new("1 Mapping", vec![1.0], vec![2_500_000]);
        let mut out = Vec::new();
        p.advance(0, 1_000_000, &mut out);
        p.advance(0, 1_000_000, &mut out);
        p.advance(0, 500_000, &mut out);
        p.finish(&mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "1 Mapping: 40% (1.0M splats)\n\
             1 Mapping: 80% (2.0M splats)\n\
             1 Mapping: 100% (2.5M splats)\n"
        );
    }

    #[test]
    fn final_100_not_duplicated() {
        let mut p = Progress::new("2 Merging", vec![1.0], vec![2_000_000]);
        let mut out = Vec::new();
        p.advance(0, 1_000_000, &mut out);
        p.advance(0, 1_000_000, &mut out); // crosses the boundary at 100%
        p.finish(&mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "2 Merging: 50% (1.0M splats)\n\
             2 Merging: 100% (2.0M splats)\n"
        );
    }

    #[test]
    fn weighted_sub_progress_worked_example() {
        // Weights 10% / 90%; one million of the 5M splats in the 10% file.
        let mut p = Progress::new("1 Mapping", vec![0.1, 0.9], vec![5_000_000, 5_000_000]);
        let mut out = Vec::new();
        p.advance(0, 1_000_000, &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "1 Mapping: 2% (1.0M splats)\n"
        );
    }

    #[test]
    fn zero_splat_file_counts_as_fully_processed() {
        // File 0 has zero splats but half the weight: it always contributes,
        // so the boundary line at 1M of file 1 already reaches 100% and the
        // final line is suppressed.
        let mut p = Progress::new("1 Mapping", vec![0.5, 0.5], vec![0, 1_000_000]);
        let mut out = Vec::new();
        p.advance(1, 1_000_000, &mut out);
        p.finish(&mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "1 Mapping: 100% (1.0M splats)\n"
        );
    }
}
