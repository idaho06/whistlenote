//! The tempo state of the BPM tab: onsets heard on the microphone, tap
//! tempo, the candidate tempo in the BPM field and the tempo used for
//! recordings.

use crate::music::bpm::{TapTempo, estimate_bpm};

/// Most recent onsets the estimate uses.
pub const ONSET_WINDOW: usize = 12;

/// A longer gap between onsets in seconds starts a new run.
pub const RUN_GAP_S: f64 = 4.0;

/// How long the onset indicator stays lit in seconds.
pub const FLASH_S: f64 = 0.15;

/// Lowest tempo in BPM the candidate accepts.
pub const MIN_BPM: f64 = 40.0;

/// Highest tempo in BPM the candidate accepts.
pub const MAX_BPM: f64 = 220.0;

/// Tempo in BPM the BPM field starts with.
pub const DEFAULT_BPM: f64 = 120.0;

/// The tempo state of the BPM tab.
#[derive(Debug, Clone, PartialEq)]
pub struct BpmState {
    /// At most [`ONSET_WINDOW`] onset times of the current run, oldest first.
    onsets: Vec<f64>,
    /// Engine-clock time of the latest onset in seconds.
    last_onset_s: Option<f64>,
    /// The tap tempo estimator.
    taps: TapTempo,
    /// Tempo in the BPM field.
    candidate_bpm: f64,
    /// Tempo used for recordings.
    record_bpm: f64,
}

impl Default for BpmState {
    fn default() -> Self {
        Self {
            onsets: Vec::new(),
            last_onset_s: None,
            taps: TapTempo::new(),
            candidate_bpm: DEFAULT_BPM,
            record_bpm: DEFAULT_BPM,
        }
    }
}

impl BpmState {
    /// Pushes the onset at `time_s` seconds into the current run and updates the candidate.
    ///
    /// A non-finite `time_s` is ignored. When the latest stored onset lies
    /// more than [`RUN_GAP_S`] before `time_s`, the stored onsets clear
    /// first. At most [`ONSET_WINDOW`] onsets are kept. When the stored
    /// onsets estimate a tempo, it becomes the candidate.
    pub fn push_onset(&mut self, time_s: f64) {
        if !time_s.is_finite() {
            return;
        }
        if self
            .onsets
            .last()
            .is_some_and(|&last| time_s - last > RUN_GAP_S)
        {
            self.onsets.clear();
        }
        self.onsets.push(time_s);
        if self.onsets.len() > ONSET_WINDOW {
            self.onsets.remove(0);
        }
        self.last_onset_s = Some(time_s);
        if let Some(bpm) = self.estimate() {
            self.set_candidate(bpm);
        }
    }

    /// Returns the tempo estimated from the stored onsets, or `None` when there are too few of them.
    pub fn estimate(&self) -> Option<f64> {
        estimate_bpm(&self.onsets)
    }

    /// Returns how many onsets the current run stores.
    pub fn onset_count(&self) -> usize {
        self.onsets.len()
    }

    /// Returns whether the onset indicator is lit at `now_s` seconds: a latest onset `t` exists with `now_s >= t` and `now_s - t` below [`FLASH_S`].
    pub fn flash(&self, now_s: f64) -> bool {
        self.last_onset_s
            .is_some_and(|t| now_s >= t && now_s - t < FLASH_S)
    }

    /// Records a tap at `now_s` seconds, returns its tempo and makes it the candidate.
    pub fn tap(&mut self, now_s: f64) -> Option<f64> {
        let bpm = self.taps.tap(now_s);
        if let Some(bpm) = bpm {
            self.set_candidate(bpm);
        }
        bpm
    }

    /// Returns the tempo in the BPM field.
    pub fn candidate_bpm(&self) -> f64 {
        self.candidate_bpm
    }

    /// Stores `bpm` as the candidate, rounded to one decimal and clamped to [`MIN_BPM`]..=[`MAX_BPM`]; a non-finite `bpm` is ignored.
    pub fn set_candidate(&mut self, bpm: f64) {
        if !bpm.is_finite() {
            return;
        }
        self.candidate_bpm = ((bpm * 10.0).round() / 10.0).clamp(MIN_BPM, MAX_BPM);
    }

    /// Copies the candidate into the tempo used for recordings.
    pub fn use_candidate(&mut self) {
        self.record_bpm = self.candidate_bpm;
    }

