//! The view of the Record tab: the tempo, grid and metronome controls, the Record and Stop buttons, the beat lights with the beat line, the live note and the preview of the notes recorded so far while a recording runs, and the finished take with its summary, the edit row, the editable notes table and the MIDI and CSV export.

use eframe::egui;

use crate::engine::{BeatPhase, EngineCommand};
use crate::metronome::BEATS_PER_BAR;
use crate::music::note::midi_to_name;
use crate::music::quantize::{Grid, QuantNote};
use crate::ui::bpm_mode::{MAX_BPM, MIN_BPM};
use crate::ui::state::AppState;
use crate::ui::{GOOD_COLOR, centered_row};

use super::{RecordPhase, Row, Take, table_rows};

/// An edit the user asks for in the notes table of the Record tab.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Edit {
    /// Deletes the edited note at the index.
    Delete(usize),
    /// Merges the edited note at the index into the note before it.
    MergeIntoPrevious(usize),
    /// Merges the edited note at the index into the note after it.
    MergeIntoNext(usize),
    /// Moves beat 0 later by whole beats so the first note starts on beat 0.
    StartAtFirstNote,
    /// Resets the edits to the raw segmentation.
    Reset,
    /// Sets the short-note threshold in seconds.
    MergeNoteS(f64),
}

