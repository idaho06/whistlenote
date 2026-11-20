//! Pitch tracking of band-passed input with the McLeod pitch method.
//!
//! [`PitchTracker`] band-passes the input, analyses windows of [`WINDOW`]
//! samples every [`HOP`] samples at sample rates below 72 kHz, scaled by 2
//! or 4 at higher rates, and stamps each [`PitchFrame`] with the
//! engine-clock time of the window's centre sample (index `window_len / 2`).

use pitch_detection::detector::PitchDetector;
use pitch_detection::detector::mcleod::McLeodDetector;

use super::filter::{BAND_HIGH_HZ, BandPass};
use crate::audio::rms;
use crate::music::segment::PitchFrame;

/// Analysis window length in samples at sample rates below 72 kHz; the
/// tracker multiplies it by its scale (2 below 144 kHz, 4 from 144 kHz).
pub const WINDOW: usize = 2048;
/// Distance in samples between the starts of consecutive windows at sample
/// rates below 72 kHz; the tracker multiplies it by its scale (2 below
/// 144 kHz, 4 from 144 kHz).
pub const HOP: usize = 512;
/// Lowest reported frequency in Hz.
pub const MIN_FREQ_HZ: f32 = 400.0;
/// Highest reported frequency in Hz; the top of the band-pass. The tracker
/// also caps reported pitches at a quarter of the sample rate.
pub const MAX_FREQ_HZ: f32 = BAND_HIGH_HZ;

/// Clarity a peak of the normalized square difference function needs to be
/// considered by the detector.
const CLARITY_THRESHOLD: f32 = 0.5;

/// Returns the factor by which a tracker at `sample_rate` Hz scales
/// [`WINDOW`] and [`HOP`]: 1 below 72 kHz, 2 below 144 kHz, 4 from 144 kHz.
fn scale_for(sample_rate: u32) -> usize {
    if sample_rate < 72_000 {
        1
    } else if sample_rate < 144_000 {
        2
    } else {
        4
    }
}

/// Settings of a [`PitchTracker`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchConfig {
    /// Window RMS, after band-passing, below which a window is silent.
    pub silence_rms: f32,
}

impl Default for PitchConfig {
    /// A silence level of 0.002 RMS.
    fn default() -> Self {
        Self { silence_rms: 0.002 }
    }
}

/// Turns blocks of mono samples into timed pitch frames.
///
/// The tracker is not `Send`; it lives on the thread that creates it.
pub struct PitchTracker {
    config: PitchConfig,
    sample_rate: u32,
    /// Length of one analysis window in samples, [`WINDOW`] times the
    /// scale at `sample_rate`.
    window_len: usize,
    /// Distance in samples between the starts of consecutive windows,
    /// [`HOP`] times the scale at `sample_rate`.
    hop: usize,
    /// Highest reported frequency in Hz, at most a quarter of the sample
    /// rate.
    max_freq_hz: f32,
    filter: BandPass,
    detector: McLeodDetector<f32>,
    window: Vec<f32>,
}

impl PitchTracker {
    /// Creates a tracker for input at `sample_rate` Hz.
    ///
    /// Pitches above a quarter of the sample rate are not reported. At
    /// sample rates below 16 kHz a tone between a quarter and half of the
    /// sample rate can read one octave low.
    pub fn new(sample_rate: u32, config: PitchConfig) -> Self {
        let scale = scale_for(sample_rate);
        let window_len = WINDOW * scale;
        Self {
            config,
            sample_rate,
            window_len,
            hop: HOP * scale,
            max_freq_hz: MAX_FREQ_HZ.min(sample_rate as f32 / 4.0),
            filter: BandPass::new(sample_rate),
            detector: McLeodDetector::new(window_len, Self::padding(window_len)),
            window: Vec::with_capacity(window_len),
        }
    }

    /// Returns the sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the duration of one analysis window in seconds.
    pub fn window_s(&self) -> f64 {
        self.window_len as f64 / f64::from(self.sample_rate)
    }

    /// Zero padding of the detector's autocorrelation buffer, in samples,
    /// for a window of `window_len` samples.
    fn padding(window_len: usize) -> usize {
        window_len / 2
    }

    /// Samples from the last sample of a full window of `window_len` samples
    /// back to its centre, the sample at index `window_len / 2`.
    fn centre_offset(window_len: usize) -> usize {
        window_len - 1 - window_len / 2
    }

