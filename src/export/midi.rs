//! Standard MIDI file export.

#![allow(unused_variables)]

use crate::music::quantize::QuantNote;

/// Serializes notes to a format 0 Standard MIDI File and returns its bytes.
///
/// A `bpm` that is not finite or not positive is treated as 120.
pub fn to_midi_bytes(notes: &[QuantNote], bpm: f64) -> Vec<u8> {
    todo!()
}
