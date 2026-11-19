//! The tempo state of the BPM tab: onsets heard on the microphone, tap
//! tempo, the candidate tempo in the BPM field and the tempo used for
//! recordings, and the view of the tab: the onset indicator, the detected
//! tempo, the tap and candidate controls and the record tempo.

use eframe::egui;

use crate::music::bpm::{TapTempo, estimate_bpm};
use crate::ui::centered_row;
use crate::ui::state::AppState;

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

/// Returns the detected-tempo line: the estimate with the onset count when there is one, otherwise a prompt to clap a steady beat.
fn detected_text(estimate: Option<f64>, onsets: usize) -> String {
    match estimate {
        Some(bpm) => format!("Detected: {bpm:.1} BPM ({onsets} claps)"),
        None => "Clap a steady beat".to_string(),
    }
}

/// Returns the record-tempo line with `bpm`.
fn record_text(bpm: f64) -> String {
    format!("Record tempo: {bpm:.1} BPM")
}

/// Draws the onset indicator: a green filled circle while `lit`, a faint filled circle with a weak outline otherwise.
fn onset_light(ui: &mut egui::Ui, lit: bool) {
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
    let center = rect.center();
    let painter = ui.painter();
    if lit {
        painter.circle_filled(center, 16.0, egui::Color32::from_rgb(60, 180, 75));
    } else {
        painter.circle_filled(center, 16.0, ui.visuals().faint_bg_color);
        painter.circle_stroke(
            center,
            16.0,
            egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        );
    }
}

/// Shows the BPM tab: a hint to turn on Listen while the engine is idle, the onset indicator, the detected tempo, a row with the Tap button, the candidate BPM field, the "Use this BPM" and "Clear" buttons, and the record tempo line, centered in `ui`.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.vertical_centered(|ui| {
        if !state.listening() {
            ui.weak("Turn on Listen to detect claps; Tap works either way.");
        }
        let lit = state.listening()
            && state
                .latest_live()
                .is_some_and(|frame| state.bpm.flash(frame.time_s));
        onset_light(ui, lit);
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(detected_text(state.bpm.estimate(), state.bpm.onset_count()))
                .size(28.0)
                .strong(),
        );
        ui.add_space(8.0);
        centered_row(ui, "bpm_controls", |ui| {
            if ui
                .add(egui::Button::new("Tap").min_size(egui::vec2(80.0, 40.0)))
                .clicked()
            {
                state.bpm.tap(ui.ctx().input(|i| i.time));
            }
            let mut bpm = state.bpm.candidate_bpm();
            let response = ui.add(
                egui::DragValue::new(&mut bpm)
                    .range(MIN_BPM..=MAX_BPM)
                    .speed(0.5)
                    .fixed_decimals(1)
                    .suffix(" BPM"),
            );
            if response.changed() {
                state.bpm.set_candidate(bpm);
            }
            if ui.button("Use this BPM").clicked() {
                state.bpm.use_candidate();
            }
            if ui.button("Clear").clicked() {
                state.bpm.clear();
            }
        });
        ui.add_space(8.0);
        ui.label(record_text(state.bpm.record_bpm()))
            .on_hover_text("The Record tab uses this tempo");
    });
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

    #[test]
    fn detected_text_shows_the_estimate_and_onset_count() {
        assert_eq!(
            detected_text(Some(90.04), 8),
            "Detected: 90.0 BPM (8 claps)"
        );
    }

    #[test]
    fn detected_text_without_an_estimate_asks_for_claps() {
        assert_eq!(detected_text(None, 0), "Clap a steady beat");
        assert_eq!(detected_text(None, 1), "Clap a steady beat");
    }

    #[test]
    fn record_text_shows_the_record_tempo() {
        assert_eq!(record_text(120.0), "Record tempo: 120.0 BPM");
        assert_eq!(record_text(97.3), "Record tempo: 97.3 BPM");
    }
}
