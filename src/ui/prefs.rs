//! The preferences window: its state — the draft tuning edited in the window, the tuning it opened with, and the take kept for Cancel — and the window drawn from that state while it is open.

use std::ops::RangeInclusive;
use std::path::Path;

use eframe::egui;

use crate::settings::range as r;
use crate::settings::{Settings, Tuning};
use crate::ui::record_mode::Take;
use crate::ui::state::AppState;

/// A section of the preferences window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    /// Reference pitch and voiced-frame confidence.
    #[default]
    Tuning,
    /// Pitch tracking.
    Pitch,
    /// Clap and tap detection.
    Onsets,
    /// Segmentation of pitch frames into notes.
    Notes,
    /// Recording timing.
    Timing,
    /// The click sound.
    Metronome,
    /// The tempo estimate from claps.
    Tempo,
    /// The live note readout.
    Live,
    /// MIDI export.
    Export,
}

impl Section {
    /// Every section, in the order the window lists them.
    pub const ALL: [Section; 9] = [
        Section::Tuning,
        Section::Pitch,
        Section::Onsets,
        Section::Notes,
        Section::Timing,
        Section::Metronome,
        Section::Tempo,
        Section::Live,
        Section::Export,
    ];

    /// Returns the section's label.
    pub fn label(self) -> &'static str {
        match self {
            Section::Tuning => "Tuning",
            Section::Pitch => "Pitch",
            Section::Onsets => "Onsets",
            Section::Notes => "Notes",
            Section::Timing => "Timing",
            Section::Metronome => "Metronome",
            Section::Tempo => "Tempo",
            Section::Live => "Live",
            Section::Export => "Export",
        }
    }
}

/// State of the preferences window.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrefsWindow {
    /// Whether the window is open.
    open: bool,
    /// The tuning edited in the window, applied live while the window is open.
    draft: Tuning,
    /// The tuning the window opened with, restored by Cancel.
    original: Tuning,
    /// The shown take as it was just before the first re-segment of this window session, with its generation; restored by Cancel.
    snapshot: Option<(u64, Take)>,
    /// The section shown in the window.
    section: Section,
    /// The error of the latest failed save, if any.
    save_error: Option<String>,
}

/// Space between the section list and the values of the section, in points.
const SECTION_GAP: f32 = 16.0;

/// A button of the preferences window clicked in one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// "Defaults" resets the values of the current section.
    Defaults,
    /// "Restore all" resets the values of every section.
    RestoreAll,
    /// "Cancel" restores the values the window opened with and closes the window.
    Cancel,
    /// "Save" saves the values and closes the window.
    Save,
}

impl PrefsWindow {
    /// Returns whether the window is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the window with the applied tuning of `state` as the draft and the original and clears the take snapshot and the save error; an already open window keeps its draft, and the section keeps its last value either way.
    pub fn open(&mut self, state: &AppState) {
        if self.open {
            return;
        }
        self.draft = state.tuning().clone();
        self.original = state.tuning().clone();
        self.snapshot = None;
        self.save_error = None;
        self.open = true;
    }

    /// Returns the draft tuning edited in the window.
    pub fn draft(&self) -> &Tuning {
        &self.draft
    }

    /// Returns the draft tuning mutably.
    pub fn draft_mut(&mut self) -> &mut Tuning {
        &mut self.draft
    }

    /// Returns the section shown in the window.
    pub fn section(&self) -> Section {
        self.section
    }

    /// Shows `section` in the window.
    pub fn set_section(&mut self, section: Section) {
        self.section = section;
    }

    /// Returns the error of the latest failed save, if any.
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Applies the draft to `state`, called every frame: does nothing when the window is closed or the applied tuning already equals the draft; otherwise keeps the shown take with its edits in the snapshot when it is about to be re-segmented for the first time in this window session, then applies the draft.
    pub fn apply(&mut self, state: &mut AppState) {
        if !self.open || state.tuning() == &self.draft {
            return;
        }
        if self.snapshot.is_none()
            && state.tuning().segment() != self.draft.segment()
            && let Some(take) = state.record.take()
        {
            self.snapshot = Some((state.record.take_generation(), take.clone()));
        }
        state.apply_tuning(&self.draft);
    }

