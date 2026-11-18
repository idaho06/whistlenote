//! Quantization of notes to a beat grid.

#![allow(unused_variables)]

use super::segment::RawNote;

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

/// Converts notes in seconds to notes in beats snapped to `grid`.
///
/// `origin_s` is the time of beat 0. Returns an empty vector when `bpm` is not
/// finite or not positive.
pub fn quantize(notes: &[RawNote], bpm: f64, grid: Grid, origin_s: f64) -> Vec<QuantNote> {
    todo!()
}
