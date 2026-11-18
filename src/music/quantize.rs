//! Quantization of notes to a beat grid.

use super::segment::RawNote;

/// Smallest length in beats that survives filtering.
const MIN_LENGTH_B: f64 = 1e-9;

/// Resolution of the quantization grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Grid {
    /// No snapping; beat positions keep their exact values.
    Off,
    /// Snap to quarter notes (1 beat).
    Quarter,
    /// Snap to eighth notes (0.5 beat).
    Eighth,
    /// Snap to sixteenth notes (0.25 beat).
    #[default]
    Sixteenth,
}

/// A note positioned in beats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuantNote {
    /// Start position in beats from beat 0.
    pub start_beats: f64,
    /// Length in beats.
    pub length_beats: f64,
    /// MIDI note number in 0..=127.
    pub midi: u8,
}

/// Returns the grid step in beats, or `None` for [`Grid::Off`].
fn step_beats(grid: Grid) -> Option<f64> {
    match grid {
        Grid::Off => None,
        Grid::Quarter => Some(1.0),
        Grid::Eighth => Some(0.5),
        Grid::Sixteenth => Some(0.25),
    }
}

/// Rounds `x` to the nearest multiple of `step`; ties round up.
fn snap(x: f64, step: f64) -> f64 {
    (x / step + 0.5).floor() * step
}

/// Converts notes in seconds to notes in beats snapped to `grid`.
///
/// `origin_s` is the time of beat 0. Returns an empty vector when `bpm` is not
/// finite or not positive.
pub fn quantize(notes: &[RawNote], bpm: f64, grid: Grid, origin_s: f64) -> Vec<QuantNote> {
    if !bpm.is_finite() || bpm <= 0.0 {
        return Vec::new();
    }
    let mut sorted: Vec<RawNote> = notes.to_vec();
    sorted.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));

    let step = step_beats(grid);
    let to_beats = |t_s: f64| (t_s - origin_s) * bpm / 60.0;
    let mut quant: Vec<QuantNote> = Vec::with_capacity(sorted.len());
    for note in &sorted {
        let start_b = to_beats(note.start_s);
        let end_b = to_beats(note.end_s);
        if end_b <= 0.0 {
            continue;
        }
        let start_b = if start_b < 0.0 { 0.0 } else { start_b };
        let (start_b, length_b) = match step {
            Some(step) => {
                let start_b = snap(start_b, step);
                let length_b = snap(end_b, step) - start_b;
                (start_b, if length_b <= 0.0 { step } else { length_b })
            }
            None => {
                let length_b = end_b - start_b;
                if length_b <= MIN_LENGTH_B {
                    continue;
                }
                (start_b, length_b)
            }
        };
        quant.push(QuantNote {
            start_beats: start_b,
            length_beats: length_b,
            midi: note.midi.clamp(0, 127) as u8,
        });
    }

    for i in 1..quant.len() {
        let next_start = quant[i].start_beats;
        let end_b = (quant[i - 1].start_beats + quant[i - 1].length_beats).min(next_start);
        quant[i - 1].length_beats = end_b - quant[i - 1].start_beats;
    }
    quant.retain(|note| note.length_beats > MIN_LENGTH_B);
    quant
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::assert_close;

    /// Builds a raw note from `start_s` to `end_s` at MIDI 69.
    fn raw(start_s: f64, end_s: f64) -> RawNote {
        raw_midi(start_s, end_s, 69)
    }

    /// Builds a raw note with an explicit MIDI number.
    fn raw_midi(start_s: f64, end_s: f64, midi: i32) -> RawNote {
        RawNote {
            start_s,
            end_s,
            midi,
        }
    }

    /// Asserts that `note` starts at `start_beats` and lasts `length_beats`.
    fn assert_note(note: &QuantNote, start_beats: f64, length_beats: f64) {
        assert_close(note.start_beats, start_beats, 1e-9);
        assert_close(note.length_beats, length_beats, 1e-9);
    }

    #[test]
    fn sixteenth_note_snaps_to_whole_beats() {
        let notes = quantize(&[raw(0.51, 0.99)], 120.0, Grid::Sixteenth, 0.0);
        assert_eq!(notes.len(), 1);
        assert_note(&notes[0], 1.0, 1.0);
    }

    #[test]
    fn note_shorter_than_a_step_gets_one_step() {
        let notes = quantize(&[raw(0.0, 0.01)], 120.0, Grid::Sixteenth, 0.0);
        assert_eq!(notes.len(), 1);
        assert_note(&notes[0], 0.0, 0.25);
    }

    #[test]
    fn snapped_overlap_is_clamped_to_the_next_start() {
        let notes = quantize(&[raw(0.0, 0.8), raw(0.6, 1.0)], 120.0, Grid::Quarter, 0.0);
        assert_eq!(notes.len(), 2);
        assert_note(&notes[0], 0.0, 1.0);
        assert_note(&notes[1], 1.0, 1.0);
    }

    #[test]
    fn off_grid_keeps_exact_beat_values() {
        let notes = quantize(&[raw(0.1, 0.35)], 120.0, Grid::Off, 0.0);
        assert_eq!(notes.len(), 1);
        assert_note(&notes[0], 0.2, 0.5);
    }

    #[test]
    fn start_before_origin_is_clamped_to_zero() {
        let notes = quantize(&[raw(-0.1, 0.5)], 120.0, Grid::Sixteenth, 0.0);
        assert_eq!(notes.len(), 1);
        assert_note(&notes[0], 0.0, 1.0);
    }

    #[test]
    fn note_ending_before_origin_is_dropped() {
        let notes = quantize(&[raw(-1.0, -0.5)], 120.0, Grid::Sixteenth, 0.0);
        assert!(notes.is_empty());
    }

    #[test]
    fn non_positive_or_nan_bpm_gives_no_notes() {
        for bpm in [0.0, -120.0, f64::NAN] {
            let notes = quantize(&[raw(0.0, 1.0)], bpm, Grid::Sixteenth, 0.0);
            assert!(notes.is_empty());
        }
    }

    #[test]
    fn unsorted_input_comes_out_sorted() {
        let notes = quantize(&[raw(1.0, 1.5), raw(0.0, 0.5)], 120.0, Grid::Quarter, 0.0);
        assert_eq!(notes.len(), 2);
        assert_note(&notes[0], 0.0, 1.0);
        assert_note(&notes[1], 2.0, 1.0);
    }

    #[test]
    fn midi_is_clamped_into_byte_range() {
        let notes = quantize(
            &[raw_midi(0.0, 0.5, 200), raw_midi(0.6, 1.0, -5)],
            120.0,
            Grid::Quarter,
            0.0,
        );
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].midi, 127);
        assert_eq!(notes[1].midi, 0);
    }

    #[test]
    fn origin_moves_beat_zero() {
        let notes = quantize(&[raw(2.5, 3.0)], 120.0, Grid::Quarter, 2.0);
        assert_eq!(notes.len(), 1);
        assert_note(&notes[0], 1.0, 1.0);
    }
}
