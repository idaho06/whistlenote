//! The Record tab: the recording state — the phase of a recording, the grid and metronome settings, the latest beat and the finished take — and the view that draws it: the tempo, grid and metronome controls, the Record and Stop buttons, the beat indicator with the beat line, and the notes table and the MIDI and CSV export of the finished take.

use eframe::egui;

use crate::engine::{BeatPhase, EngineCommand, EngineEvent};
use crate::export::csv::to_csv;
use crate::export::midi::to_midi_bytes;
use crate::metronome::BEATS_PER_BAR;
use crate::music::note::midi_to_name;
use crate::music::quantize::{Grid, QuantNote, quantize_indexed};
use crate::music::segment::{PitchFrame, RawNote, SegmentConfig, segment};
use crate::ui::bpm_mode::{MAX_BPM, MIN_BPM};
use crate::ui::state::AppState;
use crate::ui::{GOOD_COLOR, centered_row};

/// Phase of a recording as the Record tab sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordPhase {
    /// No recording runs; the tab waits for a start.
    #[default]
    Idle,
    /// The count-in bar plays; the recording has not begun.
    CountIn,
    /// The recording runs and collects pitch frames.
    Recording,
    /// A stop is sent; the tab waits for the finished take.
    Stopping,
}

/// A finished recording: its frames and the notes quantized from them, editable before export.
#[derive(Debug, Clone, PartialEq)]
pub struct Take {
    /// Pitch frames of the recording.
    frames: Vec<PitchFrame>,
    /// Engine-clock time of beat 0 as recorded.
    origin_s: f64,
    /// Tempo the take was recorded at.
    bpm: f64,
    /// Grid the edited notes are quantized onto.
    grid: Grid,
    /// Short-note threshold used for `raw`.
    merge_note_s: f64,
    /// Notes segmented from `frames` with `SegmentConfig { merge_note_s, ..Default::default() }`.
    raw: Vec<RawNote>,
    /// The raw notes after the user's edits, in time order.
    edited: Vec<RawNote>,
    /// Whole beats beat 0 moves later by.
    shift_beats: u32,
    /// The edited notes quantized onto the grid, each with its index in `edited`.
    rows: Vec<(usize, QuantNote)>,
    /// The notes of `rows`.
    notes: Vec<QuantNote>,
}

impl Take {
    /// Segments `frames` with the default config and quantizes the notes onto `grid`, building the take recorded at `bpm` with beat 0 at `origin_s`.
    pub fn new(frames: Vec<PitchFrame>, origin_s: f64, bpm: f64, grid: Grid) -> Self {
        let merge_note_s = SegmentConfig::default().merge_note_s;
        let raw = segment(
            &frames,
            &SegmentConfig {
                merge_note_s,
                ..SegmentConfig::default()
            },
        );
        let edited = raw.clone();
        let mut take = Self {
            frames,
            origin_s,
            bpm,
            grid,
            merge_note_s,
            raw,
            edited,
            shift_beats: 0,
            rows: Vec::new(),
            notes: Vec::new(),
        };
        take.requantize();
        take
    }

    /// Recomputes `rows` and `notes` from the edited notes, the grid and the shifted origin.
    fn requantize(&mut self) {
        let origin_s = self.origin_s + f64::from(self.shift_beats) * 60.0 / self.bpm;
        self.rows = quantize_indexed(&self.edited, self.bpm, self.grid, origin_s);
        self.notes = self.rows.iter().map(|(_, note)| *note).collect();
    }

    /// Returns the edited notes quantized onto the grid.
    pub fn notes(&self) -> &[QuantNote] {
        &self.notes
    }

    /// Returns the edited notes with the index of each note in the edited notes of the take.
    pub fn rows(&self) -> &[(usize, QuantNote)] {
        &self.rows
    }

    /// Stores `grid` and requantizes; the edits stay.
    pub fn set_grid(&mut self, grid: Grid) {
        self.grid = grid;
        self.requantize();
    }

    /// Removes the edited note at `index`; an out-of-range index does nothing.
    pub fn delete(&mut self, index: usize) {
        if index < self.edited.len() {
            self.edited.remove(index);
            self.requantize();
        }
    }

    /// Merges the edited note at `index` into the note before it, which keeps its pitch and covers both; does nothing when `index` is 0 or out of range.
    pub fn merge_into_previous(&mut self, index: usize) {
        if index > 0 && index < self.edited.len() {
            self.edited[index - 1].end_s = self.edited[index].end_s;
            self.edited.remove(index);
            self.requantize();
        }
    }

    /// Merges the edited note at `index` into the note after it, which keeps its pitch and covers both; does nothing when there is no next note.
    pub fn merge_into_next(&mut self, index: usize) {
        if index + 1 < self.edited.len() {
            self.edited[index + 1].start_s = self.edited[index].start_s;
            self.edited.remove(index);
            self.requantize();
        }
    }

