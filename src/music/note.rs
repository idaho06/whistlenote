//! Conversion between frequencies, MIDI note numbers and note names.

#![allow(unused_variables)]

/// A frequency expressed as the nearest MIDI note plus a deviation in cents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteReading {
    /// Nearest MIDI note number (60 is middle C, 69 is A4).
    pub midi: i32,
    /// Deviation from the nearest note in cents, within -50.0..=50.0.
    pub cents: f32,
}

/// Returns the frequency in Hz of a (possibly fractional) MIDI note number.
///
/// MIDI note 69 (A4) is 440.0 Hz.
pub fn midi_to_freq(midi: f32) -> f32 {
    todo!()
}

/// Returns the fractional MIDI note number of a frequency in Hz.
///
/// This is the inverse of [`midi_to_freq`].
pub fn freq_to_midi_float(hz: f32) -> f32 {
    todo!()
}

/// Returns the nearest note of a frequency in Hz.
///
/// Returns `None` when `hz` is not finite or is below 1 Hz.
pub fn from_freq(hz: f32) -> Option<NoteReading> {
    todo!()
}

/// Returns the scientific pitch name of a MIDI note number.
///
/// Sharps are written with `#`, and middle C (60) is `C4`.
/// For example 61 is `C#4` and -1 is `B-2`.
pub fn midi_to_name(midi: i32) -> String {
    todo!()
}
