//! Audio input and output behind a backend abstraction.
//!
//! [`backend`] defines the traits the engine uses, [`ring`] carries input
//! blocks from the audio callback to the engine thread, and [`fake`] is a
//! deterministic backend for tests.

use std::fmt;

pub mod backend;
pub mod fake;
pub mod ring;

/// An error reported by an audio backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// The host has no default device of the requested direction.
    NoDevice,
    /// No device has the requested name.
    UnknownDevice(String),
    /// The device's sample format is not supported.
    UnsupportedFormat(String),
    /// The backend reports a failure, described by the message.
    Backend(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoDevice => write!(f, "no audio device is available"),
            AudioError::UnknownDevice(name) => write!(f, "unknown audio device: {name}"),
            AudioError::UnsupportedFormat(format) => {
                write!(f, "unsupported sample format: {format}")
            }
            AudioError::Backend(message) => write!(f, "audio backend error: {message}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Returns the root mean square of `samples`, or 0 for an empty slice.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_a_square_wave_is_its_amplitude() {
        assert_eq!(rms(&[]), 0.0);
        assert!((rms(&[0.5, -0.5, 0.5, -0.5]) - 0.5).abs() < 1e-6);
    }
}
