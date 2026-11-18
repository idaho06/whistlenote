//! whistlenote turns whistled pitches into musical notes.
//!
//! The crate converts pitch frames into note names, segments them into notes,
//! quantizes the notes to a beat grid, estimates tempo from onsets and taps,
//! and exports the result as MIDI and CSV. Audio devices are reached through
//! a backend abstraction with a deterministic fake backend for tests, and
//! microphone input is band-passed and pitch-tracked.

#![deny(missing_docs)]

pub mod audio;
pub mod dsp;
pub mod export;
pub mod music;