    /// Cancels the window: restores the original tuning and the snapshotted take, then closes; a take recorded since the snapshot is kept, because the restore checks the generation. Does nothing when the window is closed.
    pub fn cancel(&mut self, state: &mut AppState) {
        if !self.open {
            return;
        }
        state.apply_tuning(&self.original);
        if let Some((generation, take)) = self.snapshot.take() {
            state.record.restore_take(generation, take);
        }
        self.close();
    }

    /// Closes the window and drops the take snapshot and the save error.
    fn close(&mut self) {
        self.open = false;
        self.snapshot = None;
        self.save_error = None;
    }

    /// Resets the sub-struct of the current section in the draft to its default; changes only the draft, never the session.
    pub fn reset_section(&mut self) {
        match self.section {
            Section::Tuning => self.draft.voicing = Default::default(),
            Section::Pitch => self.draft.pitch = Default::default(),
            Section::Onsets => self.draft.onsets = Default::default(),
            Section::Notes => self.draft.notes = Default::default(),
            Section::Timing => self.draft.timing = Default::default(),
            Section::Metronome => self.draft.metronome = Default::default(),
            Section::Tempo => self.draft.tempo = Default::default(),
            Section::Live => self.draft.live = Default::default(),
            Section::Export => self.draft.export = Default::default(),
        }
    }

    /// Resets the whole draft to the default tuning.
    pub fn restore_all(&mut self) {
        self.draft = Tuning::default();
    }

    /// Saves the session of `state` and the draft to the settings file at `path` and returns the saved tuning: on success applies the saved tuning and closes the window; on failure keeps the window open and stores the error. With `path == None` stores an error and returns `None`.
    pub fn save(&mut self, state: &mut AppState, path: Option<&Path>) -> Option<Tuning> {
        let Some(path) = path else {
            self.save_error = Some(
                "No configuration directory is available, so the settings cannot be saved."
                    .to_string(),
            );
            return None;
        };
        let settings = Settings::new(state.session(), self.draft.clone());
        match settings.save_to(path) {
            Ok(()) => {
                state.apply_tuning(&settings.tuning);
                self.close();
                Some(settings.tuning)
            }
            Err(error) => {
                self.save_error = Some(format!("Could not save {}: {error}", path.display()));
                None
            }
        }
    }

    /// Draws the window while it is open, applies the draft live and handles the buttons; returns the saved tuning when Save succeeds.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &mut AppState,
        path: Option<&Path>,
    ) -> Option<Tuning> {
        if !self.open {
            return None;
        }
        let recording = !state.record.is_idle();
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape))
            && ctx.memory(|m| m.focused().is_none());
        let mut action = None;
        egui::Window::new("Preferences")
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        for section in Section::ALL {
                            if ui
                                .selectable_label(self.section == section, section.label())
                                .clicked()
                            {
                                self.section = section;
                            }
                        }
                    });
                    // A vertical separator here would take the window's full
                    // height and make the resizable window grow every frame.
                    ui.add_space(SECTION_GAP);
                    ui.vertical(|ui| self.section_ui(ui, recording));
                });
                ui.separator();
                let (left, right) = egui::Sides::new().show(
                    ui,
                    |ui| {
                        let defaults = button(ui, "Defaults", "Resets the values of this section");
                        let all = button(ui, "Restore all", "Resets the values of every section");
                        clicked(&[(defaults, Action::Defaults), (all, Action::RestoreAll)])
                    },
                    |ui| {
                        let save = button(ui, "Save", "Saves the values and closes the window");
                        let cancel = button(
                            ui,
                            "Cancel",
                            "Restores the values the window opened with and closes it",
                        );
                        clicked(&[(save, Action::Save), (cancel, Action::Cancel)])
                    },
                );
                action = left.or(right);
                if let Some(path) = path {
                    ui.weak(format!("Settings file: {}", path.display()));
                } else {
                    ui.weak("No configuration directory is available");
                }
                if let Some(error) = &self.save_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
        if escape {
            action = Some(Action::Cancel);
        }
        let saved = match action {
            Some(Action::Cancel) => {
                self.cancel(state);
                None
            }
            Some(Action::Save) => self.save(state, path),
            Some(Action::Defaults) => {
                self.reset_section();
                None
            }
            Some(Action::RestoreAll) => {
                self.restore_all();
                None
            }
            None => None,
        };
        self.apply(state);
        saved
    }

    /// Draws the current section in the window: its heading, the basic rows, the advanced rows when the section has them, and the section notes.
    fn section_ui(&mut self, ui: &mut egui::Ui, recording: bool) {
        let section = self.section;
        let label = section.label();
        ui.heading(label);
        egui::Grid::new(("prefs_basic", label))
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| basic_rows(ui, section, &mut self.draft));
        if let Some(rows) = advanced_rows(section) {
            egui::CollapsingHeader::new("Advanced")
                .id_salt(("prefs_advanced", label))
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new(("prefs_advanced_rows", label))
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| rows(ui, &mut self.draft));
                });
        }
        if matches!(section, Section::Tuning | Section::Notes) {
            ui.weak(
                "Changing these values re-segments the recorded take and resets its edits; Cancel brings the edits back.",
            );
        } else if recording && matches!(section, Section::Pitch | Section::Onsets | Section::Timing)
        {
            ui.weak("A recording runs; these values take effect when it ends.");
        }
    }
}

