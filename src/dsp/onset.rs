//! Onset detection of claps and taps in high-passed input.
//!
//! [`OnsetDetector`] high-passes the input, analyses windows of [`WINDOW`]
//! samples every [`HOP`] samples, and reports an onset when the positive
//! change of the window's log energy rises above a threshold derived from
//! the recent flux history, the window is loud enough and the refractory
//! period since the last onset has elapsed.

use std::collections::VecDeque;

use super::filter::{Butterworth, FilterKind};
use crate::audio::rms;
use crate::music::median;

/// Analysis window length in samples.
pub const WINDOW: usize = 512;
/// Distance in samples between the starts of consecutive windows.
pub const HOP: usize = 128;

/// Default cutoff of the high-pass in Hz.
const HIGH_PASS_HZ: f32 = 1000.0;
/// Floor added to the window energy before taking its logarithm.
const LOG_FLOOR: f64 = 1e-9;
/// Default length of the flux history in seconds.
const FLUX_HISTORY_S: f64 = 0.5;
/// Default offset added to the median flux to form the detection threshold.
const THRESHOLD_OFFSET: f64 = 1.0;
/// Default minimum spacing between reported onsets in seconds.
const REFRACTORY_S: f64 = 0.1;

/// Samples from the last sample of a full window back to its centre, the
/// sample at index `WINDOW / 2`.
const CENTRE_OFFSET: usize = WINDOW - 1 - WINDOW / 2;

/// Returns the number of flux values that a history of `flux_history_s`
/// seconds holds at `sample_rate` Hz.
fn history_len(flux_history_s: f64, sample_rate: u32) -> usize {
    (flux_history_s * f64::from(sample_rate) / HOP as f64).round() as usize
}

/// Settings of an [`OnsetDetector`]. The caller keeps the values sensible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnsetConfig {
    /// Window RMS, after high-passing, below which no onset is reported.
    pub gate_rms: f32,
    /// Offset added to the median flux to form the detection threshold.
    pub threshold_offset: f64,
    /// Minimum spacing between reported onsets in seconds.
    pub refractory_s: f64,
    /// Cutoff of the high-pass in Hz.
    pub high_pass_hz: f32,
    /// Length of the flux history in seconds.
    pub flux_history_s: f64,
}

impl Default for OnsetConfig {
    /// A gate of 0.01 RMS, a threshold offset of 1.0, a refractory period of
    /// 0.1 s, a high-pass cutoff of 1000 Hz and a flux history of 0.5 s.
    fn default() -> Self {
        Self {
            gate_rms: 0.01,
            threshold_offset: THRESHOLD_OFFSET,
            refractory_s: REFRACTORY_S,
            high_pass_hz: HIGH_PASS_HZ,
            flux_history_s: FLUX_HISTORY_S,
        }
    }
}

/// Turns blocks of mono samples into onset times in engine-clock seconds.
pub struct OnsetDetector {
    config: OnsetConfig,
    sample_rate: u32,
    filter: Butterworth,
    window: Vec<f32>,
    flux_history: VecDeque<f64>,
    history_len: usize,
    prev_log_e: Option<f64>,
    last_onset_s: Option<f64>,
}

impl OnsetDetector {
    /// Creates a detector for input at `sample_rate` Hz.
    pub fn new(sample_rate: u32, config: OnsetConfig) -> Self {
        let len = history_len(config.flux_history_s, sample_rate);
        Self {
            config,
            sample_rate,
            filter: Butterworth::new(FilterKind::HighPass, sample_rate, config.high_pass_hz),
            window: Vec::with_capacity(WINDOW),
            flux_history: VecDeque::with_capacity(len),
            history_len: len,
            prev_log_e: None,
            last_onset_s: None,
        }
    }

    /// Returns the detector's current configuration.
    pub fn config(&self) -> OnsetConfig {
        self.config
    }

