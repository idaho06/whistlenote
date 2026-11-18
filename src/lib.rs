//! whistlenote turns whistled pitches into musical notes.
//!
//! The crate converts pitch frames into note names, segments them into notes,
//! quantizes the notes to a beat grid, estimates tempo from onsets and taps,
//! and exports the result as MIDI and CSV.

#![deny(missing_docs)]

pub mod export;
pub mod music;
