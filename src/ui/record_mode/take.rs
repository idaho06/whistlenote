//! The finished or preview take of a recording, editable before export, and the rows of its notes table.

use crate::export::csv::to_csv;
use crate::export::midi::to_midi_bytes;
use crate::music::quantize::{Grid, QuantNote, quantize_indexed};
use crate::music::segment::{PitchFrame, RawNote, SegmentConfig, segment};

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

#[cfg(test)]
mod tests {
    use super::super::test_support::{three_note_take, tone_frames};
    use super::*;
    use crate::music::assert_close;

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
}
