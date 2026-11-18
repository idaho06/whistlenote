//! Tempo estimation from onsets and taps.

#![allow(unused_variables)]

/// Estimates a tempo in BPM from onset times in seconds.
///
/// Returns `None` when there are too few usable intervals. The result lies in
/// 40.0..=220.0 and is rounded to one decimal.
pub fn estimate_bpm(onsets_s: &[f64]) -> Option<f64> {
    todo!()
}

/// Tap tempo estimator.
#[derive(Debug, Clone, PartialEq)]
pub struct TapTempo {
    taps: Vec<f64>,
}

impl TapTempo {
    /// Creates an estimator with no taps.
    pub fn new() -> Self {
        Self { taps: Vec::new() }
    }

    /// Records a tap at `now_s` seconds and returns the tempo in BPM.
    ///
    /// Returns `None` when the tap is the only one stored.
    pub fn tap(&mut self, now_s: f64) -> Option<f64> {
        todo!()
    }

    /// Forgets all taps.
    pub fn reset(&mut self) {
        self.taps.clear();
    }
}

impl Default for TapTempo {
    fn default() -> Self {
        Self::new()
    }
}
