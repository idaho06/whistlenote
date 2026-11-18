//! Tempo estimation from onsets and taps.

/// Lowest tempo in BPM that is reported.
const MIN_BPM: f64 = 40.0;
/// Highest tempo in BPM that is reported.
const MAX_BPM: f64 = 220.0;
/// Longest onset interval in seconds that is kept.
const MAX_INTERVAL_S: f64 = 4.0;
/// Number of taps that [`TapTempo`] stores.
const MAX_TAPS: usize = 8;
/// Longest gap in seconds between consecutive taps within one run.
const MAX_TAP_GAP_S: f64 = 2.5;

/// Folds `interval_s` into `[60.0 / MAX_BPM, 60.0 / MIN_BPM]` seconds by
/// doubling while it is too short and halving while it is too long.
fn fold_interval(interval_s: f64) -> f64 {
    let mut folded = interval_s;
    while folded < 60.0 / MAX_BPM {
        folded *= 2.0;
    }
    while folded > 60.0 / MIN_BPM {
        folded /= 2.0;
    }
    folded
}

/// Returns the median of `values`, or `None` when `values` is empty.
///
/// An even count averages the two middle values of the sorted values.
fn median(mut values: Vec<f64>) -> Option<f64> {
    let len = values.len();
    if len == 0 {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = len / 2;
    Some(if len % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    })
}

/// Rounds `bpm` to one decimal place and clamps it into `MIN_BPM..=MAX_BPM`.
fn normalize_bpm(bpm: f64) -> f64 {
    ((bpm * 10.0).round() / 10.0).clamp(MIN_BPM, MAX_BPM)
}

/// Estimates a tempo in BPM from onset times in seconds.
///
/// Consecutive onsets give intervals; an interval that is not positive, not
/// finite, or longer than 4 seconds is dropped. Each remaining interval is
/// folded into `60 / 220..=60 / 40` seconds and the tempo comes from the
/// median interval. Returns `None` when there are too few usable intervals.
/// The result lies in 40.0..=220.0 and is rounded to one decimal.
pub fn estimate_bpm(onsets_s: &[f64]) -> Option<f64> {
    if onsets_s.len() < 3 {
        return None;
    }
    let intervals: Vec<f64> = onsets_s
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|interval| interval.is_finite() && *interval > 0.0 && *interval <= MAX_INTERVAL_S)
        .map(fold_interval)
        .collect();
    let median_s = median(intervals)?;
    Some(normalize_bpm(60.0 / median_s))
}

/// Tap tempo estimator.
#[derive(Debug, Clone, PartialEq)]
pub struct TapTempo {
    taps: Vec<f64>,
}

impl TapTempo {
    /// Creates an estimator with no taps.
    pub fn new() -> Self {
        Self { taps: Vec::new() }
    }

    /// Records a tap at `now_s` seconds and returns the tempo in BPM.
    ///
    /// A gap of more than 2.5 seconds from the latest stored tap clears the
    /// stored taps, and at most the last 8 taps are kept. Returns `None` when
    /// the tap is the only one stored or the mean interval is not a positive
    /// finite number.
    pub fn tap(&mut self, now_s: f64) -> Option<f64> {
        if self
            .taps
            .last()
            .is_some_and(|&last_s| now_s - last_s > MAX_TAP_GAP_S)
        {
            self.taps.clear();
        }
        self.taps.push(now_s);
        if self.taps.len() > MAX_TAPS {
            self.taps.remove(0);
        }
        let count = self.taps.len();
        if count < 2 {
            return None;
        }
        let mean_interval_s = (self.taps[count - 1] - self.taps[0]) / (count - 1) as f64;
        if !mean_interval_s.is_finite() || mean_interval_s <= 0.0 {
            return None;
        }
        Some(normalize_bpm(60.0 / mean_interval_s))
    }

    /// Forgets all taps.
    pub fn reset(&mut self) {
        self.taps.clear();
    }
}

