//! Butterworth biquad filters with state that persists across blocks.

use biquad::{Biquad, Coefficients, DirectForm2Transposed, Hertz, Q_BUTTERWORTH_F32, Type};

/// Cutoff of the band-pass high-pass section in Hz.
pub const BAND_LOW_HZ: f32 = 300.0;
/// Cutoff of the band-pass low-pass section in Hz.
pub const BAND_HIGH_HZ: f32 = 4000.0;

/// Highest cutoff as a fraction of the sample rate; higher cutoffs are
/// lowered to it so that they stay below the Nyquist frequency.
const MAX_CUTOFF_RATIO: f32 = 0.45;

/// Coefficients that pass a sample through unchanged, used when no valid
/// filter can be built.
const PASS_THROUGH: Coefficients<f32> = Coefficients {
    a1: 0.0,
    a2: 0.0,
    b0: 1.0,
    b1: 0.0,
    b2: 0.0,
};

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
        let coefficients = section_coefficients(filter_type, fs, f0).unwrap_or(PASS_THROUGH);
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
            *sample = self.run(*sample);
        }
    }

    /// Clears the filter state, as if no sample had been filtered.
    pub fn reset(&mut self) {
        self.section.reset_state();
    }
}

/// Returns the coefficients of a `filter_type` section at `sample_rate` Hz
/// with cutoff `cutoff_hz` Hz, or `None` when a value yields no valid filter.
fn section_coefficients(
    filter_type: Type<f32>,
    sample_rate: f32,
    cutoff_hz: f32,
) -> Option<Coefficients<f32>> {
    let sample_rate = Hertz::from_hz(sample_rate).ok()?;
    let cutoff = Hertz::from_hz(cutoff_hz).ok()?;
    Coefficients::from_params(filter_type, sample_rate, cutoff, Q_BUTTERWORTH_F32).ok()
}

/// A band-pass from `low_hz` to `high_hz`: a Butterworth high-pass at
/// `low_hz` followed by a Butterworth low-pass at `high_hz`. The pitch
/// tracker's defaults are [`BAND_LOW_HZ`] and [`BAND_HIGH_HZ`].
#[derive(Debug, Clone, Copy)]
pub struct BandPass {
    high_pass: Butterworth,
    low_pass: Butterworth,
}

impl BandPass {
    /// Creates the band-pass from `low_hz` to `high_hz` for `sample_rate`
    /// Hz.
    ///
    /// A cutoff above 0.45 times the sample rate is lowered to that value.
    pub fn new(sample_rate: u32, low_hz: f32, high_hz: f32) -> Self {
        Self {
            high_pass: Butterworth::new(FilterKind::HighPass, sample_rate, low_hz),
            low_pass: Butterworth::new(FilterKind::LowPass, sample_rate, high_hz),
        }
    }

    /// Filters one sample.
    pub fn run(&mut self, sample: f32) -> f32 {
        self.low_pass.run(self.high_pass.run(sample))
    }

    /// Filters `block` in place.
    pub fn process(&mut self, block: &mut [f32]) {
        self.high_pass.process(block);
        self.low_pass.process(block);
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

    /// A seeded xorshift32 generator of uniform noise in -1..1.
    pub(crate) struct Noise(pub(crate) u32);

    impl Noise {
        /// Returns the next pseudo-random value in -1..1.
        pub(crate) fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
        }
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
            let filter = BandPass::new(rate, BAND_LOW_HZ, BAND_HIGH_HZ);
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
        BandPass::new(48_000, BAND_LOW_HZ, BAND_HIGH_HZ).process(&mut whole);

        let mut split = signal;
        let mut filter = BandPass::new(48_000, BAND_LOW_HZ, BAND_HIGH_HZ);
        let (first, second) = split.split_at_mut(1234);
        filter.process(first);
        filter.process(second);
        assert_eq!(whole, split);
    }

    #[test]
    fn reset_clears_the_state() {
        let mut filter = BandPass::new(48_000, BAND_LOW_HZ, BAND_HIGH_HZ);
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
