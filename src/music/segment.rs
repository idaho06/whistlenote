//! Segmentation of a pitch frame sequence into notes.

#![allow(unused_variables)]

/// One pitch analysis result at a point in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchFrame {
    /// Time of the centre of the analysis window, in seconds on the engine clock.
    pub time_s: f64,
    /// Detected frequency in Hz, or `None` when the frame is unvoiced.
    pub freq_hz: Option<f32>,
    /// Confidence of the detection in 0.0..=1.0.
    pub confidence: f32,
}

/// Parameters of [`segment`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentConfig {
    /// Minimum confidence for a frame to count as voiced.
    pub min_confidence: f32,
    /// Minimum duration of a note in seconds; shorter notes are dropped.
    pub min_note_s: f64,
    /// Length of the median filter window, in voiced frames.
    pub median_window: usize,
    /// Longest unvoiced gap in seconds that still joins two equal notes.
    pub max_gap_s: f64,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.8,
            min_note_s: 0.06,
            median_window: 5,
            max_gap_s: 0.04,
        }
    }
}

/// A note found in a pitch frame sequence, in seconds on the engine clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawNote {
    /// Start time in seconds.
    pub start_s: f64,
    /// End time in seconds.
    pub end_s: f64,
    /// MIDI note number.
    pub midi: i32,
}

/// Groups pitch frames into notes.
///
/// The frames must be ordered by `time_s`. The result is sorted by start time
/// and its notes do not overlap.
pub fn segment(frames: &[PitchFrame], cfg: &SegmentConfig) -> Vec<RawNote> {
    todo!()
}