impl Default for TapTempo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts that `a` and `b` differ by at most `tol`.
    fn assert_close(a: f64, b: f64, tol: f64) {
        assert!(
            (a - b).abs() <= tol,
            "{a} and {b} differ by more than {tol}"
        );
    }

    /// Asserts that `estimate_bpm` returns a tempo within `tol` of `expected`.
    fn assert_bpm(onsets_s: &[f64], expected: f64, tol: f64) {
        let actual = estimate_bpm(onsets_s).expect("a tempo is estimated");
        assert_close(actual, expected, tol);
    }

    #[test]
    fn onsets_every_half_second_give_120_bpm() {
        let onsets: Vec<f64> = (0..6).map(|i| i as f64 * 0.5).collect();
        assert_bpm(&onsets, 120.0, 1e-9);
    }

    #[test]
    fn onsets_every_second_give_60_bpm() {
        assert_bpm(&[0.0, 1.0, 2.0, 3.0, 4.0], 60.0, 1e-9);
    }

    #[test]
    fn jittered_half_second_onsets_stay_within_2_bpm_of_120() {
        assert_bpm(&[0.0, 0.51, 0.99, 1.52, 2.0], 120.0, 2.0);
    }

    #[test]
    fn one_missed_beat_still_gives_120_bpm() {
        assert_bpm(&[0.0, 0.5, 1.0, 2.0, 2.5, 3.0], 120.0, 1e-9);
    }

    #[test]
    fn two_second_onsets_fold_to_60_bpm() {
        assert_bpm(&[0.0, 2.0, 4.0, 6.0], 60.0, 1e-9);
    }

    #[test]
    fn two_onsets_give_none() {
        assert_eq!(estimate_bpm(&[0.0, 0.5]), None);
    }

    #[test]
    fn no_onsets_give_none() {
        assert_eq!(estimate_bpm(&[]), None);
    }

    #[test]
    fn duplicate_onsets_are_ignored() {
        assert_bpm(&[0.0, 0.0, 0.5, 1.0, 1.5], 120.0, 1e-9);
    }

    #[test]
    fn intervals_over_four_seconds_give_none() {
        assert_eq!(estimate_bpm(&[0.0, 5.0, 10.0]), None);
    }

    #[test]
    fn very_fast_onsets_fold_up_to_150_bpm() {
        assert_bpm(&[0.0, 0.1, 0.2, 0.3], 150.0, 1e-9);
    }

    #[test]
    fn taps_every_0_6_seconds_give_100_bpm() {
        let mut tap = TapTempo::new();
        assert_eq!(tap.tap(0.0), None);
        assert_close(tap.tap(0.6).expect("a tempo after two taps"), 100.0, 1e-9);
        assert_close(tap.tap(1.2).expect("a tempo after three taps"), 100.0, 1e-9);
    }

    #[test]
    fn a_tap_after_a_long_pause_restarts_the_run() {
        let mut tap = TapTempo::new();
        assert_eq!(tap.tap(0.0), None);
        assert!(tap.tap(0.6).is_some());
        assert_eq!(tap.tap(3.6), None);
        assert_close(
            tap.tap(4.2).expect("a tempo after the restart"),
            100.0,
            1e-9,
        );
    }

    #[test]
    fn reset_forgets_all_taps() {
        let mut tap = TapTempo::new();
        assert_eq!(tap.tap(0.0), None);
        assert!(tap.tap(0.6).is_some());
        tap.reset();
        assert_eq!(tap.tap(1.2), None);
    }

    #[test]
    fn only_the_last_eight_taps_count() {
        let mut tap = TapTempo::new();
        for i in 0..8 {
            let _ = tap.tap(i as f64);
        }
        // Each tap beyond the eighth drops the oldest tap, so the stored
        // taps are 1.0..=8.5, then 2.0..=9.0, then 3.0..=9.5.
        assert_close(tap.tap(8.5).expect("a tempo"), 56.0, 1e-9);
        assert_close(tap.tap(9.0).expect("a tempo"), 60.0, 1e-9);
        // Stored taps: 3.0, 4.0, 5.0, 6.0, 7.0, 8.5, 9.0, 9.5.
        // Mean interval (9.5 - 3.0) / 7 = 6.5 / 7 s, so 60 / (6.5 / 7) = 64.6 BPM.
        assert_close(tap.tap(9.5).expect("a tempo"), 64.6, 1e-9);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(TapTempo::default(), TapTempo::new());
    }
}