    /// Applies `config` in place. The partial window, the flux history, the
    /// stored log energy and the last onset time are kept. When `high_pass_hz`
    /// differs from the current value, the filter is replaced with a fresh
    /// one and its state is lost; when the history length changes, the oldest
    /// flux values are dropped until the history fits the new length.
    pub fn set_config(&mut self, config: OnsetConfig) {
        if config.high_pass_hz != self.config.high_pass_hz {
            self.filter =
                Butterworth::new(FilterKind::HighPass, self.sample_rate, config.high_pass_hz);
        }
        let len = history_len(config.flux_history_s, self.sample_rate);
        if len != self.history_len {
            self.history_len = len;
            while self.flux_history.len() > self.history_len {
                self.flux_history.pop_front();
            }
        }
        self.config = config;
    }

    /// Analyses `block`, whose first sample has engine-clock time
    /// `start_time_s`, and appends the times of detected onsets to `out`.
    ///
    /// The samples continue those of earlier blocks without a gap; after a
    /// gap, call [`reset`](Self::reset) first.
    pub fn process(&mut self, block: &[f32], start_time_s: f64, out: &mut Vec<f64>) {
        let rate = f64::from(self.sample_rate);
        for (index, &sample) in block.iter().enumerate() {
            self.window.push(self.filter.run(sample));
            if self.window.len() == WINDOW {
                let end_time_s = start_time_s + index as f64 / rate;
                let time_s = end_time_s - CENTRE_OFFSET as f64 / rate;
                self.analyse(time_s, out);
                self.window.drain(..HOP);
            }
        }
    }

    /// Discards the partial window, the filter state, the flux history, the
    /// stored log energy and the last onset time.
    pub fn reset(&mut self) {
        self.window.clear();
        self.filter.reset();
        self.flux_history.clear();
        self.prev_log_e = None;
        self.last_onset_s = None;
    }