    /// Moves beat 0 later by whole beats so the first note starts at beat 0; does nothing when the take has no notes.
    pub fn start_at_first_note(&mut self) {
        let rows = quantize_indexed(&self.edited, self.bpm, self.grid, self.origin_s);
        let Some((_, first)) = rows.first() else {
            return;
        };
        let shift = (first.start_beats + 1e-9).floor();
        self.shift_beats = if shift >= 1.0 { shift as u32 } else { 0 };
        self.requantize();
    }

    /// Returns how many whole beats beat 0 has moved later by.
    pub fn shift_beats(&self) -> u32 {
        self.shift_beats
    }

    /// Stores `s` as the short-note threshold, re-segments `frames` with it, resets the edits and requantizes; a non-finite or negative `s` is ignored.
    pub fn set_merge_note_s(&mut self, s: f64) {
        if !s.is_finite() || s < 0.0 {
            return;
        }
        self.merge_note_s = s;
        self.raw = segment(
            &self.frames,
            &SegmentConfig {
                merge_note_s: s,
                ..SegmentConfig::default()
            },
        );
        self.reset_edits();
    }

    /// Returns the short-note threshold used for the raw notes.
    pub fn merge_note_s(&self) -> f64 {
        self.merge_note_s
    }

    /// Resets the edits to the raw notes and beat 0 to its recorded position, and requantizes.
    pub fn reset_edits(&mut self) {
        self.edited = self.raw.clone();
        self.shift_beats = 0;
        self.requantize();
    }

    /// Returns whether the take differs from its raw segmentation: the edited notes differ or beat 0 has moved.
    pub fn is_edited(&self) -> bool {
        self.edited != self.raw || self.shift_beats != 0
    }

    /// Returns the tempo the take was recorded at.
    pub fn bpm(&self) -> f64 {
        self.bpm
    }

    /// Returns the engine-clock time of the recording origin in seconds.
    pub fn origin_s(&self) -> f64 {
        self.origin_s
    }

    /// Returns how many pitch frames the take holds.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Returns the edited notes as a Standard MIDI File at the take's tempo.
    pub fn midi_bytes(&self) -> Vec<u8> {
        to_midi_bytes(&self.notes, self.bpm)
    }

    /// Returns the edited notes as CSV text.
    pub fn csv(&self) -> String {
        to_csv(&self.notes)
    }
}

/// A row of the notes table: a note of the take or a rest between notes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Row {
    /// A note of the take.
    Note {
        /// Position of the note in the edited notes of the take.
        index: usize,
        /// The quantized note.
        note: QuantNote,
    },
    /// A rest between notes.
    Rest {
        /// Start position in beats from beat 0.
        start_beats: f64,
        /// Length in beats.
        length_beats: f64,
    },
}

/// Returns the table rows of `rows`: a rest from beat 0 to the first note, the notes, and a rest wherever a note ends before the next starts; gaps under 1e-6 beats are no rest.
pub fn table_rows(rows: &[(usize, QuantNote)]) -> Vec<Row> {
    let mut table = Vec::new();
    let mut cursor_b = 0.0;
    for (index, note) in rows {
        let gap_b = note.start_beats - cursor_b;
        if gap_b >= 1e-6 {
            table.push(Row::Rest {
                start_beats: cursor_b,
                length_beats: gap_b,
            });
        }
        table.push(Row::Note {
            index: *index,
            note: *note,
        });
        cursor_b = note.start_beats + note.length_beats;
    }
    table
}

/// The recording state of the Record tab.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordState {
    /// Phase of the running recording.
    phase: RecordPhase,
    /// Grid the finished take is quantized onto.
    grid: Grid,
    /// Whether recording beats are audible.
    metronome: bool,
    /// Tempo of the running recording, locked at start.
    bpm: f64,
    /// Index and phase of the latest beat of the running recording.
    last_beat: Option<(u32, BeatPhase)>,
    /// Whether a count-in beat arrived for the running recording.
    count_in_seen: bool,
    /// The latest finished take.
    take: Option<Take>,
}

impl Default for RecordState {
    fn default() -> Self {
        Self {
            phase: RecordPhase::Idle,
            grid: Grid::default(),
            metronome: true,
            bpm: 0.0,
            last_beat: None,
            count_in_seen: false,
            take: None,
        }
    }
}

impl RecordState {
    /// Returns the phase of the running recording.
    pub fn phase(&self) -> RecordPhase {
        self.phase
    }

    /// Returns the grid the finished take is quantized onto.
    pub fn grid(&self) -> Grid {
        self.grid
    }

    /// Returns whether recording beats are audible.
    pub fn metronome(&self) -> bool {
        self.metronome
    }

    /// Returns the latest finished take, if there is one.
    pub fn take(&self) -> Option<&Take> {
        self.take.as_ref()
    }

    /// Returns the latest finished take mutably, if there is one.
    pub fn take_mut(&mut self) -> Option<&mut Take> {
        self.take.as_mut()
    }