/// Hover text shared by the two band-pass edges.
const BAND_HOVER: &str = "The input is filtered to this band before pitch tracking.";

/// Hover text shared by the two tempo limits.
const TEMPO_HOVER: &str = "Tempos are reported between the slowest and the fastest tempo; the fastest is at least twice the slowest.";

/// Draws the basic rows of `section`, editing the tuning `d`.
fn basic_rows(ui: &mut egui::Ui, section: Section, d: &mut Tuning) {
    match section {
        Section::Tuning => {
            let v = &mut d.voicing;
            drag_row(
                ui,
                "A4 reference",
                num(&mut v.a4_hz, r::A4_HZ, 0.1, 1, " Hz"),
                "Pitch of A4. Note names, cents and the CSV frequencies follow it; MIDI files store note numbers, which a player plays in its own tuning.",
            );
            drag_row(
                ui,
                "Voiced confidence",
                num(&mut v.min_confidence, r::MIN_CONFIDENCE, 0.01, 2, ""),
                "How sure the pitch tracker must be for a frame to count as part of a note; higher drops doubtful frames.",
            );
        }
        Section::Pitch => {
            let p = &mut d.pitch;
            drag_row(
                ui,
                "Silence level",
                num(&mut p.silence_dbfs, r::SILENCE_DBFS, 0.5, 0, " dBFS"),
                "Quieter input counts as silence.",
            );
            drag_row(
                ui,
                "Clarity",
                num(&mut p.clarity, r::CLARITY, 0.01, 2, ""),
                "How clear a pitch must be to be tracked; higher ignores breathy sounds.",
            );
            let range = p.min_hz_range();
            drag_row(
                ui,
                "Lowest pitch",
                num(&mut p.min_hz, range, 5.0, 0, " Hz"),
                "Lower pitches are ignored.",
            );
            let range = p.max_hz_range();
            drag_row(
                ui,
                "Highest pitch",
                num(&mut p.max_hz, range, 5.0, 0, " Hz"),
                "Higher pitches are ignored; the tracker also stays below a quarter of the input sample rate.",
            );
        }
        Section::Onsets => {
            let o = &mut d.onsets;
            drag_row(
                ui,
                "Clap gate",
                num(&mut o.gate_dbfs, r::GATE_DBFS, 0.5, 0, " dBFS"),
                "Quieter claps are ignored.",
            );
            drag_row(
                ui,
                "Sensitivity offset",
                num(&mut o.threshold, r::THRESHOLD, 0.01, 2, ""),
                "How far a clap must rise above the recent sound; lower detects softer claps and more false ones.",
            );
            drag_row(
                ui,
                "Time between claps",
                num(&mut o.refractory_ms, r::REFRACTORY_MS, 1.0, 0, " ms"),
                "A clap sooner than this after the previous one is ignored.",
            );
        }
        Section::Notes => {
            let n = &mut d.notes;
            drag_row(
                ui,
                "Shortest note",
                num(&mut n.min_note_ms, r::MIN_NOTE_MS, 1.0, 0, " ms"),
                "Shorter notes are dropped.",
            );
            drag_row(
                ui,
                "Median smoothing",
                count(&mut n.median_frames, r::MEDIAN_FRAMES, " frames"),
                "Smooths the pitch over this many frames; higher steadies wobbly notes and blurs fast ones.",
            );
            drag_row(
                ui,
                "Join gap",
                num(&mut n.max_gap_ms, r::MAX_GAP_MS, 1.0, 0, " ms"),
                "Two equal notes with a shorter gap between them join into one.",
            );
            drag_row(
                ui,
                "Merge short notes",
                num(&mut n.merge_note_ms, r::MERGE_NOTE_MS, 1.0, 0, " ms"),
                "Shorter notes that touch a neighbour merge into it.",
            );
        }
        Section::Timing => drag_row(
            ui,
            "Pre-roll",
            num(&mut d.timing.pre_roll_ms, r::PRE_ROLL_MS, 1.0, 0, " ms"),
            "How much audio before the first beat a recording keeps, so a note started slightly early is not cut.",
        ),
        Section::Metronome => drag_row(
            ui,
            "Click volume",
            num(
                &mut d.metronome.volume_percent,
                r::VOLUME_PERCENT,
                0.5,
                0,
                " %",
            ),
            "Volume of the metronome click.",
        ),
        Section::Tempo => {
            let t = &mut d.tempo;
            drag_row(
                ui,
                "Claps used",
                count(&mut t.onset_window, r::ONSET_WINDOW, " claps"),
                "The tempo is estimated from this many most recent claps.",
            );
            drag_row(
                ui,
                "New run after",
                num(&mut t.run_gap_ms, r::RUN_GAP_MS, 10.0, 0, " ms"),
                "A pause longer than this starts a new tempo estimate.",
            );
        }
        Section::Live => {
            let l = &mut d.live;
            drag_row(
                ui,
                "Agreeing frames",
                count(&mut l.agree_frames, r::AGREE_FRAMES, " frames"),
                "Frames in a row that must agree before a new note is shown; higher is steadier and slower.",
            );
            drag_row(
                ui,
                "Clear after",
                num(&mut l.clear_ms, r::CLEAR_MS, 5.0, 0, " ms"),
                "The shown note clears after this much silence.",
            );
            drag_row(
                ui,
                "In-tune band",
                num(&mut l.in_tune_cents, r::IN_TUNE_CENTS, 0.1, 0, " cents"),
                "The needle shows in tune within this many cents.",
            );
        }
        Section::Export => {
            let e = &mut d.export;
            drag_row(
                ui,
                "MIDI velocity",
                count(&mut e.velocity, r::VELOCITY, ""),
                "Velocity of the exported notes.",
            );
            drag_row(
                ui,
                "MIDI channel",
                count(&mut e.channel, r::CHANNEL, ""),
                "Channel of the exported notes; channel 10 plays drums on General MIDI players.",
            );
        }
    }
}