/// Applies `edit` to `take`.
fn apply_edit(take: &mut Take, edit: Edit) {
    match edit {
        Edit::Delete(index) => take.delete(index),
        Edit::MergeIntoPrevious(index) => take.merge_into_previous(index),
        Edit::MergeIntoNext(index) => take.merge_into_next(index),
        Edit::StartAtFirstNote => take.start_at_first_note(),
        Edit::Reset => take.reset_edits(),
        Edit::MergeNoteS(s) => take.set_merge_note_s(s),
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

/// Returns `count` followed by the singular noun for 1 and the plural noun otherwise: "1 note", "3 notes".
fn count_text(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// Returns the summary line of `take`: empty without a take, "No notes in this take" for a take without notes, and the note count and tempo otherwise, followed by " (edited)" when the take is edited.
fn take_summary(take: Option<&Take>) -> String {
    match take {
        None => String::new(),
        Some(take) => {
            let count = take.notes().len();
            if count == 0 {
                "No notes in this take".to_string()
            } else {
                let mut summary = format!(
                    "{} at {:.1} BPM",
                    count_text(count, "note", "notes"),
                    take.bpm()
                );
                if take.is_edited() {
                    summary.push_str(" (edited)");
                }
                summary
            }
        }
    }
}

/// Returns the note-count text of the preview of the running recording: "No notes yet", "1 note so far" or "{count} notes so far".
fn preview_summary(count: usize) -> String {
    match count {
        0 => "No notes yet".to_string(),
        count => format!("{} so far", count_text(count, "note", "notes")),
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

/// Returns the table row of a rest from `start_beats` for `length_beats`: the start beat, the length in beats, "rest" and "-".
fn rest_row(start_beats: f64, length_beats: f64) -> [String; 4] {
    [
        format!("{start_beats:.2}"),
        format!("{length_beats:.2}"),
        "rest".to_string(),
        "-".to_string(),
    ]
}

/// Draws the notes table of `take` — the rest rows and the note rows — and returns the edit a per-note button asks for; an editable table has an "Edit" column with delete and merge buttons, a non-editable table has four columns, sticks to the bottom so the newest rows stay visible and returns `None`.
fn notes_table(ui: &mut egui::Ui, take: &Take, editable: bool) -> Option<Edit> {
    let mut edit: Option<Edit> = None;
    let table = table_rows(take.rows());
    let first_note = table.iter().position(|row| matches!(row, Row::Note { .. }));
    let last_note = table
        .iter()
        .rposition(|row| matches!(row, Row::Note { .. }));
    let (grid_salt, scroll_salt, columns) = if editable {
        ("notes", "notes_scroll", 5)
    } else {
        ("preview_notes", "preview_notes_scroll", 4)
    };
    let headers = ["Start beat", "Length", "Note", "MIDI", "Edit"];
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .id_salt(scroll_salt)
        .stick_to_bottom(!editable)
        .show(ui, |ui| {
            egui::Grid::new(grid_salt)
                .striped(true)
                .num_columns(columns)
                .show(ui, |ui| {
                    for header in headers[..columns].iter().copied() {
                        ui.strong(header);
                    }
                    ui.end_row();
                    for (pos, row) in table.iter().enumerate() {
                        match row {
                            Row::Rest {
                                start_beats,
                                length_beats,
                            } => {
                                for cell in rest_row(*start_beats, *length_beats) {
                                    ui.weak(cell);
                                }
                                if editable {
                                    ui.label("");
                                }
                                ui.end_row();
                            }
                            Row::Note { index, note } => {
                                for cell in note_row(note) {
                                    ui.label(cell);
                                }
                                if editable {
                                    ui.horizontal(|ui| {
                                        if ui.small_button("Delete").clicked() {
                                            edit = Some(Edit::Delete(*index));
                                        }
                                        for (label, hover, merge, enabled) in [
                                            (
                                                "Merge ←",
                                                "Merge into the previous note, which keeps its pitch",
                                                Edit::MergeIntoPrevious as fn(usize) -> Edit,
                                                Some(pos) != first_note,
                                            ),
                                            (
                                                "Merge →",
                                                "Merge into the next note, which keeps its pitch",
                                                Edit::MergeIntoNext as fn(usize) -> Edit,
                                                Some(pos) != last_note,
                                            ),
                                        ] {
                                            if ui
                                                .add_enabled(
                                                    enabled,
                                                    egui::Button::new(label).small(),
                                                )
                                                .on_hover_text(hover)
                                                .clicked()
                                            {
                                                edit = Some(merge(*index));
                                            }
                                        }
                                    });
                                }
                                ui.end_row();
                            }
                        }
                    }
                });
        });
    edit
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

/// Shows the Record tab: the headphone hint, the tempo, grid and metronome settings, the Record and Stop buttons, the beat indicator with the beat line, the live note, the preview of the notes recorded so far while a recording runs, and the finished take while idle with its summary, the edit row that starts at the first note, resets the edits and sets the short-note threshold, the notes table with rest rows and per-note delete and merge buttons, and the MIDI and CSV export; returns the command the user's action sends to the engine.
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
        let mut edit: Option<Edit> = None;
        if !state.record.is_idle() {
            if let Some(preview) = state.record.preview() {
                ui.add_space(8.0);
                ui.label(format!(
                    "Recording: {}",
                    preview_summary(preview.notes().len())
                ));
                if !preview.notes().is_empty() {
                    ui.add_space(4.0);
                    notes_table(ui, preview, false);
                }
            }
        } else if let Some(take) = state.record.take() {
            ui.add_space(8.0);
            ui.label(take_summary(Some(take)));
            let has_notes = !take.notes().is_empty();
            ui.add_space(4.0);
            centered_row(ui, "record_edit", |ui| {
                if ui
                    .add_enabled(has_notes, egui::Button::new("Start at first note").small())
                    .on_hover_text(
                        "Move beat 0 later by whole beats so the first note starts on beat 1",
                    )
                    .clicked()
                {
                    edit = Some(Edit::StartAtFirstNote);
                }
                if ui
                    .add_enabled(take.is_edited(), egui::Button::new("Reset edits").small())
                    .clicked()
                {
                    edit = Some(Edit::Reset);
                }
                ui.label("Merge notes shorter than");
                let mut ms = take.merge_note_s() * 1000.0;
                let response = ui
                    .add(
                        egui::DragValue::new(&mut ms)
                            .range(0.0..=250.0)
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(" ms"),
                    )
                    .on_hover_text(
                        "Shorter notes that touch a neighbour merge into it; changing this resets the edits",
                    );
                if response.changed() {
                    edit = Some(Edit::MergeNoteS(ms / 1000.0));
                }
            });
            let shift = take.shift_beats();
            if shift > 0 {
                ui.add_space(2.0);
                ui.weak(format!(
                    "Beat 0 moved {} later",
                    count_text(shift as usize, "beat", "beats")
                ));
            }
            if has_notes {
                ui.add_space(4.0);
                if let Some(table_edit) = notes_table(ui, take, true) {
                    edit = Some(table_edit);
                }
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
        if let (Some(edit), Some(take)) = (edit, state.record.take_mut()) {
            apply_edit(take, edit);
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
    use super::super::test_support::{recorded_state, three_note_take, tone_frames};
    use super::*;
    use crate::music::assert_close;

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
    fn take_summary_marks_an_edited_take() {
        let mut take = three_note_take();
        take.delete(1);
        assert_eq!(take_summary(Some(&take)), "2 notes at 120.0 BPM (edited)");
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
    fn rest_row_formats_a_rest() {
        assert_eq!(rest_row(0.0, 2.0), ["0.00", "2.00", "rest", "-"]);
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

    #[test]
    fn apply_edit_deletes_merges_and_resets() {
        let mut take = three_note_take();
        apply_edit(&mut take, Edit::Delete(1));
        assert_eq!(take.notes().len(), 2);
        let midis: Vec<u8> = take.notes().iter().map(|note| note.midi).collect();
        assert_eq!(midis, [69, 76]);

        let mut take = three_note_take();
        apply_edit(&mut take, Edit::MergeIntoPrevious(1));
        assert_eq!(take.notes().len(), 2);
        let midis: Vec<u8> = take.notes().iter().map(|note| note.midi).collect();
        assert_eq!(midis, [69, 76]);
        let first = take.notes()[0];
        assert_eq!(first.midi, 69);
        assert!(
            first.length_beats > 2.0,
            "the first note covers both notes: {} beats",
            first.length_beats
        );

        let mut take = three_note_take();
        apply_edit(&mut take, Edit::MergeIntoNext(1));
        assert_eq!(take.notes().len(), 2);
        let midis: Vec<u8> = take.notes().iter().map(|note| note.midi).collect();
        assert_eq!(midis, [69, 76]);
        let second = take.notes()[1];
        assert_eq!(second.midi, 76);
        assert_close(second.start_beats, 2.0, 0.26);

        let mut take = three_note_take();
        apply_edit(&mut take, Edit::Delete(1));
        assert_eq!(take.notes().len(), 2);
        apply_edit(&mut take, Edit::Reset);
        assert_eq!(take.notes().len(), 3);
        assert!(!take.is_edited());
    }

    #[test]
    fn apply_edit_sets_the_short_note_threshold() {
        let mut take = three_note_take();
        apply_edit(&mut take, Edit::MergeNoteS(0.0));
        assert_eq!(take.merge_note_s(), 0.0);
    }

    #[test]
    fn apply_edit_start_at_first_note_moves_beat_zero() {
        let frames = tone_frames(3.0, 0.5, 440.0);
        let mut take = Take::new(frames, 1.0, 120.0, Grid::Sixteenth);
        apply_edit(&mut take, Edit::StartAtFirstNote);
        assert_eq!(take.shift_beats(), 4);
    }

    #[test]
    fn preview_summary_counts_the_notes() {
        assert_eq!(preview_summary(0), "No notes yet");
        assert_eq!(preview_summary(1), "1 note so far");
        assert_eq!(preview_summary(3), "3 notes so far");
    }
}