    /// Returns the index and phase of the latest beat of the running recording.
    pub fn last_beat(&self) -> Option<(u32, BeatPhase)> {
        self.last_beat
    }

    /// Returns whether no recording runs.
    pub fn is_idle(&self) -> bool {
        self.phase == RecordPhase::Idle
    }

    /// Returns the tempo of the running recording, or `None` while idle.
    pub fn recording_bpm(&self) -> Option<f64> {
        if self.is_idle() { None } else { Some(self.bpm) }
    }

    /// Stores `grid` and re-quantizes the finished take onto it, if there is one.
    pub fn set_grid(&mut self, grid: Grid) {
        self.grid = grid;
        if let Some(take) = &mut self.take {
            take.set_grid(grid);
        }
    }

    /// Stores whether recording beats are audible; the setting changes only while idle and is locked during a recording.
    pub fn set_metronome(&mut self, on: bool) {
        if self.is_idle() {
            self.metronome = on;
        }
    }

    /// Locks `bpm`, clears the latest beat, enters the count-in and returns the command that starts the recording; returns `None` and changes nothing unless the tab is idle and `bpm` is finite and positive. The finished take is kept.
    pub fn start(&mut self, bpm: f64) -> Option<EngineCommand> {
        if !self.is_idle() || !bpm.is_finite() || bpm <= 0.0 {
            return None;
        }
        self.bpm = bpm;
        self.last_beat = None;
        self.count_in_seen = false;
        self.phase = RecordPhase::CountIn;
        Some(EngineCommand::StartRecording {
            bpm,
            metronome: self.metronome,
        })
    }

    /// Enters the stopping phase and returns the command that ends the recording; returns `None` while idle or already stopping.
    pub fn stop(&mut self) -> Option<EngineCommand> {
        match self.phase {
            RecordPhase::CountIn | RecordPhase::Recording => {
                self.phase = RecordPhase::Stopping;
                Some(EngineCommand::StopRecording)
            }
            RecordPhase::Idle | RecordPhase::Stopping => None,
        }
    }

    /// Returns to idle without a take when a recording runs, because the engine discards a recording without an event when listening stops or a device changes; the finished take is kept.
    pub fn cancel(&mut self) {
        if !self.is_idle() {
            self.phase = RecordPhase::Idle;
            self.last_beat = None;
            self.count_in_seen = false;
        }
    }

    /// Folds `event` into the state: a beat while a recording runs updates the latest beat and a count-in beat marks the count-in as seen, a start moves the count-in into the recording, a finish stores the take and returns to idle, an error before the first count-in beat aborts the start. Other events change nothing.
    pub fn apply_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::Beat { index, phase, .. } if !self.is_idle() => {
                self.last_beat = Some((*index, *phase));
                if *phase == BeatPhase::CountIn {
                    self.count_in_seen = true;
                }
            }
            EngineEvent::RecordingStarted { .. } if self.phase == RecordPhase::CountIn => {
                self.phase = RecordPhase::Recording;
            }
            EngineEvent::RecordingFinished { frames, origin_s } if !self.is_idle() => {
                self.take = Some(Take::new(frames.clone(), *origin_s, self.bpm, self.grid));
                self.phase = RecordPhase::Idle;
                self.last_beat = None;
                self.count_in_seen = false;
            }
            EngineEvent::Error(_) if self.phase == RecordPhase::CountIn && !self.count_in_seen => {
                self.phase = RecordPhase::Idle;
            }
            _ => {}
        }
    }

    /// Returns the beat line of the tab: empty while idle, "Stopping" while stopping, and the count-in or bar position of the latest beat otherwise.
    pub fn beat_text(&self) -> String {
        match self.phase {
            RecordPhase::Idle => String::new(),
            RecordPhase::Stopping => "Stopping".to_string(),
            RecordPhase::CountIn | RecordPhase::Recording => match self.last_beat {
                Some((index, BeatPhase::CountIn)) => {
                    format!("Count-in {}/{}", index % BEATS_PER_BAR + 1, BEATS_PER_BAR)
                }
                Some((index, BeatPhase::Recording)) => format!(
                    "Bar {} beat {}",
                    index / BEATS_PER_BAR + 1,
                    index % BEATS_PER_BAR + 1
                ),
                Some((_, BeatPhase::Free)) | None => "Count-in".to_string(),
            },
        }
    }
}

/// Returns the label of `grid` in the grid combo box.
fn grid_label(grid: Grid) -> &'static str {
    match grid {
        Grid::Off => "Off",
        Grid::Quarter => "1/4",
        Grid::Eighth => "1/8",
        Grid::Sixteenth => "1/16",
    }
}

/// Returns the summary line of `take`: empty without a take, "No notes in this take" for a take without notes, and the note count and tempo otherwise.
fn take_summary(take: Option<&Take>) -> String {
    match take {
        None => String::new(),
        Some(take) => {
            let count = take.notes().len();
            if count == 0 {
                "No notes in this take".to_string()
            } else if count == 1 {
                format!("1 note at {:.1} BPM", take.bpm())
            } else {
                format!("{count} notes at {:.1} BPM", take.bpm())
            }
        }
    }
}