    /// Analyses `block`, whose first sample has engine-clock time
    /// `start_time_s`, and appends one frame per completed window to `out`.
    ///
    /// The samples continue those of earlier blocks without a gap; after a
    /// gap, call [`reset`](Self::reset) first.
    pub fn process(&mut self, block: &[f32], start_time_s: f64, out: &mut Vec<PitchFrame>) {
        let rate = f64::from(self.sample_rate);
        for (index, &sample) in block.iter().enumerate() {
            self.window.push(self.filter.run(sample));
            if self.window.len() == self.window_len {
                // The window ends at this sample; its centre sample lies
                // centre_offset samples earlier.
                let end_time_s = start_time_s + index as f64 / rate;
                let time_s = end_time_s - Self::centre_offset(self.window_len) as f64 / rate;
                out.push(self.analyse(time_s));
                self.window.drain(..self.hop);
            }
        }
    }

    /// Discards the partial window and the filter state.
    pub fn reset(&mut self) {
        self.window.clear();
        self.filter.reset();
    }

    /// Analyses the full window as the frame at `time_s`.
    fn analyse(&mut self, time_s: f64) -> PitchFrame {
        let unvoiced = |confidence| PitchFrame {
            time_s,
            freq_hz: None,
            confidence,
        };
        let silence = self.config.silence_rms;
        if rms(&self.window) < silence {
            return unvoiced(0.0);
        }
        // The detector compares this with the window's sum of squares, which
        // is the silence level squared times the window length at exactly
        // that RMS.
        let power_threshold = silence * silence * self.window_len as f32;
        match self.detector.get_pitch(
            &self.window,
            self.sample_rate as usize,
            power_threshold,
            CLARITY_THRESHOLD,
        ) {
            Some(pitch)
                if pitch.frequency.is_finite()
                    && (MIN_FREQ_HZ..=self.max_freq_hz).contains(&pitch.frequency) =>
            {
                PitchFrame {
                    time_s,
                    freq_hz: Some(pitch.frequency),
                    confidence: pitch.clarity,
                }
            }
            other => unvoiced(other.map_or(0.0, |pitch| pitch.clarity)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::filter::tests::{Noise, sine};
    use crate::music::assert_close;
    use crate::music::note::from_freq;

    /// Tracks `signal` in blocks of 300 samples starting at `start_time_s`.
    fn track(signal: &[f32], sample_rate: u32, start_time_s: f64) -> Vec<PitchFrame> {
        let mut tracker = PitchTracker::new(sample_rate, PitchConfig::default());
        let mut frames = Vec::new();
        for (index, block) in signal.chunks(300).enumerate() {
            let time_s = start_time_s + (index * 300) as f64 / f64::from(sample_rate);
            tracker.process(block, time_s, &mut frames);
        }
        frames
    }

    fn assert_all_near(frames: &[PitchFrame], freq_hz: f32) {
        assert!(!frames.is_empty());
        for frame in frames {
            let detected = frame
                .freq_hz
                .unwrap_or_else(|| panic!("unvoiced {frame:?}"));
            let error = (detected - freq_hz).abs() / freq_hz;
            assert!(error < 0.01, "{detected} Hz for {freq_hz} Hz");
        }
    }

    #[test]
    fn sines_are_detected_within_one_percent() {
        for rate in [44_100, 48_000] {
            for freq in [523.25, 1046.5, 2093.0] {
                let frames = track(&sine(freq, 0.5, rate, rate as usize / 2), rate, 0.0);
                assert_all_near(&frames, freq);
                assert!(frames.iter().all(|f| f.confidence > 0.9));
            }
        }
    }

    #[test]
    fn silence_and_quiet_noise_are_unvoiced() {
        let silent = track(&vec![0.0; 24_000], 48_000, 0.0);
        assert!(!silent.is_empty());
        assert!(silent.iter().all(|f| f.freq_hz.is_none()));

        let mut noise = Noise(7);
        let quiet: Vec<f32> = (0..24_000).map(|_| 0.001 * noise.next()).collect();
        assert!(
            track(&quiet, 48_000, 0.0)
                .iter()
                .all(|f| f.freq_hz.is_none())
        );
    }

    #[test]
    fn a_sine_in_noise_at_20_db_snr_is_detected() {
        // Uniform noise in -a..a has RMS a / sqrt(3); a sine of amplitude A
        // has RMS A / sqrt(2). 20 dB SNR is an RMS ratio of 10.
        let amplitude = 0.5;
        let noise_peak = amplitude / std::f32::consts::SQRT_2 / 10.0 * 3f32.sqrt();
        let mut noise = Noise(12345);
        let signal: Vec<f32> = sine(1046.5, amplitude, 48_000, 24_000)
            .into_iter()
            .map(|s| s + noise_peak * noise.next())
            .collect();
        assert_all_near(&track(&signal, 48_000, 0.0), 1046.5);
    }

    #[test]
    fn frequencies_outside_the_range_are_unvoiced() {
        for freq in [250.0, 4500.0] {
            let frames = track(&sine(freq, 0.5, 48_000, 24_000), 48_000, 0.0);
            assert!(
                frames.iter().all(|f| f.freq_hz.is_none()),
                "{freq} Hz is reported"
            );
        }
    }

    #[test]
    fn a_short_burst_gives_a_short_voiced_run() {
        // 20 ms of 1 kHz between 0.5 s of silence on each side.
        let rate = 48_000;
        let mut signal = vec![0.0; rate as usize / 2];
        signal.extend(sine(1000.0, 0.5, rate, rate as usize / 50));
        signal.extend(vec![0.0; rate as usize / 2]);

        let frames = track(&signal, rate, 0.0);
        let voiced: Vec<&PitchFrame> = frames.iter().filter(|f| f.freq_hz.is_some()).collect();
        let hop_s = HOP as f64 / f64::from(rate);
        let voiced_s = voiced.len() as f64 * hop_s;
        assert!(!voiced.is_empty());
        assert!(voiced_s < 0.08, "voiced for {voiced_s} s");
        // The voiced frames form one run around the burst.
        for pair in voiced.windows(2) {
            assert_close(pair[1].time_s - pair[0].time_s, hop_s, 1e-9);
        }
    }

    #[test]
    fn frames_are_stamped_at_the_window_centre() {
        let rate = 48_000;
        let frames = track(&sine(1000.0, 0.5, rate, 4096), rate, 10.0);
        let r = f64::from(rate);
        // Windows start at samples 0, 512, ..., 2048: five full windows.
        assert_eq!(frames.len(), 5);
        assert_close(frames[0].time_s, 10.0 + 1024.0 / r, 1e-9);
        assert_close(frames[4].time_s, 10.0 + (4.0 * 512.0 + 1024.0) / r, 1e-9);
    }

    #[test]
    fn reset_restarts_the_window() {
        let rate = 48_000;
        let mut tracker = PitchTracker::new(rate, PitchConfig::default());
        let mut frames = Vec::new();
        tracker.process(&sine(1000.0, 0.5, rate, 2000), 0.0, &mut frames);
        tracker.reset();
        tracker.process(&sine(1000.0, 0.5, rate, 2047), 1.0, &mut frames);
        assert!(frames.is_empty());
        tracker.process(&[0.0], 1.0 + 2047.0 / f64::from(rate), &mut frames);
        assert_eq!(frames.len(), 1);
        assert_close(frames[0].time_s, 1.0 + 1024.0 / f64::from(rate), 1e-9);
        assert_eq!(tracker.window_s(), 2048.0 / 48_000.0);
        assert_eq!(tracker.sample_rate(), rate);
    }

    #[test]
    fn pitches_above_a_quarter_of_the_sample_rate_are_unvoiced() {
        // 3500 Hz is above a quarter of 8000 Hz.
        let rate = 8_000;
        let frames = track(&sine(3500.0, 0.3, rate, rate as usize), rate, 0.0);
        assert!(!frames.is_empty());
        assert!(
            frames.iter().all(|f| f.freq_hz.is_none()),
            "3500 Hz at 8 kHz is reported"
        );
    }

    #[test]
    fn a_1600_hz_sine_at_8_khz_is_detected_within_ten_cents() {
        let rate = 8_000;
        let frames = track(&sine(1600.0, 0.3, rate, rate as usize), rate, 0.0);
        assert!(!frames.is_empty());
        for frame in &frames {
            let detected = frame
                .freq_hz
                .unwrap_or_else(|| panic!("unvoiced {frame:?}"));
            let cents = 1200.0 * (detected / 1600.0).log2();
            assert!(
                cents.abs() <= 10.0,
                "{detected} Hz is {cents} cents from 1600 Hz"
            );
        }
    }

    #[test]
    fn a_450_hz_sine_at_192_khz_reads_a4() {
        let rate = 192_000;
        let frames = track(&sine(450.0, 0.3, rate, rate as usize), rate, 0.0);
        let voiced: Vec<&PitchFrame> = frames.iter().filter(|f| f.freq_hz.is_some()).collect();
        assert!(voiced.len() >= 10, "only {} voiced frames", voiced.len());
        let a4 = voiced
            .iter()
            .filter(|frame| from_freq(frame.freq_hz.unwrap()).map(|note| note.midi) == Some(69))
            .count();
        assert!(
            a4 * 100 >= voiced.len() * 95,
            "{a4} of {} voiced frames read A4",
            voiced.len()
        );
    }

    #[test]
    fn window_s_scales_the_window_with_the_sample_rate() {
        let tracker = PitchTracker::new(96_000, PitchConfig::default());
        assert_eq!(tracker.window_s(), 2048.0 * 2.0 / 96_000.0);
    }
}
