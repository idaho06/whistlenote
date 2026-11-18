//! CSV export.

use crate::music::note::{midi_to_freq, midi_to_name};
use crate::music::quantize::QuantNote;
use std::fmt::Write;

/// Formats notes as CSV text with a header line and one row per note.
pub fn to_csv(notes: &[QuantNote]) -> String {
    let mut csv = String::from("start_beat,length_beats,note,midi,freq_hz\n");
    for note in notes {
        writeln!(
            csv,
            "{:.4},{:.4},{},{},{:.2}",
            note.start_beats,
            note.length_beats,
            midi_to_name(i32::from(note.midi)),
            note.midi,
            midi_to_freq(f32::from(note.midi)),
        )
        .expect("writing to a String is infallible");
    }
    csv
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a note at `start_beats` lasting `length_beats` with the given MIDI number.
    fn note(start_beats: f64, length_beats: f64, midi: u8) -> QuantNote {
        QuantNote {
            start_beats,
            length_beats,
            midi,
        }
    }

    #[test]
    fn two_notes_format_as_the_expected_csv() {
        let notes = [note(0.0, 1.0, 60), note(1.0, 0.5, 69)];
        assert_eq!(
            to_csv(&notes),
            "start_beat,length_beats,note,midi,freq_hz\n\
             0.0000,1.0000,C4,60,261.63\n\
             1.0000,0.5000,A4,69,440.00\n"
        );
    }

    #[test]
    fn empty_list_is_the_header_line_only() {
        assert_eq!(to_csv(&[]), "start_beat,length_beats,note,midi,freq_hz\n");
    }

    #[test]
    fn fractional_start_rounds_to_four_decimals() {
        assert_eq!(
            to_csv(&[note(0.3333333, 1.0, 60)]),
            "start_beat,length_beats,note,midi,freq_hz\n0.3333,1.0000,C4,60,261.63\n"
        );
    }

    #[test]
    fn sharp_note_uses_the_sharp_name() {
        assert_eq!(
            to_csv(&[note(0.0, 1.0, 61)]),
            "start_beat,length_beats,note,midi,freq_hz\n0.0000,1.0000,C#4,61,277.18\n"
        );
    }
}
