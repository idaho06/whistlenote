//! Smoothing of live pitch frames into the note the live view shows.

use crate::engine::LiveFrame;
use crate::music::note::freq_to_midi_float;

/// Frames that must agree in a row before a new note is shown.
pub const AGREE_FRAMES: u32 = 3;

/// Distance in cents from the shown note within which the shown note stays.
pub const HOLD_CENTS: f32 = 65.0;

/// Seconds after the last voiced frame at which the shown note clears.
pub const CLEAR_S: f64 = 0.3;

/// Confidence a frame needs to count as voiced; the same as the default minimum confidence of note segmentation.
pub const MIN_CONFIDENCE: f32 = 0.8;

/// The note the live view shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShownNote {
    /// Nearest MIDI note number of the reading (60 is middle C, 69 is A4).
    pub midi: i32,
    /// Distance of the reading from `midi` in cents, within ±`HOLD_CENTS`.
    pub cents: f32,
    /// Frequency in Hz of the voiced frame that last updated the note.
    pub freq_hz: f32,
}

/// Turns live frames into a stable shown note.
///
/// A new note appears once [`AGREE_FRAMES`] voiced frames in a row agree on
/// the same nearest note, a shown note stays while the readings stay within
/// [`HOLD_CENTS`] of it, and the shown note clears [`CLEAR_S`] after the last
/// voiced frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NoteSmoother {
    /// The note the live view shows.
    shown: Option<ShownNote>,
    /// Nearest MIDI note of the latest voiced frames and how many agreed in a row.
    candidate: Option<(i32, u32)>,
    /// Engine-clock time of the last voiced frame in seconds.
    last_voiced_s: f64,
}

impl NoteSmoother {
    /// Folds `frame` into the smoother.
    ///
    /// A voiced frame updates the shown note while it stays within
    /// [`HOLD_CENTS`] of it and otherwise counts towards [`AGREE_FRAMES`]
    /// agreeing frames on a new note; an unvoiced frame breaks the agreement.
    /// Once [`CLEAR_S`] passes without a voiced frame, the shown note clears.
    pub fn push(&mut self, frame: &LiveFrame) {
        let voiced = match frame.freq_hz {
            Some(hz) if hz.is_finite() && hz > 0.0 && frame.confidence >= MIN_CONFIDENCE => {
                Some(hz)
            }
            _ => None,
        };
        if let Some(hz) = voiced {
            self.last_voiced_s = frame.time_s;
            let midi_float = freq_to_midi_float(hz);
            let held = self
                .shown
                .filter(|shown| ((midi_float - shown.midi as f32) * 100.0).abs() <= HOLD_CENTS);
            if let Some(shown) = held {
                self.shown = Some(ShownNote {
                    midi: shown.midi,
                    cents: (midi_float - shown.midi as f32) * 100.0,
                    freq_hz: hz,
                });
                self.candidate = None;
            } else {
                let nearest = midi_float.round() as i32;
                let count = match self.candidate {
                    Some((note, count)) if note == nearest => count + 1,
                    _ => 1,
                };
                if count >= AGREE_FRAMES {
                    self.shown = Some(ShownNote {
                        midi: nearest,
                        cents: (midi_float - nearest as f32) * 100.0,
                        freq_hz: hz,
                    });
                    self.candidate = None;
                } else {
                    self.candidate = Some((nearest, count));
                }
            }
        } else {
            self.candidate = None;
        }
        if self.shown.is_some() && frame.time_s - self.last_voiced_s > CLEAR_S {
            self.shown = None;
            self.candidate = None;
        }
    }

    /// Returns the note the live view shows.
    pub fn shown(&self) -> Option<ShownNote> {
        self.shown
    }

    /// Returns the smoother to its default state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::assert_close;
    use crate::music::note::midi_to_freq;
    use crate::music::segment::SegmentConfig;

    /// Returns a voiced frame at `time_s` with the pitch of `midi`.
    fn voiced(time_s: f64, midi: f32) -> LiveFrame {
        LiveFrame {
            time_s,
            freq_hz: Some(midi_to_freq(midi)),
            confidence: 0.95,
            level: 0.01,
        }
    }

