//! Tempo estimation from onsets and taps.

use std::f64::consts::SQRT_2;

use super::median;

/// Lowest tempo in BPM that is reported.
pub const MIN_BPM: f64 = 40.0;
/// Highest tempo in BPM that is reported.
pub const MAX_BPM: f64 = 220.0;
/// Longest onset interval in seconds that is kept.
const MAX_INTERVAL_S: f64 = 4.0;
/// Number of taps that [`TapTempo`] stores.
const MAX_TAPS: usize = 8;
/// Longest gap in seconds between consecutive taps within one run: one beat
/// at [`MIN_BPM`] plus a quarter second.
const MAX_TAP_GAP_S: f64 = 60.0 / MIN_BPM + 0.25;

/// Folds `interval_s` into `[reference_s / SQRT_2, reference_s * SQRT_2)` by
/// doubling while it is below the range and halving while it is at or above
/// the range.
///
/// `interval_s` and `reference_s` are positive and finite; any other value
/// does not terminate.
fn fold_interval(interval_s: f64, reference_s: f64) -> f64 {
    let mut folded = interval_s;
    while folded < reference_s / SQRT_2 {
        folded *= 2.0;
    }
    while folded >= reference_s * SQRT_2 {
        folded /= 2.0;
    }
    folded
}

/// Rounds `bpm` to one decimal place and clamps it into `MIN_BPM..=MAX_BPM`.
fn normalize_bpm(bpm: f64) -> f64 {
    ((bpm * 10.0).round() / 10.0).clamp(MIN_BPM, MAX_BPM)
}

/// Estimates a tempo in BPM from onset times in seconds.
///
/// Consecutive onsets give intervals; an interval that is not positive, not
/// finite, or longer than 4 seconds is dropped. The kept intervals are
/// sorted and the lower middle one is the reference; each interval is folded
/// into `[reference / SQRT_2, reference * SQRT_2)` and the tempo is 60
/// divided by the median folded interval, halved or doubled into
/// `MIN_BPM..=MAX_BPM`. Returns `None` when there are too few usable
/// intervals. The result lies in 40.0..=220.0 and is rounded to one decimal.
pub fn estimate_bpm(onsets_s: &[f64]) -> Option<f64> {
    if onsets_s.len() < 3 {
        return None;
    }
    let mut intervals: Vec<f64> = onsets_s
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|interval| interval.is_finite() && *interval > 0.0 && *interval <= MAX_INTERVAL_S)
        .collect();
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_by(f64::total_cmp);
    let reference_s = intervals[(intervals.len() - 1) / 2];
    let folded: Vec<f64> = intervals
        .iter()
        .map(|&interval| fold_interval(interval, reference_s))
        .collect();
    let median_s = median(folded)?;
    let mut bpm = 60.0 / median_s;
    while bpm > MAX_BPM {
        bpm /= 2.0;
    }
    while bpm < MIN_BPM {
        bpm *= 2.0;
    }
    Some(normalize_bpm(bpm))
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
    /// A gap of more than one beat at [`MIN_BPM`] plus a quarter second from
    /// the latest stored tap clears the stored taps, and at most the last 8
    /// taps are kept. The tempo is 60 divided by the median interval between
    /// the stored taps. Returns `None` when the tap is the only one stored or
    /// the median interval is not a positive finite number.
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
        if self.taps.len() < 2 {
            return None;
        }
        let intervals: Vec<f64> = self.taps.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let median_interval_s = median(intervals)?;
        if !median_interval_s.is_finite() || median_interval_s <= 0.0 {
            return None;
        }
        Some(normalize_bpm(60.0 / median_interval_s))
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
    use crate::music::assert_close;

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
    fn intervals_around_the_fold_boundary_give_one_tempo() {
        // Intervals 0.270, 0.275, 0.270, 0.275 s lie on both sides of
        // 60 / 220 s; folded around their reference 0.270 s they all stay, so
        // the median interval is 0.2725 s, 220.2 BPM, halved to 110.1 BPM.
        assert_bpm(&[0.0, 0.270, 0.545, 0.815, 1.090], 110.1, 1e-9);
    }

    #[test]
    fn half_second_then_one_second_onsets_give_120_bpm() {
        // Intervals 0.5 s and 1.0 s fold around the reference 0.5 s to two
        // 0.5 s intervals, so the tempo is 120 BPM.
        assert_bpm(&[0.0, 0.5, 1.5], 120.0, 1e-9);
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
    fn a_pause_longer_than_one_beat_at_40_bpm_starts_a_new_tap_run() {
        let mut tap = TapTempo::new();
        assert_eq!(tap.tap(0.0), None);
        assert_close(tap.tap(0.5).expect("a tempo"), 120.0, 1e-9);
        assert_close(tap.tap(1.0).expect("a tempo"), 120.0, 1e-9);
        assert_close(tap.tap(1.5).expect("a tempo"), 120.0, 1e-9);
        // The 2.0 s gap from 1.5 s to 3.5 s is longer than one beat at 40
        // BPM plus a quarter second, so the stored taps are cleared.
        assert_eq!(tap.tap(3.5), None);
        for i in 8..=14 {
            assert_close(
                tap.tap(i as f64 * 0.5).expect("a tempo after the pause"),
                120.0,
                1e-9,
            );
        }
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
        // taps are 1.0..=7.0 plus the new tap, then 2.0..=7.0 plus the new
        // taps, and so on.
        // Stored taps 1.0..=7.0, 7.5: six 1.0 s and one 0.5 s intervals, so
        // the median interval is 1.0 s.
        assert_close(tap.tap(7.5).expect("a tempo"), 60.0, 1e-9);
        // Stored taps 2.0..=7.0, 7.5, 8.0: five 1.0 s and two 0.5 s intervals.
        assert_close(tap.tap(8.0).expect("a tempo"), 60.0, 1e-9);
        // Stored taps 3.0..=7.0, 7.5, 8.0, 8.5: four 1.0 s and three 0.5 s
        // intervals, so the median interval is 1.0 s.
        assert_close(tap.tap(8.5).expect("a tempo"), 60.0, 1e-9);
        // Stored taps 4.0..=7.0, 7.5, 8.0, 8.5, 9.0: three 1.0 s and four
        // 0.5 s intervals, so the median interval is 0.5 s. With every tap
        // kept the median interval would still be 1.0 s.
        assert_close(tap.tap(9.0).expect("a tempo"), 120.0, 1e-9);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(TapTempo::default(), TapTempo::new());
    }
}