/// Returns the function that draws the Advanced rows of `section`, editing the tuning, or `None` for a section without an Advanced area.
fn advanced_rows(section: Section) -> Option<fn(&mut egui::Ui, &mut Tuning)> {
    match section {
        Section::Pitch => Some(|ui, d| {
            let p = &mut d.pitch;
            let range = p.band_low_hz_range();
            drag_row(
                ui,
                "Band-pass low edge",
                num(&mut p.band_low_hz, range, 5.0, 0, " Hz"),
                BAND_HOVER,
            );
            let range = p.band_high_hz_range();
            drag_row(
                ui,
                "Band-pass high edge",
                num(&mut p.band_high_hz, range, 5.0, 0, " Hz"),
                BAND_HOVER,
            );
        }),
        Section::Onsets => Some(|ui, d| {
            let o = &mut d.onsets;
            drag_row(
                ui,
                "High-pass cutoff",
                num(&mut o.high_pass_hz, r::HIGH_PASS_HZ, 5.0, 0, " Hz"),
                "Clap detection listens above this frequency.",
            );
            drag_row(
                ui,
                "Flux history",
                num(&mut o.history_ms, r::HISTORY_MS, 5.0, 0, " ms"),
                "How much recent sound the sensitivity offset is measured against.",
            );
        }),
        Section::Metronome => Some(|ui, d| {
            let m = &mut d.metronome;
            drag_row(
                ui,
                "Accent pitch",
                num(&mut m.accent_hz, r::CLICK_HZ, 5.0, 0, " Hz"),
                "Pitch of the first click of a bar.",
            );
            drag_row(
                ui,
                "Beat pitch",
                num(&mut m.beat_hz, r::CLICK_HZ, 5.0, 0, " Hz"),
                "Pitch of the other clicks.",
            );
        }),
        Section::Tempo => Some(|ui, d| {
            let t = &mut d.tempo;
            let range = t.min_bpm_range();
            drag_row(
                ui,
                "Slowest tempo",
                num(&mut t.min_bpm, range, 0.5, 0, " BPM"),
                TEMPO_HOVER,
            );
            let range = t.max_bpm_range();
            drag_row(
                ui,
                "Fastest tempo",
                num(&mut t.max_bpm, range, 0.5, 0, " BPM"),
                TEMPO_HOVER,
            );
        }),
        Section::Live => Some(|ui, d| {
            let l = &mut d.live;
            drag_row(
                ui,
                "Hold range",
                num(&mut l.hold_cents, r::HOLD_CENTS, 0.5, 0, " cents"),
                "The shown note stays while the pitch is within this many cents of it.",
            );
            drag_row(
                ui,
                "Level meter floor",
                num(&mut l.level_floor_db, r::LEVEL_FLOOR_DB, 0.5, 0, " dB"),
                "Bottom of the input level meter.",
            );
        }),
        Section::Tuning | Section::Notes | Section::Timing | Section::Export => None,
    }
}