    /// Analyses the full window as a candidate onset at `time_s` and appends
    /// `time_s` to `out` when the window qualifies.
    fn analyse(&mut self, time_s: f64, out: &mut Vec<f64>) {
        let energy = self
            .window
            .iter()
            .map(|&sample| {
                let sample = f64::from(sample);
                sample * sample
            })
            .sum::<f64>()
            / WINDOW as f64;
        let log_e = (energy + LOG_FLOOR).ln();
        if let Some(prev_log_e) = self.prev_log_e {
            let flux = (log_e - prev_log_e).max(0.0);
            // The flux values are never negative, so their median is at
            // least 0 and a flux up to the configured threshold offset never
            // qualifies; the cheap tests run first and the median only when
            // they pass.
            let is_onset = flux > self.config.threshold_offset
                && self
                    .last_onset_s
                    .is_none_or(|last| time_s - last >= self.config.refractory_s)
                && rms(&self.window) >= self.config.gate_rms
                && flux
                    > median(self.flux_history.iter().copied().collect()).unwrap_or(0.0)
                        + self.config.threshold_offset;
            if is_onset {
                out.push(time_s);
                self.last_onset_s = Some(time_s);
            }
            self.flux_history.push_back(flux);
            while self.flux_history.len() > self.history_len {
                self.flux_history.pop_front();
            }
        }
        self.prev_log_e = Some(log_e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::filter::tests::{Noise, sine};
    use crate::music::assert_close;

    /// Feeds `signal` to `detector` in blocks of 300 samples, the first
    /// sample having time `start_time_s`, and appends onsets to `out`.
    fn run(
        detector: &mut OnsetDetector,
        signal: &[f32],
        rate: u32,
        start_time_s: f64,
        out: &mut Vec<f64>,
    ) {
        for (index, block) in signal.chunks(300).enumerate() {
            let time_s = start_time_s + (index * 300) as f64 / f64::from(rate);
            detector.process(block, time_s, out);
        }
    }

    /// Runs a default detector over `signal` in blocks of 300 samples and
    /// returns the onsets.
    fn detect(signal: &[f32], rate: u32) -> Vec<f64> {
        let mut detector = OnsetDetector::new(rate, OnsetConfig::default());
        let mut onsets = Vec::new();
        run(&mut detector, signal, rate, 0.0, &mut onsets);
        onsets
    }

    /// Returns 4 s of background noise at peak 0.001 with 8 clicks at
    /// `0.25 + k × 0.5` s, each 5 ms of noise decaying from peak 0.5.
    fn click_track(rate: u32, seed: u32) -> Vec<f32> {
        let len = 4 * rate as usize;
        let mut noise = Noise(seed);
        let mut signal: Vec<f32> = (0..len).map(|_| noise.next() * 0.001).collect();
        let tau = 0.005 / 100.0_f64.ln();
        let click_len = (0.005 * f64::from(rate)) as usize;
        for k in 0..8_usize {
            let start = ((0.25 + 0.5 * k as f64) * f64::from(rate)).round() as usize;
            for (i, sample) in signal[start..start + click_len].iter_mut().enumerate() {
                let t = i as f64 / f64::from(rate);
                *sample += noise.next() * 0.5 * (-t / tau).exp() as f32;
            }
        }
        signal
    }

    #[test]
    fn a_tap_between_silence_gives_one_onset() {
        let rate = 48_000;
        // 0.5 s of silence, 5 ms of a 3 kHz tap, 0.5 s of silence.
        let mut signal = vec![0.0; rate as usize / 2];
        signal.extend(sine(3000.0, 0.5, rate, rate as usize / 200));
        signal.extend(vec![0.0; rate as usize / 2]);

        let onsets = detect(&signal, rate);
        assert_eq!(onsets.len(), 1);
        assert_close(onsets[0], 0.5, 0.015);
    }

    #[test]
    fn clicks_at_120_bpm_are_found_once_each() {
        for rate in [48_000_u32, 44_100] {
            let onsets = detect(&click_track(rate, 0x2545_F491), rate);
            assert_eq!(onsets.len(), 8, "rate {rate}: {onsets:?}");
            for (k, &onset) in onsets.iter().enumerate() {
                assert_close(onset, 0.25 + 0.5 * k as f64, 0.015);
            }
        }
    }

    #[test]
    fn low_frequency_rumble_does_not_trigger() {
        let rate = 48_000;
        let len = 2 * rate as usize;
        let fade_len = (0.020 * f64::from(rate)) as usize;
        let tone = sine(100.0, 0.3, rate, len);
        let mut noise = Noise(0x1D2B_3C4D);
        let signal: Vec<f32> = tone
            .iter()
            .enumerate()
            .map(|(i, &sample)| {
                let gain = (i as f64 / fade_len as f64).min(1.0) as f32;
                sample * gain + noise.next() * 0.001
            })
            .collect();
        let onsets = detect(&signal, rate);
        assert!(onsets.is_empty(), "onsets: {onsets:?}");
    }

    #[test]
    fn a_steady_tone_triggers_only_at_its_start() {
        let rate = 48_000;
        let mut noise = Noise(0x0BAD_F00D);
        let mut signal: Vec<f32> = (0..rate as usize / 2)
            .map(|_| noise.next() * 0.001)
            .collect();
        // A 1.5 kHz tone starts abruptly at 0.5 s and sounds for 2 s.
        signal.extend(sine(1500.0, 0.5, rate, 2 * rate as usize));
        let onsets = detect(&signal, rate);
        assert_eq!(onsets.len(), 1, "onsets: {onsets:?}");
        assert_close(onsets[0], 0.5, 0.015);
    }

    #[test]
    fn silence_and_background_noise_do_not_trigger() {
        let rate = 48_000;
        let silence = vec![0.0_f32; 2 * rate as usize];
        let onsets = detect(&silence, rate);
        assert!(onsets.is_empty(), "onsets: {onsets:?}");

        let mut noise = Noise(0x5EED_1234);
        let background: Vec<f32> = (0..2 * rate as usize)
            .map(|_| noise.next() * 0.001)
            .collect();
        let onsets = detect(&background, rate);
        assert!(onsets.is_empty(), "onsets: {onsets:?}");
    }

    #[test]
    fn reset_discards_earlier_input() {
        let rate = 48_000;
        let signal = click_track(rate, 0xC1C0_567A);
        let mut detector = OnsetDetector::new(rate, OnsetConfig::default());
        let mut before = Vec::new();
        // The first 0.3 s, including the 0.25 s click.
        run(&mut detector, &signal[..14_400], rate, 0.0, &mut before);
        detector.reset();
        // The rest of the track, with its original times.
        let mut after = Vec::new();
        run(&mut detector, &signal[14_400..], rate, 0.3, &mut after);
        assert_eq!(after.len(), 7, "onsets: {after:?}");
        for (k, &onset) in after.iter().enumerate() {
            assert_close(onset, 0.75 + 0.5 * k as f64, 0.015);
        }
    }

    #[test]
    fn a_threshold_above_any_flux_hides_every_click() {
        let rate = 48_000;
        let mut detector = OnsetDetector::new(
            rate,
            OnsetConfig {
                threshold_offset: 50.0,
                ..Default::default()
            },
        );
        let mut onsets = Vec::new();
        run(
            &mut detector,
            &click_track(rate, 0x2545_F491),
            rate,
            0.0,
            &mut onsets,
        );
        assert!(onsets.is_empty(), "onsets: {onsets:?}");
    }

    #[test]
    fn a_long_refractory_period_skips_every_other_click() {
        let rate = 48_000;
        let mut detector = OnsetDetector::new(
            rate,
            OnsetConfig {
                refractory_s: 0.6,
                ..Default::default()
            },
        );
        let mut onsets = Vec::new();
        run(
            &mut detector,
            &click_track(rate, 0x2545_F491),
            rate,
            0.0,
            &mut onsets,
        );
        assert_eq!(onsets.len(), 4, "onsets: {onsets:?}");
        for (k, &onset) in onsets.iter().enumerate() {
            assert_close(onset, 0.25 + k as f64, 0.015);
        }
    }

    #[test]
    fn a_gate_above_the_click_level_hides_every_click() {
        let rate = 48_000;
        let mut detector = OnsetDetector::new(
            rate,
            OnsetConfig {
                gate_rms: 0.6,
                ..Default::default()
            },
        );
        let mut onsets = Vec::new();
        run(
            &mut detector,
            &click_track(rate, 0x2545_F491),
            rate,
            0.0,
            &mut onsets,
        );
        assert!(onsets.is_empty(), "onsets: {onsets:?}");
    }

    #[test]
    fn set_config_with_the_same_values_changes_nothing() {
        let rate = 48_000;
        let signal = click_track(rate, 0x2545_F491);
        let all = detect(&signal, rate);

        let mut detector = OnsetDetector::new(rate, OnsetConfig::default());
        let mut onsets = Vec::new();
        run(&mut detector, &signal[..96_000], rate, 0.0, &mut onsets);
        detector.set_config(OnsetConfig::default());
        run(&mut detector, &signal[96_000..], rate, 2.0, &mut onsets);

        assert_eq!(onsets.len(), all.len(), "onsets: {onsets:?} vs {all:?}");
        for (&applied, &expected) in onsets.iter().zip(&all) {
            assert!(
                (applied - expected).abs() <= 1e-9,
                "{applied} and {expected} differ by more than 1e-9"
            );
        }
    }

    #[test]
    fn set_config_with_a_longer_history_keeps_detecting() {
        let rate = 48_000;
        let signal = click_track(rate, 0x2545_F491);
        let mut detector = OnsetDetector::new(rate, OnsetConfig::default());
        let mut onsets = Vec::new();
        run(&mut detector, &signal[..96_000], rate, 0.0, &mut onsets);
        detector.set_config(OnsetConfig {
            flux_history_s: 1.0,
            ..Default::default()
        });
        run(&mut detector, &signal[96_000..], rate, 2.0, &mut onsets);
        assert_eq!(onsets.len(), 8, "onsets: {onsets:?}");
        for (k, &onset) in onsets.iter().enumerate() {
            assert_close(onset, 0.25 + 0.5 * k as f64, 0.015);
        }
    }

    #[test]
    fn config_returns_the_applied_config() {
        let rate = 48_000;
        let config = OnsetConfig {
            gate_rms: 0.02,
            threshold_offset: 2.0,
            refractory_s: 0.2,
            high_pass_hz: 500.0,
            flux_history_s: 0.25,
        };
        let mut detector = OnsetDetector::new(rate, config);
        assert_eq!(detector.config(), config);
        let changed = OnsetConfig {
            refractory_s: 0.3,
            ..config
        };
        detector.set_config(changed);
        assert_eq!(detector.config(), changed);
    }
}