    /// Returns the tempo used for recordings.
    pub fn record_bpm(&self) -> f64 {
        self.record_bpm
    }

    /// Clears the onsets and the taps; the candidate and record tempos stay.
    pub fn clear(&mut self) {
        self.onsets.clear();
        self.last_onset_s = None;
        self.taps.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::assert_close;

    #[test]
    fn default_has_no_onsets_and_120_bpm() {
        let state = BpmState::default();
        assert_eq!(state.onset_count(), 0);
        assert_eq!(state.estimate(), None);
        assert_eq!(state.candidate_bpm(), 120.0);
        assert_eq!(state.record_bpm(), 120.0);
        assert!(!state.flash(0.0));
    }

    #[test]
    fn eight_onsets_at_90_bpm_set_the_candidate() {
        let mut state = BpmState::default();
        for i in 0..8 {
            state.push_onset(i as f64 * 60.0 / 90.0);
        }
        let bpm = state.estimate().expect("a tempo is estimated");
        assert_close(bpm, 90.0, 0.5);
        assert_close(state.candidate_bpm(), bpm, 1e-9);
    }

    #[test]
    fn only_the_latest_onsets_are_kept() {
        let mut state = BpmState::default();
        for i in 0..20 {
            state.push_onset(i as f64 * 0.5);
        }
        assert_eq!(state.onset_count(), ONSET_WINDOW);
    }

    #[test]
    fn a_long_gap_starts_a_new_run() {
        let mut state = BpmState::default();
        for i in 0..4 {
            state.push_onset(i as f64 * 0.5);
        }
        state.push_onset(6.5);
        assert_eq!(state.onset_count(), 1);
        for i in 1..4 {
            state.push_onset(6.5 + i as f64);
        }
        assert_close(state.estimate().expect("a tempo is estimated"), 60.0, 0.5);
    }

    #[test]
    fn the_onset_indicator_flashes_for_a_short_time() {
        let mut state = BpmState::default();
        state.push_onset(10.0);
        assert!(state.flash(10.0));
        assert!(state.flash(10.1));
        assert!(!state.flash(10.2));
        assert!(!state.flash(9.9));
    }

    #[test]
    fn taps_set_the_candidate() {
        let mut state = BpmState::default();
        assert_eq!(state.tap(0.0), None);
        assert!(state.tap(0.5).is_some());
        assert!(state.tap(1.0).is_some());
        assert_close(state.tap(1.5).expect("a tempo after four taps"), 120.0, 0.5);
        assert_close(state.candidate_bpm(), 120.0, 0.5);
    }

    #[test]
    fn the_candidate_is_rounded_and_clamped() {
        let mut state = BpmState::default();
        state.set_candidate(300.0);
        assert_eq!(state.candidate_bpm(), MAX_BPM);
        state.set_candidate(10.0);
        assert_eq!(state.candidate_bpm(), MIN_BPM);
        state.set_candidate(97.26);
        assert_eq!(state.candidate_bpm(), 97.3);
        state.set_candidate(f64::NAN);
        assert_eq!(state.candidate_bpm(), 97.3);
    }

    #[test]
    fn use_candidate_copies_the_candidate_into_the_record_tempo() {
        let mut state = BpmState::default();
        state.set_candidate(90.0);
        state.use_candidate();
        assert_eq!(state.record_bpm(), 90.0);
        state.set_candidate(60.0);
        assert_eq!(state.record_bpm(), 90.0);
    }

    #[test]
    fn clear_forgets_the_onsets_and_taps_but_keeps_the_tempos() {
        let mut state = BpmState::default();
        state.push_onset(10.0);
        state.set_candidate(90.0);
        state.use_candidate();
        assert!(state.tap(20.0).is_none());
        state.clear();
        assert_eq!(state.onset_count(), 0);
        assert_eq!(state.estimate(), None);
        assert!(!state.flash(10.0));
        assert_eq!(state.candidate_bpm(), 90.0);
        assert_eq!(state.record_bpm(), 90.0);
        assert_eq!(state.tap(20.1), None);
    }

    #[test]
    fn a_non_finite_onset_is_ignored() {
        let mut state = BpmState::default();
        state.push_onset(f64::NAN);
        state.push_onset(f64::INFINITY);
        assert_eq!(state.onset_count(), 0);
    }
}