/// Returns a drag value for `value` within `range`, changing by `speed` per dragged point and shown with `decimals` decimals and `suffix`.
fn num<'a>(
    value: &'a mut f64,
    range: RangeInclusive<f64>,
    speed: f64,
    decimals: usize,
    suffix: &'static str,
) -> egui::DragValue<'a> {
    egui::DragValue::new(value)
        .range(range)
        .speed(speed)
        .fixed_decimals(decimals)
        .suffix(suffix)
}

/// Returns a drag value for the whole number `value` within `range`, shown with `suffix`.
fn count<'a>(
    value: &'a mut u32,
    range: RangeInclusive<u32>,
    suffix: &'static str,
) -> egui::DragValue<'a> {
    egui::DragValue::new(value)
        .range(range)
        .speed(0.1)
        .suffix(suffix)
}

/// Draws one row of a settings grid: `label` in the first column and the `drag` value in the second column, both with `hover` as their hover text, then ends the row.
fn drag_row(ui: &mut egui::Ui, label: &str, drag: egui::DragValue<'_>, hover: &str) {
    ui.label(label).on_hover_text(hover);
    ui.add(drag).on_hover_text(hover);
    ui.end_row();
}

/// Returns the action of the first clicked button among `buttons`, each a click flag with its action.
fn clicked(buttons: &[(bool, Action)]) -> Option<Action> {
    buttons
        .iter()
        .find_map(|&(clicked, action)| clicked.then_some(action))
}

