//! A sample-accurate metronome that renders decaying sine clicks.
//!
//! [`Metronome`] synthesizes clicks whose amplitude decays exponentially,
//! accents the first beat of every bar with a higher frequency, schedules
//! the beats sample-accurately inside the output render callback and
//! reports each beat with its engine-clock time.

use crate::audio::backend::OutputBlockInfo;

/// Beats per bar; the first beat of each bar is accented.
pub const BEATS_PER_BAR: u32 = 4;
/// Click length in seconds.
pub const CLICK_S: f64 = 0.020;
/// Frequency of accented clicks in Hz.
pub const ACCENT_HZ: f32 = 1500.0;
/// Frequency of other clicks in Hz.
pub const BEAT_HZ: f32 = 1000.0;
/// Lowest accepted tempo in BPM.
pub const MIN_BPM: f64 = 20.0;
/// Highest accepted tempo in BPM.
pub const MAX_BPM: f64 = 300.0;

/// Peak amplitude of a click.
const CLICK_AMPLITUDE: f32 = 0.5;
/// Tolerance in samples when a beat position is rounded up to a sample.
const POSITION_EPSILON: f64 = 1e-6;

/// What a reported beat belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeatPhase {
    /// A count-in beat before a recording.
    CountIn,
    /// A beat played during a recording.
    Recording,
    /// A beat of the free-running sequence.
    Free,
}

/// One beat of the metronome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beat {
    /// Index of the beat within its sequence, starting at 0.
    pub index: u32,
    /// Whether the beat is accented, which the first beat of a bar is.
    pub accent: bool,
    /// What the beat belongs to.
    pub phase: BeatPhase,
    /// Engine-clock time at which the beat plays, in seconds.
    pub time_s: f64,
    /// Whether the beat's click is audible.
    pub audible: bool,
}

/// A command that changes the metronome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetronomeCommand {
    /// Sets the tempo and the on/off state of the free-running sequence.
    SetFree {
        /// Tempo in BPM, clamped into [`MIN_BPM`]..=[`MAX_BPM`].
        bpm: f64,
        /// Whether the free-running sequence runs.
        enabled: bool,
    },
}

/// A ringing click.
struct Click {
    /// Frequency of the click in Hz.
    freq_hz: f32,
    /// Index of the next click sample.
    n: usize,
}

/// A metronome that renders beat clicks into output blocks.
///
/// The free-running sequence plays a click every `60 / bpm` seconds and
/// accents the first beat of each bar.
pub struct Metronome {
    /// Whether a free sequence is running.
    running: bool,
    /// Tempo of the free sequence in BPM.
    free_bpm: f64,
    /// Whether the free sequence is enabled.
    free_enabled: bool,
    /// Index of the next beat.
    index: u32,
    /// Whether the next beat fires at the start of the next block.
    start_pending: bool,
    /// Exact sample position of the last fired beat.
    last_beat: f64,
    /// Absolute sample index of the next block's first sample, starting at 0.
    position: u64,
    /// The ringing click.
    click: Option<Click>,
}

impl Default for Metronome {
    /// Same as [`new`](Self::new).
    fn default() -> Self {
        Self::new()
    }
}

impl Metronome {
    /// Creates a stopped metronome at 120 BPM.
    pub fn new() -> Self {
        Self {
            running: false,
            free_bpm: 120.0,
            free_enabled: false,
            index: 0,
            start_pending: false,
            last_beat: 0.0,
            position: 0,
            click: None,
        }
    }

    /// Applies `command`; a tempo that is not finite is ignored.
    pub fn apply(&mut self, command: MetronomeCommand) {
        let MetronomeCommand::SetFree { bpm, enabled } = command;
        if !bpm.is_finite() {
            return;
        }
        self.free_bpm = bpm.clamp(MIN_BPM, MAX_BPM);
        self.free_enabled = enabled;
        if !enabled {
            self.running = false;
        } else if !self.running {
            self.running = true;
            self.index = 0;
            self.start_pending = true;
        }
    }

    /// Renders one output block and reports its beats through `on_beat`.
    ///
    /// `out` arrives zero-filled and receives the ringing clicks; `info`
    /// times the first sample of the block on the engine clock. Samples
    /// before a beat belong to the earlier click and samples from the beat
    /// on belong to the new click, which continues across blocks. Never
    /// allocates, locks or blocks.
    pub fn render(
        &mut self,
        out: &mut [f32],
        info: OutputBlockInfo,
        mut on_beat: impl FnMut(Beat),
    ) {
        let block_start = self.position as f64;
        let rate = f64::from(info.sample_rate);
        let interval = 60.0 * rate / self.free_bpm;
        let block_end = block_start + out.len() as f64;
        let click_len = (CLICK_S * rate).round() as usize;
        let tau = CLICK_S / 100.0_f64.ln();
        for (i, sample) in out.iter_mut().enumerate() {
            let here = block_start + i as f64;
            while self.running {
                let next = if self.start_pending {
                    block_start
                } else {
                    (self.last_beat + interval).max(block_start)
                };
                let fire = (next - POSITION_EPSILON).ceil();
                if fire >= block_end || fire > here {
                    break;
                }
                self.last_beat = next;
                self.start_pending = false;
                let accent = self.index.is_multiple_of(BEATS_PER_BAR);
                on_beat(Beat {
                    index: self.index,
                    accent,
                    phase: BeatPhase::Free,
                    time_s: info.time_s + (fire - block_start) / rate,
                    audible: true,
                });
                self.click = Some(Click {
                    freq_hz: if accent { ACCENT_HZ } else { BEAT_HZ },
                    n: 0,
                });
                self.index += 1;
            }
            if let Some(mut click) = self.click.take() {
                let t = click.n as f64 / rate;
                let wave = (-t / tau).exp()
                    * (2.0 * std::f64::consts::PI * f64::from(click.freq_hz) * t).sin();
                *sample += (CLICK_AMPLITUDE as f64 * wave) as f32;
                click.n += 1;
                if click.n < click_len {
                    self.click = Some(click);
                }
            }
        }
        self.position += out.len() as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::assert_close;

    #[test]
    fn free_mode_at_120_bpm_fires_every_half_second() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        });
        let mut output = vec![0.0_f32; 96_000];
        let mut beats = Vec::new();
        for (block_index, block) in output.chunks_mut(256).enumerate() {
            let start = block_index * 256;
            let info = OutputBlockInfo {
                time_s: start as f64 / f64::from(rate),
                sample_rate: rate,
            };
            metronome.render(block, info, |beat| beats.push(beat));
        }
        assert_eq!(beats.len(), 4);
        for (k, beat) in beats.iter().enumerate() {
            assert_close(beat.time_s, 0.5 * k as f64, 1e-9);
            assert_eq!(beat.index, k as u32);
            assert_eq!(beat.phase, BeatPhase::Free);
            assert_eq!(beat.accent, k == 0);
            assert!(beat.audible);
        }
        assert!(output[1] != 0.0);
        assert_eq!(output[23_999], 0.0);
        assert!(output[24_001] != 0.0);
    }
}