    /// Returns an unvoiced frame at `time_s`.
    fn unvoiced(time_s: f64) -> LiveFrame {
        LiveFrame {
            time_s,
            freq_hz: None,
            confidence: 0.0,
            level: 0.01,
        }
    }

    #[test]
    fn three_agreeing_frames_show_a_note() {
        let mut smoother = NoteSmoother::default();
        smoother.push(&voiced(0.00, 69.0));
        smoother.push(&voiced(0.01, 69.0));
        assert_eq!(smoother.shown(), None);
        smoother.push(&voiced(0.02, 69.0));
        let shown = smoother.shown().expect("the third frame shows a note");
        assert_eq!(shown.midi, 69);
        assert_close(shown.cents as f64, 0.0, 0.5);
        assert_close(shown.freq_hz as f64, 440.0, 0.5);
    }

    #[test]
    fn an_unvoiced_frame_breaks_the_agreement() {
        let mut smoother = NoteSmoother::default();
        for frame in [
            voiced(0.00, 69.0),
            voiced(0.01, 69.0),
            unvoiced(0.02),
            voiced(0.03, 69.0),
            voiced(0.04, 69.0),
        ] {
            smoother.push(&frame);
        }
        assert_eq!(smoother.shown(), None);
        smoother.push(&voiced(0.05, 69.0));
        assert_eq!(smoother.shown().map(|note| note.midi), Some(69));
    }

    #[test]
    fn a_low_confidence_frame_counts_as_unvoiced() {
        let mut smoother = NoteSmoother::default();
        let mut low_confidence = voiced(0.02, 69.0);
        low_confidence.confidence = 0.5;
        for frame in [
            voiced(0.00, 69.0),
            voiced(0.01, 69.0),
            low_confidence,
            voiced(0.03, 69.0),
            voiced(0.04, 69.0),
        ] {
            smoother.push(&frame);
        }
        assert_eq!(smoother.shown(), None);
    }

    #[test]
    fn a_shown_note_stays_within_hold_cents() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        for time_s in [0.03, 0.04, 0.05, 0.06, 0.07] {
            smoother.push(&voiced(time_s, 69.6));
            let shown = smoother.shown().expect("the note stays shown");
            assert_eq!(shown.midi, 69);
            assert_close(shown.cents as f64, 60.0, 0.5);
        }
        for time_s in [0.08, 0.09] {
            smoother.push(&voiced(time_s, 69.7));
            assert_eq!(smoother.shown().map(|note| note.midi), Some(69));
        }
        smoother.push(&voiced(0.10, 69.7));
        let shown = smoother
            .shown()
            .expect("the third frame shows the new note");
        assert_eq!(shown.midi, 70);
        assert_close(shown.cents as f64, -30.0, 0.5);
    }

    #[test]
    fn a_shown_note_stays_within_hold_cents_downwards() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        smoother.push(&voiced(0.03, 68.4));
        let shown = smoother.shown().expect("the note stays shown");
        assert_eq!(shown.midi, 69);
        assert_close(shown.cents as f64, -60.0, 0.5);
    }

    #[test]
    fn the_shown_note_clears_after_clear_s_without_voiced_frames() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        assert!(smoother.shown().is_some());
        for time_s in [0.12, 0.22, 0.32] {
            smoother.push(&unvoiced(time_s));
            assert!(smoother.shown().is_some(), "still shown at {time_s}");
        }
        smoother.push(&unvoiced(0.33));
        assert_eq!(smoother.shown(), None);
    }

    #[test]
    fn reset_returns_to_the_default_state() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        assert!(smoother.shown().is_some());
        smoother.reset();
        assert_eq!(smoother.shown(), None);
        assert_eq!(smoother, NoteSmoother::default());
    }

    #[test]
    fn min_confidence_matches_the_segment_config_default() {
        assert_eq!(MIN_CONFIDENCE, SegmentConfig::default().min_confidence);
    }
}