/// Returns the table row of `note`: the start beat, the length in beats, the note name and the MIDI number.
fn note_row(note: &QuantNote) -> [String; 4] {
    [
        format!("{:.2}", note.start_beats),
        format!("{:.2}", note.length_beats),
        midi_to_name(i32::from(note.midi)),
        note.midi.to_string(),
    ]
}

/// Returns the position in the bar and the phase of the latest beat, or `None` without a beat.
fn lit_beat(last_beat: Option<(u32, BeatPhase)>) -> Option<(u32, BeatPhase)> {
    last_beat.map(|(index, phase)| (index % BEATS_PER_BAR, phase))
}

/// Spacing of the beat light centres in points; also the height of the beat lights row.
const BEAT_LIGHT_SPACING: f32 = 32.0;

/// Side of the square the four beat lights are drawn in, in points.
const BEAT_LIGHTS_SIZE: f32 = 4.0 * BEAT_LIGHT_SPACING;

/// Radius of one beat light in points.
const BEAT_LIGHT_RADIUS: f32 = 10.0;

/// Draws the four beat lights in a row: the circle at the lit position is filled with the warn color for a count-in beat and the good color for a recording beat, the others are faint with a weak stroke.
fn beat_lights(ui: &mut egui::Ui, beat: Option<(u32, BeatPhase)>) {
    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(BEAT_LIGHTS_SIZE, BEAT_LIGHT_SPACING),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    for i in 0..BEATS_PER_BAR {
        let center = egui::pos2(
            rect.left() + (i as f32 + 0.5) * BEAT_LIGHT_SPACING,
            rect.center().y,
        );
        match beat {
            Some((lit, phase)) if lit == i => {
                let color = match phase {
                    BeatPhase::CountIn => ui.visuals().warn_fg_color,
                    BeatPhase::Recording | BeatPhase::Free => GOOD_COLOR,
                };
                painter.circle_filled(center, BEAT_LIGHT_RADIUS, color);
            }
            _ => {
                painter.circle_filled(center, BEAT_LIGHT_RADIUS, ui.visuals().faint_bg_color);
                painter.circle_stroke(
                    center,
                    BEAT_LIGHT_RADIUS,
                    egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
                );
            }
        }
    }
}

