//! The finished or preview take of a recording, editable before export, and the rows of its notes table.

use crate::export::csv::to_csv_at;
use crate::export::midi::{MidiOptions, to_midi_bytes_with};
use crate::music::quantize::{Grid, MIN_LENGTH_B, QuantNote, quantize_indexed, step_beats};
use crate::music::segment::{self, PitchFrame, RawNote, SegmentConfig};

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
    /// Config the raw notes are segmented with.
    segment: SegmentConfig,
    /// MIDI export options of the take.
    midi: MidiOptions,
    /// Notes segmented from `frames` with `segment`.
    raw: Vec<RawNote>,
    /// The raw notes after the user's edits, in time order.
    edited: Vec<RawNote>,
    /// Whole beats beat 0 has moved later by; `requantize` shifts the rows in beat space by this amount.
    shift_beats: u32,
    /// The edited notes quantized onto the grid and shifted in beat space by `shift_beats`, each with its index in `edited`.
    rows: Vec<(usize, QuantNote)>,
    /// The notes of `rows`.
    notes: Vec<QuantNote>,
}

impl Take {
    /// Segments `frames` with `segment` and quantizes the notes onto `grid`, building the take recorded at `bpm` with beat 0 at `origin_s` and exported with `midi`.
    pub fn with_config(
        frames: Vec<PitchFrame>,
        origin_s: f64,
        bpm: f64,
        grid: Grid,
        segment: SegmentConfig,
        midi: MidiOptions,
    ) -> Self {
        let raw = segment::segment(&frames, &segment);
        let edited = raw.clone();
        let mut take = Self {
            frames,
            origin_s,
            bpm,
            grid,
            segment,
            midi,
            raw,
            edited,
            shift_beats: 0,
            rows: Vec::new(),
            notes: Vec::new(),
        };
        take.requantize();
        take
    }

    /// Segments `frames` with the default segmentation settings and quantizes the notes onto `grid`, building the take recorded at `bpm` with beat 0 at `origin_s`; the take uses the default export settings.
    pub fn new(frames: Vec<PitchFrame>, origin_s: f64, bpm: f64, grid: Grid) -> Self {
        Self::with_config(
            frames,
            origin_s,
            bpm,
            grid,
            SegmentConfig::default(),
            MidiOptions::default(),
        )
    }

    /// Recomputes `rows` and `notes` from the edited notes, the grid and the recorded `origin_s`: the notes are quantized with the unshifted `origin_s`, then shifted in beat space by `shift_beats`; a row whose start falls below 0 is clamped to beat 0 and shortened by the same amount, and a row left with a length of at most the beat tolerance is dropped.
    fn requantize(&mut self) {
        let mut rows = quantize_indexed(&self.edited, self.bpm, self.grid, self.origin_s);
        let shift = f64::from(self.shift_beats);
        for (_, note) in &mut rows {
            note.start_beats -= shift;
            if note.start_beats < 0.0 {
                note.length_beats += note.start_beats;
                note.start_beats = 0.0;
            }
        }
        rows.retain(|(_, note)| note.length_beats > MIN_LENGTH_B);
        self.rows = rows;
        self.notes = self.rows.iter().map(|(_, note)| *note).collect();
    }

