//! File export of quantized notes.

pub mod csv;
pub mod midi;

/// Builds a note at `start_beats` lasting `length_beats` with the given MIDI number.
#[cfg(test)]
pub(crate) fn note(
    start_beats: f64,
    length_beats: f64,
    midi: u8,
) -> crate::music::quantize::QuantNote {
    crate::music::quantize::QuantNote {
        start_beats,
        length_beats,
        midi,
    }
}