/// Shows the Record tab: the headphone hint, the tempo, grid and metronome settings, the Record and Stop buttons, the beat indicator with the beat line, and the summary, notes table and MIDI and CSV export of the finished take; returns the command the user's action sends to the engine.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) -> Option<EngineCommand> {
    let mut command: Option<EngineCommand> = None;
    ui.vertical_centered(|ui| {
        ui.weak("Use headphones, so the metronome does not reach the microphone.");
        ui.add_space(8.0);
        centered_row(ui, "record_settings", |ui| {
            ui.label("Tempo");
            if state.record.is_idle() {
                let mut bpm = state.bpm.record_bpm();
                let response = ui.add(
                    egui::DragValue::new(&mut bpm)
                        .range(MIN_BPM..=MAX_BPM)
                        .speed(0.5)
                        .fixed_decimals(1)
                        .suffix(" BPM"),
                );
                if response.changed() {
                    state.bpm.set_record_bpm(bpm);
                }
            } else if let Some(bpm) = state.record.recording_bpm() {
                ui.label(format!("{bpm:.1} BPM"));
            }
            ui.add_space(16.0);
            ui.label("Grid");
            let mut grid = state.record.grid();
            egui::ComboBox::from_id_salt("grid")
                .selected_text(grid_label(grid))
                .show_ui(ui, |ui| {
                    for candidate in [Grid::Off, Grid::Quarter, Grid::Eighth, Grid::Sixteenth] {
                        ui.selectable_value(&mut grid, candidate, grid_label(candidate));
                    }
                });
            if grid != state.record.grid() {
                state.record.set_grid(grid);
            }
            ui.add_space(16.0);
            let mut metronome = state.record.metronome();
            ui.add_enabled_ui(state.record.is_idle(), |ui| {
                if ui.checkbox(&mut metronome, "Metronome").changed() {
                    state.record.set_metronome(metronome);
                }
            });
        });
        ui.add_space(8.0);
        centered_row(ui, "record_transport", |ui| {
            if ui
                .add_enabled(
                    state.record.is_idle(),
                    egui::Button::new("Record").min_size(egui::vec2(100.0, 36.0)),
                )
                .clicked()
            {
                command = state.record.start(state.bpm.record_bpm());
            }
            if ui
                .add_enabled(
                    matches!(
                        state.record.phase(),
                        RecordPhase::CountIn | RecordPhase::Recording
                    ),
                    egui::Button::new("Stop").min_size(egui::vec2(100.0, 36.0)),
                )
                .clicked()
            {
                command = state.record.stop();
            }
        });
        ui.add_space(8.0);
        let beat = if state.record.is_idle() {
            None
        } else {
            lit_beat(state.record.last_beat())
        };
        beat_lights(ui, beat);
        let beat_text = state.record.beat_text();
        if !beat_text.is_empty() {
            ui.add_space(4.0);
            ui.strong(beat_text);
        }
        if !state.record.is_idle() {
            ui.add_space(8.0);
            crate::ui::live::note_readout(ui, state.live_note(), 48.0);
        }
        let mut export: Option<(&'static str, &'static str, &'static str, Vec<u8>)> = None;
        if let Some(take) = state.record.take() {
            ui.add_space(8.0);
            ui.label(take_summary(Some(take)));
            let notes = take.notes();
            if !notes.is_empty() {
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        egui::Grid::new("notes")
                            .striped(true)
                            .num_columns(4)
                            .show(ui, |ui| {
                                for header in ["Start beat", "Length", "Note", "MIDI"] {
                                    ui.strong(header);
                                }
                                ui.end_row();
                                for note in notes {
                                    for cell in note_row(note) {
                                        ui.label(cell);
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.add_space(8.0);
                centered_row(ui, "record_export", |ui| {
                    if ui.button("Save MIDI").clicked() {
                        export = Some(("melody.mid", "MIDI", "mid", take.midi_bytes()));
                    }
                    if ui.button("Save CSV").clicked() {
                        export = Some(("melody.csv", "CSV", "csv", take.csv().into_bytes()));
                    }
                });
            }
        }
        if let Some((default_name, filter_name, extension, bytes)) = export {
            save(state, default_name, filter_name, extension, bytes);
        }
    });
    command
}

/// Opens a save dialog for `default_name` with the `filter_name` and `extension` filter and writes `bytes` to the chosen path; a cancelled dialog does nothing, a failed write is reported as an error in the status bar.
fn save(
    state: &mut AppState,
    default_name: &str,
    filter_name: &str,
    extension: &str,
    bytes: Vec<u8>,
) {
    let Some(path) = rfd::FileDialog::new()
        .set_file_name(default_name)
        .add_filter(filter_name, &[extension])
        .save_file()
    else {
        return;
    };
    match std::fs::write(&path, &bytes) {
        Ok(()) => log::info!("saved {}", path.display()),
        Err(err) => state.report_error(format!("could not save {}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::assert_close;
    use crate::music::quantize::quantize;

    /// Returns a beat event with `index` and `phase`, accented every fourth beat, at time 0.
    fn beat(index: u32, phase: BeatPhase) -> EngineEvent {
        EngineEvent::Beat {
            index,
            accent: index.is_multiple_of(4),
            phase,
            time_s: 0.0,
        }
    }

    /// Returns voiced frames every 0.01 s from `start_s` for `len_s` seconds at `hz` Hz with confidence 0.95.
    fn tone_frames(start_s: f64, len_s: f64, hz: f32) -> Vec<PitchFrame> {
        let count = (len_s / 0.01).round() as usize;
        (0..count)
            .map(|i| PitchFrame {
                time_s: start_s + i as f64 * 0.01,
                freq_hz: Some(hz),
                confidence: 0.95,
            })
            .collect()
    }

    /// Runs a full recording at 120 BPM with `frames` and origin 1.0 and returns the state and the frames.
    fn recorded_state(frames: Vec<PitchFrame>) -> (RecordState, Vec<PitchFrame>) {
        let mut state = RecordState::default();
        state.start(120.0);
        for i in 0..4 {
            state.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        state.apply_event(&beat(0, BeatPhase::Recording));
        state.stop();
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: frames.clone(),
            origin_s: 1.0,
        });
        (state, frames)
    }

    #[test]
    fn the_default_state_is_idle() {
        let state = RecordState::default();
        assert_eq!(state.phase(), RecordPhase::Idle);
        assert!(state.is_idle());
        assert_eq!(state.grid(), Grid::Sixteenth);
        assert!(state.metronome());
        assert!(state.take().is_none());
        assert_eq!(state.last_beat(), None);
        assert_eq!(state.beat_text(), "");
        assert_eq!(state.recording_bpm(), None);
    }

    #[test]
    fn start_locks_the_tempo_and_sends_the_command() {
        let mut state = RecordState::default();
        assert_eq!(
            state.start(100.0),
            Some(EngineCommand::StartRecording {
                bpm: 100.0,
                metronome: true,
            })
        );
        assert_eq!(state.phase(), RecordPhase::CountIn);
        assert_eq!(state.recording_bpm(), Some(100.0));
        assert_eq!(state.beat_text(), "Count-in");
        assert_eq!(state.start(120.0), None, "a second start is refused");
        assert_eq!(state.recording_bpm(), Some(100.0));
    }

    #[test]
    fn an_invalid_tempo_is_refused() {
        let mut state = RecordState::default();
        for bpm in [f64::NAN, 0.0, -120.0, f64::INFINITY] {
            assert_eq!(state.start(bpm), None);
        }
        assert!(state.is_idle());
        assert_eq!(state.recording_bpm(), None);
    }

    #[test]
    fn count_in_beats_show_the_count_in_position() {
        let mut state = RecordState::default();
        state.start(120.0);
        for (i, text) in [
            "Count-in 1/4",
            "Count-in 2/4",
            "Count-in 3/4",
            "Count-in 4/4",
        ]
        .iter()
        .enumerate()
        {
            state.apply_event(&beat(i as u32, BeatPhase::CountIn));
            assert_eq!(state.beat_text(), *text);
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 2.0 });
        assert_eq!(state.phase(), RecordPhase::Recording);
    }

    #[test]
    fn recording_beats_show_bar_and_beat() {
        let mut state = RecordState::default();
        state.start(120.0);
        for i in 0..4 {
            state.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 2.0 });
        for (index, text) in [
            (0, "Bar 1 beat 1"),
            (3, "Bar 1 beat 4"),
            (4, "Bar 2 beat 1"),
            (9, "Bar 3 beat 2"),
        ] {
            state.apply_event(&beat(index, BeatPhase::Recording));
            assert_eq!(state.beat_text(), text);
        }
        state.apply_event(&beat(12, BeatPhase::Free));
        assert_eq!(state.beat_text(), "Count-in");
    }

    #[test]
    fn stop_sends_the_stop_command() {
        let mut state = RecordState::default();
        assert_eq!(state.stop(), None, "stop while idle does nothing");
        state.start(120.0);
        for i in 0..4 {
            state.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 2.0 });
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
        assert_eq!(state.phase(), RecordPhase::Stopping);
        assert_eq!(state.beat_text(), "Stopping");
        assert_eq!(state.stop(), None, "a second stop does nothing");
    }

    #[test]
    fn a_full_recording_produces_a_take() {
        let frames = tone_frames(1.0, 0.6, 440.0);
        let (state, frames) = recorded_state(frames);
        assert!(state.is_idle());
        let take = state.take().expect("a take");
        assert_eq!(take.bpm(), 120.0);
        assert_eq!(take.origin_s(), 1.0);
        assert_eq!(take.frame_count(), 60);
        let expected = quantize(
            &segment(&frames, &SegmentConfig::default()),
            120.0,
            Grid::Sixteenth,
            1.0,
        );
        assert_eq!(take.notes(), expected.as_slice());
        assert!(!take.notes().is_empty());
        assert!(take.notes().iter().all(|note| note.midi == 69));
    }

    #[test]
    fn set_grid_requantizes_the_take() {
        let frames = tone_frames(1.0, 0.6, 440.0);
        let (mut state, frames) = recorded_state(frames);
        state.set_grid(Grid::Quarter);
        assert_eq!(state.grid(), Grid::Quarter);
        let take = state.take().expect("a take");
        let expected = quantize(
            &segment(&frames, &SegmentConfig::default()),
            120.0,
            Grid::Quarter,
            1.0,
        );
        assert_eq!(take.notes(), expected.as_slice());
    }

    #[test]
    fn the_take_exports_midi_and_csv() {
        let (state, _) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        let take = state.take().expect("a take");
        assert_eq!(take.midi_bytes(), to_midi_bytes(take.notes(), 120.0));
        assert_eq!(take.csv(), to_csv(take.notes()));
    }

    #[test]
    fn stopping_during_the_count_in_gives_an_empty_take() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&beat(0, BeatPhase::CountIn));
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: Vec::new(),
            origin_s: 0.0,
        });
        assert!(state.is_idle());
        let take = state.take().expect("a take");
        assert!(take.notes().is_empty());
        assert_eq!(take.frame_count(), 0);
        assert_eq!(take.origin_s(), 0.0);
    }

    #[test]
    fn an_error_before_the_first_count_in_beat_aborts_the_start() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&EngineEvent::Error("device busy".to_string()));
        assert!(state.is_idle());

        state.start(120.0);
        state.apply_event(&beat(0, BeatPhase::CountIn));
        state.apply_event(&EngineEvent::Error("device busy".to_string()));
        assert_eq!(
            state.phase(),
            RecordPhase::CountIn,
            "an error after a count-in beat changes nothing"
        );
    }

    #[test]
    fn cancel_returns_to_idle_and_keeps_the_take() {
        let (mut state, frames) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        let take_before = state.take().cloned();
        state.start(90.0);
        state.apply_event(&beat(0, BeatPhase::CountIn));
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 5.0 });
        state.cancel();
        assert!(state.is_idle());
        assert_eq!(state.last_beat(), None);
        assert_eq!(state.take(), take_before.as_ref());
        state.apply_event(&EngineEvent::RecordingFinished {
            frames,
            origin_s: 5.0,
        });
        assert_eq!(
            state.take(),
            take_before.as_ref(),
            "a finish while idle is ignored"
        );
    }

    #[test]
    fn the_metronome_setting_is_locked_during_a_recording() {
        let mut state = RecordState::default();
        state.set_metronome(false);
        assert!(!state.metronome());
        assert_eq!(
            state.start(100.0),
            Some(EngineCommand::StartRecording {
                bpm: 100.0,
                metronome: false,
            })
        );
        state.set_metronome(true);
        assert!(
            !state.metronome(),
            "the setting is locked during the count-in"
        );
    }

    #[test]
    fn events_while_idle_are_ignored() {
        let mut state = RecordState::default();
        state.apply_event(&beat(2, BeatPhase::Recording));
        state.apply_event(&beat(0, BeatPhase::CountIn));
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: Vec::new(),
            origin_s: 0.0,
        });
        state.apply_event(&EngineEvent::Error("x".to_string()));
        assert_eq!(state.last_beat(), None);
        assert!(state.is_idle());
        assert!(state.take().is_none());
        assert_eq!(state.beat_text(), "");
    }

    #[test]
    fn grid_labels_name_the_grids() {
        assert_eq!(grid_label(Grid::Off), "Off");
        assert_eq!(grid_label(Grid::Quarter), "1/4");
        assert_eq!(grid_label(Grid::Eighth), "1/8");
        assert_eq!(grid_label(Grid::Sixteenth), "1/16");
    }

    #[test]
    fn take_summary_is_empty_without_a_take() {
        assert_eq!(take_summary(None), "");
    }

    #[test]
    fn take_summary_of_an_empty_take_says_so() {
        let take = Take::new(Vec::new(), 0.0, 120.0, Grid::Sixteenth);
        assert_eq!(take_summary(Some(&take)), "No notes in this take");
    }

    #[test]
    fn take_summary_singles_out_one_note() {
        let (state, _) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        let take = state.take().expect("a take");
        assert_eq!(take_summary(Some(take)), "1 note at 120.0 BPM");
    }

    #[test]
    fn take_summary_counts_several_notes() {
        let mut frames = tone_frames(1.0, 0.5, 440.0);
        frames.extend(tone_frames(2.0, 0.5, 523.25));
        let take = Take::new(frames, 1.0, 120.0, Grid::Sixteenth);
        let summary = take_summary(Some(&take));
        assert!(summary.starts_with("2 notes at 120.0 BPM"), "{summary}");
    }

    #[test]
    fn note_row_formats_start_length_name_and_midi() {
        let note = QuantNote {
            start_beats: 1.5,
            length_beats: 0.25,
            midi: 69,
        };
        assert_eq!(note_row(&note), ["1.50", "0.25", "A4", "69"]);
    }

    #[test]
    fn lit_beat_keeps_the_position_in_the_bar() {
        assert_eq!(lit_beat(None), None);
        assert_eq!(
            lit_beat(Some((5, BeatPhase::Recording))),
            Some((1, BeatPhase::Recording))
        );
        assert_eq!(
            lit_beat(Some((3, BeatPhase::CountIn))),
            Some((3, BeatPhase::CountIn))
        );
    }

    /// Builds a take of three tones at 1.0, 2.0 and 3.0 s (A4, C5, E5) recorded at 120 BPM with beat 0 at 1.0 s on the sixteenth grid; the notes sit at beats 0, 2 and 4.
    fn three_note_take() -> Take {
        let mut frames = tone_frames(1.0, 0.5, 440.0);
        frames.extend(tone_frames(2.0, 0.5, 523.25));
        frames.extend(tone_frames(3.0, 0.5, 659.26));
        Take::new(frames, 1.0, 120.0, Grid::Sixteenth)
    }

    #[test]
    fn a_three_note_take_has_three_indexed_notes() {
        let take = three_note_take();
        assert_eq!(take.notes().len(), 3);
        assert_eq!(take.notes()[0].midi, 69);
        assert_eq!(take.notes()[1].midi, 72);
        assert_eq!(take.notes()[2].midi, 76);
        assert!(!take.is_edited());
        let indices: Vec<usize> = take.rows().iter().map(|(index, _)| *index).collect();
        assert_eq!(indices, [0, 1, 2]);
    }

    #[test]
    fn deleting_a_note_removes_it_until_the_edits_are_reset() {
        let mut take = three_note_take();
        take.delete(1);
        let midis: Vec<u8> = take.notes().iter().map(|note| note.midi).collect();
        assert_eq!(midis, [69, 76]);
        assert!(take.is_edited());
        assert_eq!(take.notes().len(), 2);
        assert_eq!(take.midi_bytes(), to_midi_bytes(take.notes(), 120.0));
        take.reset_edits();
        assert_eq!(take.notes().len(), 3);
        assert!(!take.is_edited());
    }

    #[test]
    fn merging_into_the_previous_note_extends_it_over_both() {
        let mut take = three_note_take();
        take.merge_into_previous(1);
        assert_eq!(take.notes().len(), 2);
        let first = take.notes()[0];
        assert_eq!(first.midi, 69);
        assert_close(first.start_beats, 0.0, 1e-9);
        assert_close(first.start_beats + first.length_beats, 3.0, 0.26);
    }

    #[test]
    fn merging_into_the_next_note_extends_it_back_over_both() {
        let mut take = three_note_take();
        take.merge_into_next(1);
        assert_eq!(take.notes().len(), 2);
        let second = take.notes()[1];
        assert_eq!(second.midi, 76);
        assert_close(second.start_beats, 2.0, 0.26);
    }

    #[test]
    fn merging_at_the_edges_and_deleting_out_of_range_change_nothing() {
        let mut take = three_note_take();
        take.merge_into_previous(0);
        take.merge_into_next(2);
        take.delete(99);
        assert_eq!(take.notes().len(), 3);
        assert!(!take.is_edited());
    }

    #[test]
    fn start_at_first_note_moves_beat_zero_to_the_first_note() {
        let mut frames = tone_frames(3.0, 0.5, 440.0);
        frames.extend(tone_frames(4.0, 0.5, 523.25));
        let mut take = Take::new(frames, 1.0, 120.0, Grid::Sixteenth);
        take.start_at_first_note();
        assert_eq!(take.shift_beats(), 4);
        assert_close(take.notes()[0].start_beats, 0.0, 1e-9);
        assert_close(take.notes()[1].start_beats, 2.0, 1e-9);
        assert!(take.is_edited());

        let mut take = three_note_take();
        take.start_at_first_note();
        assert_eq!(take.shift_beats(), 0, "a first note at beat 0 keeps beat 0");
    }

    #[test]
    fn edits_survive_a_grid_change() {
        let mut take = three_note_take();
        take.delete(1);
        take.set_grid(Grid::Quarter);
        assert_eq!(take.notes().len(), 2);
        assert!(take.is_edited());
    }

    #[test]
    fn set_merge_note_s_resegments_and_resets_the_edits() {
        let mut frames = tone_frames(1.0, 0.08, 466.16);
        frames.extend(tone_frames(1.08, 0.5, 440.0));
        let mut take = Take::new(frames, 1.0, 120.0, Grid::Sixteenth);
        assert_eq!(take.notes().len(), 1, "the short onset merges by default");
        take.delete(0);
        assert!(take.is_edited());
        assert!(take.notes().is_empty());
        take.set_merge_note_s(0.0);
        assert_eq!(take.notes().len(), 2);
        assert!(!take.is_edited());
        assert_eq!(take.merge_note_s(), 0.0);
        take.set_merge_note_s(f64::NAN);
        take.set_merge_note_s(-1.0);
        assert_eq!(take.merge_note_s(), 0.0, "invalid thresholds are ignored");
        assert_eq!(take.notes().len(), 2);
    }

    #[test]
    fn set_merge_note_s_also_resets_beat_zero() {
        let mut take = Take::new(tone_frames(3.0, 0.5, 440.0), 1.0, 120.0, Grid::Sixteenth);
        take.start_at_first_note();
        assert_eq!(take.shift_beats(), 4);
        take.set_merge_note_s(0.05);
        assert_eq!(take.shift_beats(), 0);
        assert!(!take.is_edited());
        assert_close(take.notes()[0].start_beats, 4.0, 1e-9);
    }

    #[test]
    fn table_rows_lists_the_rests_around_the_notes() {
        let note_a = QuantNote {
            start_beats: 2.0,
            length_beats: 1.0,
            midi: 69,
        };
        let note_b = QuantNote {
            start_beats: 4.0,
            length_beats: 0.5,
            midi: 72,
        };
        let table = table_rows(&[(0, note_a), (1, note_b)]);
        assert_eq!(
            table,
            vec![
                Row::Rest {
                    start_beats: 0.0,
                    length_beats: 2.0,
                },
                Row::Note {
                    index: 0,
                    note: note_a,
                },
                Row::Rest {
                    start_beats: 3.0,
                    length_beats: 1.0,
                },
                Row::Note {
                    index: 1,
                    note: note_b,
                },
            ]
        );

        let touching = [
            (
                0,
                QuantNote {
                    start_beats: 0.0,
                    length_beats: 1.0,
                    midi: 69,
                },
            ),
            (
                1,
                QuantNote {
                    start_beats: 1.0,
                    length_beats: 1.0,
                    midi: 72,
                },
            ),
        ];
        let table = table_rows(&touching);
        assert_eq!(table.len(), 2, "touching notes have no rest between them");
        assert!(table.iter().all(|row| matches!(row, Row::Note { .. })));

        assert!(table_rows(&[]).is_empty());
    }

    #[test]
    fn take_mut_edits_the_stored_take() {
        let (mut state, _) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        state.take_mut().expect("a take").delete(0);
        assert!(state.take().expect("a take").notes().is_empty());
    }
}