/// Draws a button with `hover` as its hover text and returns whether it was clicked.
fn button(ui: &mut egui::Ui, label: &str, hover: &str) -> bool {
    ui.button(label).on_hover_text(hover).clicked()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::settings::{FILE_NAME, Pitch};
    use crate::ui::record_mode::test_support::{
        record_into, recorded_state, three_tone_frames, tone_frames,
    };

    /// Returns a fresh scratch directory for the test `name` in the temporary directory.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("whistlenote-prefs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Returns the state with a finished take of three tones at 1.0, 2.0 and 3.0 s recorded at 120 BPM; the take has 3 notes.
    fn state_with_take() -> AppState {
        let mut state = AppState::default();
        state.record = recorded_state(three_tone_frames()).0;
        let take = state.record.take().expect("a take");
        assert_eq!(take.notes().len(), 3);
        state
    }

    #[test]
    fn opening_copies_the_applied_tuning() {
        let mut state = AppState::default();
        let mut tuning = Tuning::default();
        tuning.voicing.a4_hz = 432.0;
        state.apply_tuning(&tuning);
        let mut prefs = PrefsWindow::default();
        assert!(!prefs.is_open());
        prefs.open(&state);
        assert!(prefs.is_open());
        assert_eq!(prefs.draft(), state.tuning());
    }

    #[test]
    fn opening_an_open_window_keeps_the_draft() {
        let state = AppState::default();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.open(&state);
        assert_eq!(prefs.draft().voicing.a4_hz, 432.0);
    }

    #[test]
    fn apply_applies_a_changed_draft_and_ignores_a_closed_window() {
        let mut state = AppState::default();
        let mut prefs = PrefsWindow::default();
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.apply(&mut state);
        assert_eq!(
            state.tuning().voicing.a4_hz,
            440.0,
            "closed: nothing is applied"
        );
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.apply(&mut state);
        assert_eq!(state.tuning().voicing.a4_hz, 432.0);
    }

    #[test]
    fn cancel_restores_the_original_tuning_and_closes() {
        let mut state = AppState::default();
        let mut prefs = PrefsWindow::default();
        prefs.cancel(&mut state);
        assert_eq!(
            state.tuning(),
            &Tuning::default(),
            "closed: nothing happens"
        );
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.apply(&mut state);
        assert_eq!(state.tuning().voicing.a4_hz, 432.0);
        prefs.cancel(&mut state);
        assert!(!prefs.is_open());
        assert_eq!(state.tuning(), &Tuning::default());
    }

    #[test]
    fn cancel_restores_an_edited_take_after_a_resegment() {
        let mut state = state_with_take();
        state.record.take_mut().expect("a take").delete(0);
        let edited = state.record.take().expect("a take").clone();
        assert_eq!(edited.notes().len(), 2);
        assert!(edited.is_edited());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().notes.min_note_ms = 200.0;
        prefs.apply(&mut state);
        let take = state.record.take().expect("a take");
        assert!(!take.is_edited());
        assert_eq!(take.notes().len(), 3);
        prefs.cancel(&mut state);
        assert_eq!(state.record.take(), Some(&edited));
    }

    #[test]
    fn edits_survive_cancel_when_nothing_resegments() {
        let mut state = state_with_take();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        state.record.take_mut().expect("a take").delete(0);
        prefs.draft_mut().export.velocity = 64;
        prefs.apply(&mut state);
        prefs.cancel(&mut state);
        let take = state.record.take().expect("a take");
        assert_eq!(take.notes().len(), 2);
        assert!(take.is_edited());
    }

    #[test]
    fn cancel_keeps_a_take_recorded_after_the_resegment() {
        let mut state = state_with_take();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().notes.min_note_ms = 200.0;
        prefs.apply(&mut state);
        record_into(&mut state.record, tone_frames(1.0, 0.5, 440.0));
        prefs.cancel(&mut state);
        let take = state.record.take().expect("a take");
        assert_eq!(take.notes().len(), 1);
    }

    #[test]
    fn reset_section_resets_only_the_current_section() {
        let mut prefs = PrefsWindow::default();
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.draft_mut().pitch.clarity = 0.7;
        prefs.set_section(Section::Pitch);
        assert_eq!(prefs.section(), Section::Pitch);
        prefs.reset_section();
        assert_eq!(prefs.draft().pitch, Pitch::default());
        assert_eq!(prefs.draft().voicing.a4_hz, 432.0);
    }

    #[test]
    fn restore_all_resets_the_draft() {
        let mut prefs = PrefsWindow::default();
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.restore_all();
        assert_eq!(prefs.draft(), &Tuning::default());
    }

    #[test]
    fn save_writes_the_settings_and_closes() {
        let dir = scratch_dir("save");
        let path = dir.join(FILE_NAME);
        let mut state = AppState::default();
        assert!(state.set_latency_ms(25.0).is_some());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        let saved = prefs
            .save(&mut state, Some(&path))
            .expect("the settings are saved");
        assert_eq!(saved.voicing.a4_hz, 432.0);
        assert!(!prefs.is_open());
        assert_eq!(prefs.save_error(), None);
        assert_eq!(state.tuning().voicing.a4_hz, 432.0);
        let (loaded, warning) = Settings::load_from(&path);
        assert_eq!(warning, None);
        assert_eq!(loaded.tuning, saved);
        assert_eq!(loaded.session.latency_ms, 25.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_without_a_path_keeps_the_window_open_with_an_error() {
        let mut state = AppState::default();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        assert_eq!(prefs.save(&mut state, None), None);
        assert!(prefs.is_open());
        let error = prefs.save_error().expect("an error");
        assert!(
            error.contains("No configuration directory"),
            "unexpected: {error}"
        );
    }

    #[test]
    fn save_to_an_unwritable_path_keeps_the_window_open_with_an_error() {
        let dir = scratch_dir("unwritable");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("blocker"), "a file").unwrap();
        let mut state = AppState::default();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        let path = dir.join("blocker").join(FILE_NAME);
        assert_eq!(prefs.save(&mut state, Some(&path)), None);
        assert!(prefs.is_open());
        let error = prefs.save_error().expect("an error");
        assert!(error.contains("Could not save"), "unexpected: {error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sections_have_distinct_labels() {
        assert_eq!(Section::ALL.len(), 9);
        let mut labels: Vec<&str> = Section::ALL.iter().map(|section| section.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 9);
        assert_eq!(Section::default(), Section::Tuning);
    }

    /// Runs one frame of `ctx` on a 1200 × 900 screen with `events`, showing `prefs` without a settings path, and returns what `show` returns.
    fn frame(
        ctx: &egui::Context,
        prefs: &mut PrefsWindow,
        state: &mut AppState,
        events: Vec<egui::Event>,
    ) -> Option<Tuning> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let mut saved = None;
        ctx.run_ui(input, |ui| saved = prefs.show(ui.ctx(), state, None))
            .drop_without_applying_deltas();
        saved
    }

    #[test]
    fn show_draws_every_section_and_stays_open_without_saving() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        for section in Section::ALL {
            prefs.set_section(section);
            let saved = frame(&ctx, &mut prefs, &mut state, Vec::new());
            assert!(prefs.is_open(), "{section:?} closed the window");
            assert_eq!(saved, None, "{section:?} saved");
            assert_eq!(
                state.tuning(),
                &Tuning::default(),
                "{section:?} changed the tuning"
            );
        }
    }

    #[test]
    fn escape_cancels_the_window() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.apply(&mut state);
        assert_eq!(state.tuning().voicing.a4_hz, 432.0);
        frame(&ctx, &mut prefs, &mut state, Vec::new());
        assert!(prefs.is_open(), "without Esc the window stays open");
        let escape = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&ctx, &mut prefs, &mut state, vec![escape]);
        assert!(!prefs.is_open(), "Esc closes the window");
        assert_eq!(state.tuning().voicing.a4_hz, 440.0);
    }

    #[test]
    fn the_window_keeps_its_size_over_frames() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        let window_size = |ctx: &egui::Context| {
            ctx.memory(|m| {
                m.layer_ids()
                    .filter(|layer| layer.order == egui::Order::Middle)
                    .find_map(|layer| m.area_rect(layer.id))
            })
            .expect("the window is drawn")
            .size()
        };
        let mut sizes = Vec::new();
        for section in Section::ALL {
            prefs.set_section(section);
            for _ in 0..20 {
                frame(&ctx, &mut prefs, &mut state, Vec::new());
            }
            let first = window_size(&ctx);
            for _ in 0..20 {
                frame(&ctx, &mut prefs, &mut state, Vec::new());
            }
            let last = window_size(&ctx);
            assert_eq!(
                first, last,
                "{section:?}: the window changes size between frames"
            );
            sizes.push(last);
        }
        assert!(
            sizes.iter().all(|size| size.y < 600.0),
            "the window fills the screen: {sizes:?}"
        );
    }
}
