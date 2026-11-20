//! The preferences window: its state — the draft tuning edited in the window, the tuning it opened with, and the take kept for Cancel — and the window drawn from that state while it is open.

use std::ops::RangeInclusive;
use std::path::Path;

use eframe::egui;

use crate::settings::range as r;
use crate::settings::{Settings, Tuning, VERSION};
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
    /// No button was clicked.
    None,
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
        if state.tuning() != &self.original {
            state.apply_tuning(&self.original);
        }
        if let Some((generation, take)) = self.snapshot.take() {
            state.record.restore_take(generation, take);
        }
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
        let settings = Settings {
            version: VERSION,
            session: state.session(),
            tuning: self.draft.clone(),
        }
        .sanitized();
        match settings.save_to(path) {
            Ok(()) => {
                if state.tuning() != &settings.tuning {
                    state.apply_tuning(&settings.tuning);
                }
                self.open = false;
                self.snapshot = None;
                self.save_error = None;
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
        let save_error = self.save_error().map(str::to_string);
        let mut action = Action::None;
        egui::Window::new("Preferences")
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        for &section in &Section::ALL {
                            if ui
                                .selectable_label(self.section == section, section.label())
                                .clicked()
                            {
                                self.set_section(section);
                            }
                        }
                    });
                    // A vertical separator here would take the window's full
                    // height and make the resizable window grow every frame.
                    ui.add_space(SECTION_GAP);
                    ui.vertical(|ui| self.section_ui(ui, recording));
                });
                ui.separator();
                ui.horizontal_top(|ui| {
                    if ui
                        .button("Defaults")
                        .on_hover_text("Resets the values of this section")
                        .clicked()
                    {
                        action = Action::Defaults;
                    }
                    if ui
                        .button("Restore all")
                        .on_hover_text("Resets the values of every section")
                        .clicked()
                    {
                        action = Action::RestoreAll;
                    }
                    // One button high: a layout that takes the window's full
                    // height makes the resizable window grow every frame.
                    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                    let layout = egui::Layout::right_to_left(egui::Align::Center);
                    ui.allocate_ui_with_layout(size, layout, |ui| {
                        if ui
                            .button("Save")
                            .on_hover_text("Saves the values and closes the window")
                            .clicked()
                        {
                            action = Action::Save;
                        }
                        if ui
                            .button("Cancel")
                            .on_hover_text(
                                "Restores the values the window opened with and closes it",
                            )
                            .clicked()
                        {
                            action = Action::Cancel;
                        }
                    });
                });
                if let Some(path) = path {
                    ui.weak(format!("Settings file: {}", path.display()));
                } else {
                    ui.weak("No configuration directory is available");
                }
                if let Some(error) = &save_error {
                    let color = ui.visuals().error_fg_color;
                    ui.colored_label(color, error);
                }
            });
        if escape {
            action = Action::Cancel;
        }
        let saved = match action {
            Action::Cancel => {
                self.cancel(state);
                None
            }
            Action::Save => self.save(state, path),
            Action::Defaults => {
                self.reset_section();
                None
            }
            Action::RestoreAll => {
                self.restore_all();
                None
            }
            Action::None => None,
        };
        if self.open {
            self.apply(state);
        }
        saved
    }

    /// Draws the current section in the window: its heading, the basic rows, the advanced rows when the section has them, and the section notes.
    fn section_ui(&mut self, ui: &mut egui::Ui, recording: bool) {
        let section = self.section;
        let label = section.label();
        ui.heading(label);
        let draft = &mut self.draft;
        egui::Grid::new(("prefs_basic", label))
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| match section {
                Section::Tuning => {
                    drag_row(
                        ui,
                        "A4 reference",
                        egui::DragValue::new(&mut draft.voicing.a4_hz)
                            .range(r::A4_HZ)
                            .speed(0.1)
                            .fixed_decimals(1)
                            .suffix(" Hz"),
                        "Pitch of A4. Note names, cents and the CSV frequencies follow it; MIDI files store note numbers, which a player plays in its own tuning.",
                    );
                    drag_row(
                        ui,
                        "Voiced confidence",
                        egui::DragValue::new(&mut draft.voicing.min_confidence)
                            .range(r::MIN_CONFIDENCE)
                            .speed(0.01)
                            .fixed_decimals(2),
                        "How sure the pitch tracker must be for a frame to count as part of a note; higher drops doubtful frames.",
                    );
                }
                Section::Pitch => {
                    drag_row(
                        ui,
                        "Silence level",
                        egui::DragValue::new(&mut draft.pitch.silence_dbfs)
                            .range(r::SILENCE_DBFS)
                            .speed(0.5)
                            .fixed_decimals(0)
                            .suffix(" dBFS"),
                        "Quieter input counts as silence.",
                    );
                    drag_row(
                        ui,
                        "Clarity",
                        egui::DragValue::new(&mut draft.pitch.clarity)
                            .range(r::CLARITY)
                            .speed(0.01)
                            .fixed_decimals(2),
                        "How clear a pitch must be to be tracked; higher ignores breathy sounds.",
                    );
                    let range =
                        within(r::MIN_HZ, draft.pitch.band_low_hz + 1.0, draft.pitch.max_hz - 1.0);
                    drag_row(
                        ui,
                        "Lowest pitch",
                        egui::DragValue::new(&mut draft.pitch.min_hz)
                            .range(range)
                            .speed(5.0)
                            .fixed_decimals(0)
                            .suffix(" Hz"),
                        "Lower pitches are ignored.",
                    );
                    let range =
                        within(r::MAX_HZ, draft.pitch.min_hz + 1.0, draft.pitch.band_high_hz);
                    drag_row(
                        ui,
                        "Highest pitch",
                        egui::DragValue::new(&mut draft.pitch.max_hz)
                            .range(range)
                            .speed(5.0)
                            .fixed_decimals(0)
                            .suffix(" Hz"),
                        "Higher pitches are ignored; the tracker also stays below a quarter of the input sample rate.",
                    );
                }
                Section::Onsets => {
                    drag_row(
                        ui,
                        "Clap gate",
                        egui::DragValue::new(&mut draft.onsets.gate_dbfs)
                            .range(r::GATE_DBFS)
                            .speed(0.5)
                            .fixed_decimals(0)
                            .suffix(" dBFS"),
                        "Quieter claps are ignored.",
                    );
                    drag_row(
                        ui,
                        "Sensitivity offset",
                        egui::DragValue::new(&mut draft.onsets.threshold)
                            .range(r::THRESHOLD)
                            .speed(0.01)
                            .fixed_decimals(2),
                        "How far a clap must rise above the recent sound; lower detects softer claps and more false ones.",
                    );
                    drag_row(
                        ui,
                        "Time between claps",
                        egui::DragValue::new(&mut draft.onsets.refractory_ms)
                            .range(r::REFRACTORY_MS)
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                        "A clap sooner than this after the previous one is ignored.",
                    );
                }
                Section::Notes => {
                    drag_row(
                        ui,
                        "Shortest note",
                        egui::DragValue::new(&mut draft.notes.min_note_ms)
                            .range(r::MIN_NOTE_MS)
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                        "Shorter notes are dropped.",
                    );
                    drag_row(
                        ui,
                        "Median smoothing",
                        egui::DragValue::new(&mut draft.notes.median_frames)
                            .range(r::MEDIAN_FRAMES)
                            .speed(0.1)
                            .fixed_decimals(0)
                            .suffix(" frames"),
                        "Smooths the pitch over this many frames; higher steadies wobbly notes and blurs fast ones.",
                    );
                    drag_row(
                        ui,
                        "Join gap",
                        egui::DragValue::new(&mut draft.notes.max_gap_ms)
                            .range(r::MAX_GAP_MS)
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                        "Two equal notes with a shorter gap between them join into one.",
                    );
                    drag_row(
                        ui,
                        "Merge short notes",
                        egui::DragValue::new(&mut draft.notes.merge_note_ms)
                            .range(r::MERGE_NOTE_MS)
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                        "Shorter notes that touch a neighbour merge into it.",
                    );
                }
                Section::Timing => {
                    drag_row(
                        ui,
                        "Pre-roll",
                        egui::DragValue::new(&mut draft.timing.pre_roll_ms)
                            .range(r::PRE_ROLL_MS)
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                        "How much audio before the first beat a recording keeps, so a note started slightly early is not cut.",
                    );
                }
                Section::Metronome => {
                    drag_row(
                        ui,
                        "Click volume",
                        egui::DragValue::new(&mut draft.metronome.volume_percent)
                            .range(r::VOLUME_PERCENT)
                            .speed(0.5)
                            .fixed_decimals(0)
                            .suffix(" %"),
                        "Volume of the metronome click.",
                    );
                }
                Section::Tempo => {
                    drag_row(
                        ui,
                        "Claps used",
                        egui::DragValue::new(&mut draft.tempo.onset_window)
                            .range(r::ONSET_WINDOW)
                            .speed(0.1)
                            .fixed_decimals(0)
                            .suffix(" claps"),
                        "The tempo is estimated from this many most recent claps.",
                    );
                    drag_row(
                        ui,
                        "New run after",
                        egui::DragValue::new(&mut draft.tempo.run_gap_ms)
                            .range(r::RUN_GAP_MS)
                            .speed(10.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                        "A pause longer than this starts a new tempo estimate.",
                    );
                }
                Section::Live => {
                    drag_row(
                        ui,
                        "Agreeing frames",
                        egui::DragValue::new(&mut draft.live.agree_frames)
                            .range(r::AGREE_FRAMES)
                            .speed(0.1)
                            .fixed_decimals(0)
                            .suffix(" frames"),
                        "Frames in a row that must agree before a new note is shown; higher is steadier and slower.",
                    );
                    drag_row(
                        ui,
                        "Clear after",
                        egui::DragValue::new(&mut draft.live.clear_ms)
                            .range(r::CLEAR_MS)
                            .speed(5.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                        "The shown note clears after this much silence.",
                    );
                    drag_row(
                        ui,
                        "In-tune band",
                        egui::DragValue::new(&mut draft.live.in_tune_cents)
                            .range(r::IN_TUNE_CENTS)
                            .speed(0.1)
                            .fixed_decimals(0)
                            .suffix(" cents"),
                        "The needle shows in tune within this many cents.",
                    );
                }
                Section::Export => {
                    drag_row(
                        ui,
                        "MIDI velocity",
                        egui::DragValue::new(&mut draft.export.velocity)
                            .range(r::VELOCITY)
                            .speed(0.1)
                            .fixed_decimals(0),
                        "Velocity of the exported notes.",
                    );
                    drag_row(
                        ui,
                        "MIDI channel",
                        egui::DragValue::new(&mut draft.export.channel)
                            .range(r::CHANNEL)
                            .speed(0.1)
                            .fixed_decimals(0),
                        "Channel of the exported notes; channel 10 plays drums on General MIDI players.",
                    );
                }
            });
        if matches!(
            section,
            Section::Pitch | Section::Onsets | Section::Metronome | Section::Tempo | Section::Live
        ) {
            egui::CollapsingHeader::new("Advanced")
                .id_salt(("prefs_advanced", label))
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new(("prefs_advanced_rows", label))
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| match section {
                            Section::Pitch => {
                                let range =
                                    within(r::BAND_LOW_HZ, f64::MIN, draft.pitch.min_hz - 1.0);
                                drag_row(
                                    ui,
                                    "Band-pass low edge",
                                    egui::DragValue::new(&mut draft.pitch.band_low_hz)
                                        .range(range)
                                        .speed(5.0)
                                        .fixed_decimals(0)
                                        .suffix(" Hz"),
                                    "The input is filtered to this band before pitch tracking.",
                                );
                                let range =
                                    within(r::BAND_HIGH_HZ, draft.pitch.max_hz, f64::MAX);
                                drag_row(
                                    ui,
                                    "Band-pass high edge",
                                    egui::DragValue::new(&mut draft.pitch.band_high_hz)
                                        .range(range)
                                        .speed(5.0)
                                        .fixed_decimals(0)
                                        .suffix(" Hz"),
                                    "The input is filtered to this band before pitch tracking.",
                                );
                            }
                            Section::Onsets => {
                                drag_row(
                                    ui,
                                    "High-pass cutoff",
                                    egui::DragValue::new(&mut draft.onsets.high_pass_hz)
                                        .range(r::HIGH_PASS_HZ)
                                        .speed(5.0)
                                        .fixed_decimals(0)
                                        .suffix(" Hz"),
                                    "Clap detection listens above this frequency.",
                                );
                                drag_row(
                                    ui,
                                    "Flux history",
                                    egui::DragValue::new(&mut draft.onsets.history_ms)
                                        .range(r::HISTORY_MS)
                                        .speed(5.0)
                                        .fixed_decimals(0)
                                        .suffix(" ms"),
                                    "How much recent sound the sensitivity offset is measured against.",
                                );
                            }
                            Section::Metronome => {
                                drag_row(
                                    ui,
                                    "Accent pitch",
                                    egui::DragValue::new(&mut draft.metronome.accent_hz)
                                        .range(r::CLICK_HZ)
                                        .speed(5.0)
                                        .fixed_decimals(0)
                                        .suffix(" Hz"),
                                    "Pitch of the first click of a bar.",
                                );
                                drag_row(
                                    ui,
                                    "Beat pitch",
                                    egui::DragValue::new(&mut draft.metronome.beat_hz)
                                        .range(r::CLICK_HZ)
                                        .speed(5.0)
                                        .fixed_decimals(0)
                                        .suffix(" Hz"),
                                    "Pitch of the other clicks.",
                                );
                            }
                            Section::Tempo => {
                                let range = within(r::MIN_BPM, f64::MIN, draft.tempo.max_bpm / 2.0);
                                drag_row(
                                    ui,
                                    "Slowest tempo",
                                    egui::DragValue::new(&mut draft.tempo.min_bpm)
                                        .range(range)
                                        .speed(0.5)
                                        .fixed_decimals(0)
                                        .suffix(" BPM"),
                                    "Tempos are reported between the slowest and the fastest tempo; the fastest is at least twice the slowest.",
                                );
                                let range = within(r::MAX_BPM, draft.tempo.min_bpm * 2.0, f64::MAX);
                                drag_row(
                                    ui,
                                    "Fastest tempo",
                                    egui::DragValue::new(&mut draft.tempo.max_bpm)
                                        .range(range)
                                        .speed(0.5)
                                        .fixed_decimals(0)
                                        .suffix(" BPM"),
                                    "Tempos are reported between the slowest and the fastest tempo; the fastest is at least twice the slowest.",
                                );
                            }
                            Section::Live => {
                                drag_row(
                                    ui,
                                    "Hold range",
                                    egui::DragValue::new(&mut draft.live.hold_cents)
                                        .range(r::HOLD_CENTS)
                                        .speed(0.5)
                                        .fixed_decimals(0)
                                        .suffix(" cents"),
                                    "The shown note stays while the pitch is within this many cents of it.",
                                );
                                drag_row(
                                    ui,
                                    "Level meter floor",
                                    egui::DragValue::new(&mut draft.live.level_floor_db)
                                        .range(r::LEVEL_FLOOR_DB)
                                        .speed(0.5)
                                        .fixed_decimals(0)
                                        .suffix(" dB"),
                                    "Bottom of the input level meter.",
                                );
                            }
                            _ => {}
                        });
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

/// Returns `range` narrowed to the bounds `low` and `high` without inverting it: the start is the larger of the range start and `low`, and the end is the larger of that start and the smaller of the range end and `high`, so crossed bounds give a one-point range.
fn within(range: RangeInclusive<f64>, low: f64, high: f64) -> RangeInclusive<f64> {
    let start = range.start().max(low);
    let end = start.max(range.end().min(high));
    start..=end
}

/// Draws one row of a settings grid: `label` in the first column and the `drag` value in the second column, both with `hover` as their hover text, then ends the row.
fn drag_row(ui: &mut egui::Ui, label: &str, drag: egui::DragValue<'_>, hover: &str) {
    ui.label(label).on_hover_text(hover);
    ui.add(drag).on_hover_text(hover);
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::engine::{BeatPhase, EngineEvent};
    use crate::settings::Pitch;
    use crate::ui::record_mode::test_support::{beat, recorded_state, tone_frames};

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
        state.apply_tuning(&Tuning::default());
        let mut frames = tone_frames(1.0, 0.5, 440.0);
        frames.extend(tone_frames(2.0, 0.5, 523.25));
        frames.extend(tone_frames(3.0, 0.5, 659.26));
        state.record = recorded_state(frames).0;
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
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.open(&state);
        assert_eq!(prefs.draft().voicing.a4_hz, 432.0);
    }

    #[test]
    fn apply_applies_a_changed_draft_and_ignores_a_closed_window() {
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
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
        state.apply_tuning(&Tuning::default());
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
        assert!(state.record.start(120.0).is_some());
        for i in 0..4 {
            state.record.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state
            .record
            .apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        state.record.apply_event(&beat(0, BeatPhase::Recording));
        assert!(state.record.stop().is_some());
        state.record.apply_event(&EngineEvent::RecordingFinished {
            frames: tone_frames(1.0, 0.5, 440.0),
            origin_s: 1.0,
        });
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
        let path = dir.join("settings.toml");
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
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
        state.apply_tuning(&Tuning::default());
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
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        let path = dir.join("blocker").join("settings.toml");
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

    #[test]
    fn within_narrows_the_range_to_the_inner_bounds() {
        assert_eq!(within(0.0..=100.0, 20.0, 80.0), 20.0..=80.0);
        assert_eq!(within(50.0..=4000.0, 300.0, 3000.0), 300.0..=3000.0);
        assert_eq!(within(50.0..=4000.0, f64::MIN, f64::MAX), 50.0..=4000.0);
    }

    #[test]
    fn within_gives_a_one_point_range_for_crossed_bounds() {
        assert_eq!(within(50.0..=100.0, 200.0, 300.0), 200.0..=200.0);
        assert_eq!(within(50.0..=100.0, 80.0, 60.0), 80.0..=80.0);
        let range = within(50.0..=100.0, 200.0, 300.0);
        assert_eq!(range.start(), range.end());
    }

    #[test]
    fn show_draws_every_section_and_stays_open_without_saving() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        let mut saved: Option<Tuning> = None;
        for &section in &Section::ALL {
            prefs.set_section(section);
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                saved = prefs.show(ui.ctx(), &mut state, None);
            });
            output.drop_without_applying_deltas();
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
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.apply(&mut state);
        assert_eq!(state.tuning().voicing.a4_hz, 432.0);
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = prefs.show(ui.ctx(), &mut state, None);
        });
        output.drop_without_applying_deltas();
        assert!(prefs.is_open(), "without Esc the window stays open");
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let output = ctx.run_ui(input, |ui| {
            let _ = prefs.show(ui.ctx(), &mut state, None);
        });
        output.drop_without_applying_deltas();
        assert!(!prefs.is_open(), "Esc closes the window");
        assert_eq!(state.tuning().voicing.a4_hz, 440.0);
    }

    #[test]
    fn the_window_keeps_its_size_over_frames() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        let window_rect = |ctx: &egui::Context| {
            ctx.memory(|m| {
                m.layer_ids()
                    .filter(|layer| layer.order == egui::Order::Middle)
                    .find_map(|layer| m.area_rect(layer.id))
            })
            .expect("the window is drawn")
        };
        let mut sizes = Vec::new();
        for &section in &Section::ALL {
            prefs.set_section(section);
            for _ in 0..20 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 900.0),
                    )),
                    ..Default::default()
                };
                let output = ctx.run_ui(input, |ui| {
                    let _ = prefs.show(ui.ctx(), &mut state, None);
                });
                output.drop_without_applying_deltas();
            }
            let first = window_rect(&ctx).size();
            for _ in 0..20 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 900.0),
                    )),
                    ..Default::default()
                };
                let output = ctx.run_ui(input, |ui| {
                    let _ = prefs.show(ui.ctx(), &mut state, None);
                });
                output.drop_without_applying_deltas();
            }
            let last = window_rect(&ctx).size();
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
