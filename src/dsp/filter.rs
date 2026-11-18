//! Butterworth biquad filters with state that persists across blocks.

use biquad::{Biquad, Coefficients, DirectForm2Transposed, Hertz, Type, Q_BUTTERWORTH_F32};

/// Cutoff of the band-pass high-pass section in Hz.
pub const BAND_LOW_HZ: f32 = 300.0;
/// Cutoff of the band-pass low-pass section in Hz.
pub const BAND_HIGH_HZ: f32 = 4000.0;

/// Highest cutoff as a fraction of the sample rate; higher cutoffs are
/// lowered to it so that they stay below the Nyquist frequency.
const MAX_CUTOFF_RATIO: f32 = 0.45;

/// The kind of a [`Butterworth`] section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    /// Passes frequencies above the cutoff.
    HighPass,
    /// Passes frequencies below the cutoff.
    LowPass,
}

/// A second-order Butterworth filter section.
#[derive(Debug, Clone, Copy)]
pub struct Butterworth {
    section: DirectForm2Transposed<f32>,
}

impl Butterworth {
    /// Creates a section with cutoff `cutoff_hz` at `sample_rate` Hz.
    ///
    /// A cutoff above 0.45 times the sample rate is lowered to that value.
    /// A sample rate or cutoff that yields no valid filter gives a section
    /// that passes its input unchanged.
    pub fn new(kind: FilterKind, sample_rate: u32, cutoff_hz: f32) -> Self {
        let fs = sample_rate as f32;
        let f0 = cutoff_hz.min(fs * MAX_CUTOFF_RATIO);
        let filter_type = match kind {
            FilterKind::HighPass => Type::HighPass,
            FilterKind::LowPass => Type::LowPass,
        };
        let coefficients = Hertz::from_hz(fs)
            .and_then(|fs| Ok((fs, Hertz::from_hz(f0)?)))
            .and_then(|(fs, f0)| {
                Coefficients::<f32>::from_params(filter_type, fs, f0, Q_BUTTERWORTH_F32)
            })
            .unwrap_or(Coefficients {
                a1: 0.0,
                a2: 0.0,
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
            });
        Self {
            section: DirectForm2Transposed::new(coefficients),
        }
    }

    /// Filters one sample.
    pub fn run(&mut self, sample: f32) -> f32 {
        self.section.run(sample)
    }

    /// Filters `block` in place.
    pub fn process(&mut self, block: &mut [f32]) {
        for sample in block {
            *sample = self.section.run(*sample);
        }
    }

    /// Clears the filter state, as if no sample had been filtered.
    pub fn reset(&mut self) {
        self.section.reset_state();
    }
}

/// A band-pass from [`BAND_LOW_HZ`] to [`BAND_HIGH_HZ`]: a Butterworth
/// high-pass followed by a Butterworth low-pass.
#[derive(Debug, Clone, Copy)]
pub struct BandPass {
    high_pass: Butterworth,
    low_pass: Butterworth,
}

impl BandPass {
    /// Creates the band-pass for `sample_rate` Hz.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            high_pass: Butterworth::new(FilterKind::HighPass, sample_rate, BAND_LOW_HZ),
            low_pass: Butterworth::new(FilterKind::LowPass, sample_rate, BAND_HIGH_HZ),
        }
    }

    /// Filters one sample.
    pub fn run(&mut self, sample: f32) -> f32 {
        self.low_pass.run(self.high_pass.run(sample))
    }

    /// Filters `block` in place.
    pub fn process(&mut self, block: &mut [f32]) {
        for sample in block {
            *sample = self.run(*sample);
        }
    }

    /// Clears the filter state, as if no sample had been filtered.
    pub fn reset(&mut self) {
        self.high_pass.reset();
        self.low_pass.reset();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::f32::consts::TAU;

    use super::*;
    use crate::audio::rms;

    /// Returns `len` samples of a sine at `freq_hz` with `amplitude`.
    pub(crate) fn sine(freq_hz: f32, amplitude: f32, sample_rate: u32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| amplitude * (TAU * freq_hz * i as f32 / sample_rate as f32).sin())
            .collect()
    }

    /// Returns the steady-state gain of `filter` for a sine at `freq_hz`.
    fn gain(mut filter: BandPass, freq_hz: f32, sample_rate: u32) -> f32 {
        let mut signal = sine(freq_hz, 1.0, sample_rate, sample_rate as usize);
        filter.process(&mut signal);
        let settled = &signal[signal.len() / 2..];
        rms(settled) / std::f32::consts::FRAC_1_SQRT_2
    }

    #[test]
    fn band_passes_whistle_range_and_rejects_rumble_and_hiss() {
        for rate in [44_100, 48_000] {
            let filter = BandPass::new(rate);
            for freq in [800.0, 1500.0, 2000.0] {
                let g = gain(filter, freq, rate);
                assert!(g > 0.9, "{freq} Hz at {rate} Hz has gain {g}");
            }
            assert!(gain(filter, 50.0, rate) < 0.05, "50 Hz rumble passes");
            assert!(gain(filter, 15_000.0, rate) < 0.1, "15 kHz hiss passes");
        }
    }

    #[test]
    fn state_persists_across_blocks() {
        let signal = sine(1000.0, 0.5, 48_000, 4800);
        let mut whole = signal.clone();
        BandPass::new(48_000).process(&mut whole);

        let mut split = signal;
        let mut filter = BandPass::new(48_000);
        let (first, second) = split.split_at_mut(1234);
        filter.process(first);
        filter.process(second);
        assert_eq!(whole, split);
    }

    #[test]
    fn reset_clears_the_state() {
        let mut filter = BandPass::new(48_000);
        let mut fresh = filter;
        let mut warm_up = sine(1000.0, 0.5, 48_000, 100);
        filter.process(&mut warm_up);
        filter.reset();
        assert_eq!(filter.run(0.25), fresh.run(0.25));
    }

    #[test]
    fn cutoffs_above_nyquist_still_filter() {
        // At 8 kHz the 4 kHz low-pass cutoff is lowered to 3.6 kHz.
        let mut low_pass = Butterworth::new(FilterKind::LowPass, 8000, BAND_HIGH_HZ);
        let mut signal = sine(500.0, 1.0, 8000, 8000);
        low_pass.process(&mut signal);
        assert!(signal.iter().all(|s| s.is_finite()));
        assert!(rms(&signal[4000..]) > 0.6);
    }
}