    /// Returns the position in `rows` of the row whose edited index is `index`, if there is one.
    fn row_position(&self, index: usize) -> Option<usize> {
        self.rows
            .iter()
            .position(|(row_index, _)| *row_index == index)
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

    /// Removes the row of `rows` whose edited index is `index`, together with the edited notes hidden between that row and the previous row; an `index` that is not the index of a row of `rows` does nothing.
    pub fn delete(&mut self, index: usize) {
        let Some(row) = self.row_position(index) else {
            return;
        };
        let from = if row == 0 {
            0
        } else {
            self.rows[row - 1].0 + 1
        };
        self.edited.drain(from..=index);
        self.requantize();
    }

    /// Merges the row of `rows` whose edited index is `index` into the previous row, which keeps its pitch and covers both, and removes the edited notes hidden between the two rows; does nothing when that row is the first row or `index` is not the index of a row of `rows`.
    pub fn merge_into_previous(&mut self, index: usize) {
        let Some(row) = self.row_position(index) else {
            return;
        };
        if row == 0 {
            return;
        }
        let previous = self.rows[row - 1].0;
        self.edited[previous].end_s = self.edited[index].end_s;
        self.edited.drain(previous + 1..=index);
        self.requantize();
    }

    /// Merges the row of `rows` whose edited index is `index` into the next row, which keeps its pitch and covers both, and removes the edited notes hidden between the two rows; does nothing when that row is the last row or `index` is not the index of a row of `rows`.
    pub fn merge_into_next(&mut self, index: usize) {
        let Some(row) = self.row_position(index) else {
            return;
        };
        let Some(next) = self.rows.get(row + 1).map(|(next, _)| *next) else {
            return;
        };
        self.edited[next].start_s = self.edited[index].start_s;
        self.edited.drain(index..next);
        self.requantize();
    }

    /// Moves beat 0 later by whole beats so the first row starts at beat 0, rounding to the nearest beat when the first row reaches it; does nothing when the take has no rows.
    pub fn start_at_first_note(&mut self) {
        let rows = quantize_indexed(&self.edited, self.bpm, self.grid, self.origin_s);
        let Some((_, first)) = rows.first() else {
            return;
        };
        let start = first.start_beats;
        let reached_past_rounded = start + first.length_beats - start.round();
        let step = step_beats(self.grid).unwrap_or(MIN_LENGTH_B);
        let shift = if reached_past_rounded >= step - MIN_LENGTH_B {
            start.round()
        } else {
            start.floor()
        };
        self.shift_beats = if shift >= 1.0 { shift as u32 } else { 0 };
        self.requantize();
    }

    /// Returns how many whole beats beat 0 has moved later by; the rows are shifted in beat space by this amount.
    pub fn shift_beats(&self) -> u32 {
        self.shift_beats
    }

    /// Stores `s` as the short-note threshold of the segmentation config as [`Take::set_segment_config`] does: re-segments `frames`, resets the edits and requantizes; a non-finite or negative `s` is ignored.
    pub fn set_merge_note_s(&mut self, s: f64) {
        self.set_segment_config(SegmentConfig {
            merge_note_s: s,
            ..self.segment
        });
    }

    /// Returns the short-note threshold of the segmentation config.
    pub fn merge_note_s(&self) -> f64 {
        self.segment.merge_note_s
    }

    /// Returns the segmentation config of the raw notes.
    pub fn segment_config(&self) -> SegmentConfig {
        self.segment
    }

    /// Stores `segment` as the segmentation config, re-segments `frames` with it and resets the edits; a config whose `merge_note_s`, `min_note_s` or `max_gap_s` is not finite or negative, or whose `a4_hz` is not finite or not positive, is ignored.
    pub fn set_segment_config(&mut self, segment: SegmentConfig) {
        let seconds_valid = [segment.merge_note_s, segment.min_note_s, segment.max_gap_s]
            .iter()
            .all(|s| s.is_finite() && *s >= 0.0);
        if !seconds_valid || !segment.a4_hz.is_finite() || segment.a4_hz <= 0.0 {
            return;
        }
        self.raw = segment::segment(&self.frames, &segment);
        self.segment = segment;
        self.reset_edits();
    }

    /// Returns the MIDI export options of the take.
    pub fn midi_options(&self) -> MidiOptions {
        self.midi
    }

    /// Stores `midi` as the MIDI export options; the notes stay unchanged.
    pub fn set_midi_options(&mut self, midi: MidiOptions) {
        self.midi = midi;
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

    /// Returns the edited notes as a Standard MIDI File at the take's tempo, with the take's MIDI options.
    pub fn midi_bytes(&self) -> Vec<u8> {
        to_midi_bytes_with(&self.notes, self.bpm, &self.midi)
    }

    /// Returns the edited notes as CSV text; the frequency column follows the take's A4 reference.
    pub fn csv(&self) -> String {
        to_csv_at(&self.notes, self.segment.a4_hz)
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
    use crate::export::midi::to_midi_bytes;
    use crate::music::assert_close;

    /// Builds frames every 0.01 s from `start_s` to `end_s`, voiced at the frequency of the first span of `tones` that covers the frame time and unvoiced elsewhere.
    fn span_frames(start_s: f64, end_s: f64, tones: &[(f64, f64, f32)]) -> Vec<PitchFrame> {
        let count = ((end_s - start_s) / 0.01).round() as usize;
        (0..count)
            .map(|i| {
                let time_s = start_s + i as f64 * 0.01;
                let tone = tones
                    .iter()
                    .find(|(from, to, _)| time_s >= *from && time_s < *to);
                match tone {
                    Some((_, _, hz)) => PitchFrame {
                        time_s,
                        freq_hz: Some(*hz),
                        confidence: 0.95,
                    },
                    None => PitchFrame {
                        time_s,
                        freq_hz: None,
                        confidence: 0.0,
                    },
                }
            })
            .collect()
    }

    /// Builds a take at 120 BPM with beat 0 at 0.0 s on the quarter grid whose raw notes are C5 0.00..0.40 s, D5 0.45..0.57 s and E5 0.60..1.00 s; D5 snaps onto the start of E5 and is trimmed to zero length, so the rows show C5 [0,1) and E5 [1,2) while D5 has no row.
    fn hidden_middle_note_take() -> Take {
        let frames = span_frames(
            0.0,
            1.0,
            &[
                (0.00, 0.40, 523.25),
                (0.45, 0.57, 587.33),
                (0.60, 1.00, 659.26),
            ],
        );
        Take::new(frames, 0.0, 120.0, Grid::Quarter)
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
    fn with_config_segments_with_the_given_config() {
        let frames = tone_frames(1.0, 0.5, 440.0);
        let strict = Take::with_config(
            frames.clone(),
            1.0,
            120.0,
            Grid::Sixteenth,
            SegmentConfig {
                min_note_s: 1.0,
                ..SegmentConfig::default()
            },
            MidiOptions::default(),
        );
        assert!(strict.notes().is_empty());
        let take = Take::with_config(
            frames,
            1.0,
            120.0,
            Grid::Sixteenth,
            SegmentConfig::default(),
            MidiOptions::default(),
        );
        assert_eq!(take.notes().len(), 1);
    }

    #[test]
    fn set_segment_config_resegments_and_resets_the_edits() {
        let mut take = three_note_take();
        take.delete(1);
        assert!(take.is_edited());
        assert_eq!(take.notes().len(), 2);
        take.set_segment_config(SegmentConfig::default());
        assert!(!take.is_edited());
        assert_eq!(take.notes().len(), 3);
        take.set_segment_config(SegmentConfig {
            min_note_s: 1.0,
            ..SegmentConfig::default()
        });
        assert!(take.notes().is_empty());
        assert_eq!(take.segment_config().min_note_s, 1.0);
    }

    #[test]
    fn an_invalid_segment_config_is_ignored() {
        let mut take = three_note_take();
        let before = take.segment_config();
        take.set_segment_config(SegmentConfig {
            merge_note_s: f64::NAN,
            ..SegmentConfig::default()
        });
        assert_eq!(take.segment_config(), before);
        take.set_segment_config(SegmentConfig {
            a4_hz: 0.0,
            ..SegmentConfig::default()
        });
        assert_eq!(take.segment_config(), before);
    }

    #[test]
    fn set_merge_note_s_changes_only_the_merge_threshold() {
        let mut take = three_note_take();
        take.set_segment_config(SegmentConfig {
            min_note_s: 0.2,
            ..SegmentConfig::default()
        });
        take.set_merge_note_s(0.05);
        assert_eq!(take.segment_config().min_note_s, 0.2);
        assert_eq!(take.merge_note_s(), 0.05);
    }

    #[test]
    fn the_csv_follows_the_a4_reference() {
        let take = Take::with_config(
            tone_frames(1.0, 0.5, 432.0),
            1.0,
            120.0,
            Grid::Sixteenth,
            SegmentConfig {
                a4_hz: 432.0,
                ..SegmentConfig::default()
            },
            MidiOptions::default(),
        );
        assert!(take.csv().contains(",A4,69,432.00"));
    }

    #[test]
    fn midi_bytes_use_the_midi_options() {
        let mut take = three_note_take();
        take.set_midi_options(MidiOptions {
            velocity: 64,
            channel: 9,
        });
        assert_eq!(
            take.midi_bytes(),
            to_midi_bytes_with(
                take.notes(),
                120.0,
                &MidiOptions {
                    velocity: 64,
                    channel: 9,
                }
            )
        );
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
    fn merging_into_the_previous_row_skips_hidden_notes() {
        let mut take = hidden_middle_note_take();
        assert_eq!(take.rows().len(), 2);
        assert_eq!(take.rows()[0].0, 0);
        assert_eq!(take.rows()[0].1.midi, 72);
        assert_close(take.rows()[0].1.start_beats, 0.0, 1e-9);
        assert_close(take.rows()[0].1.length_beats, 1.0, 1e-9);
        assert_eq!(take.rows()[1].0, 2);
        assert_eq!(take.rows()[1].1.midi, 76);
        assert_close(take.rows()[1].1.start_beats, 1.0, 1e-9);
        assert_close(take.rows()[1].1.length_beats, 1.0, 1e-9);

        take.merge_into_previous(2);
        assert_eq!(take.rows().len(), 1);
        let only = take.rows()[0].1;
        assert_eq!(only.midi, 72);
        assert_close(only.start_beats, 0.0, 1e-9);
        assert_close(only.length_beats, 2.0, 1e-9);
        assert!(take.rows().iter().all(|(_, note)| note.midi != 74));
        assert!(take.notes().iter().all(|note| note.midi != 74));
    }

    #[test]
    fn deleting_a_row_removes_the_hidden_notes_before_it() {
        let mut take = hidden_middle_note_take();
        take.delete(2);
        assert_eq!(take.rows().len(), 1);
        assert_eq!(take.rows()[0].1.midi, 72);
        assert_close(take.rows()[0].1.start_beats, 0.0, 1e-9);
        assert_close(take.rows()[0].1.length_beats, 1.0, 1e-9);
        assert!(take.notes().iter().all(|note| note.midi != 74));
    }

    #[test]
    fn merging_into_the_next_row_skips_hidden_notes() {
        let mut take = hidden_middle_note_take();
        take.merge_into_next(0);
        assert_eq!(take.rows().len(), 1);
        let only = take.rows()[0].1;
        assert_eq!(only.midi, 76);
        assert_close(only.start_beats, 0.0, 1e-9);
        assert_close(only.length_beats, 2.0, 1e-9);
        assert!(take.rows().iter().all(|(_, note)| note.midi != 74));
        assert!(take.notes().iter().all(|note| note.midi != 74));
    }

    #[test]
    fn an_index_without_a_row_changes_nothing() {
        let mut take = hidden_middle_note_take();
        take.merge_into_previous(1);
        take.merge_into_next(1);
        take.delete(1);
        assert!(!take.is_edited());
        assert_eq!(take.rows().len(), 2);
        assert_eq!(take.rows()[0].1.midi, 72);
        assert_eq!(take.rows()[1].1.midi, 76);
    }

    #[test]
    fn start_at_first_note_keeps_a_short_first_note() {
        let frames = span_frames(1.0, 3.5, &[(2.30, 2.45, 440.0), (2.80, 3.25, 523.25)]);
        let mut take = Take::new(frames, 1.0, 120.0, Grid::Quarter);
        assert_eq!(take.rows().len(), 2);
        assert_eq!(take.rows()[0].1.midi, 69);
        assert_close(take.rows()[0].1.start_beats, 3.0, 1e-9);
        assert_close(take.rows()[0].1.length_beats, 1.0, 1e-9);

        take.start_at_first_note();
        assert_eq!(take.shift_beats(), 3);
        assert_eq!(take.rows().len(), 2);
        assert_eq!(take.rows()[0].1.midi, 69);
        assert_close(take.rows()[0].1.start_beats, 0.0, 1e-9);
        assert_close(take.rows()[0].1.length_beats, 1.0, 1e-9);
        assert_eq!(take.rows()[1].1.midi, 72);
        assert_close(take.rows()[1].1.start_beats, 1.0, 1e-9);
    }

    #[test]
    fn start_at_first_note_rounds_a_first_note_that_reaches_its_beat() {
        let mut frames = tone_frames(2.375, 0.5, 440.0);
        frames.extend(tone_frames(4.0, 0.5, 523.25));
        let mut take = Take::new(frames, 1.0, 120.0, Grid::Sixteenth);
        assert_eq!(take.rows().len(), 2);
        assert_close(take.rows()[0].1.start_beats, 2.75, 1e-9);
        assert_close(take.rows()[0].1.length_beats, 1.0, 1e-9);

        take.start_at_first_note();
        assert_eq!(take.shift_beats(), 3);
        assert_close(take.rows()[0].1.start_beats, 0.0, 1e-9);
        assert_close(take.rows()[0].1.length_beats, 0.75, 1e-9);
    }
}
