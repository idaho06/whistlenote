//! A sample-accurate metronome that renders decaying sine clicks.
//!
//! [`Metronome`] synthesizes clicks whose amplitude decays exponentially,
//! accents the first beat of every bar with a higher frequency, schedules
//! the beats sample-accurately inside the output render callback and
//! reports each beat with its engine-clock time. The volume and the two
//! frequencies of the clicks come from a [`ClickSound`], which the control
//! sets with [`MetronomeCommand::SetClick`].
//!
//! Two sequences share the renderer. The free-running sequence plays a
//! click every `60 / bpm` seconds. A recording starts with one count-in
//! bar of [`BEATS_PER_BAR`] beats, accented on its first beat and always
//! audible, and continues with recording beats that are audible only when
//! clicks are requested; the first recording beat is the recording
//! origin.
//!
//! [`metronome`] returns the metronome as an output source together with a
//! [`MetronomeControl`] that sends commands to it and receives its beats
//! without locks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::audio::backend::{OutputBlockInfo, OutputSource};

/// Beats per bar; the first beat of each bar is accented.
pub const BEATS_PER_BAR: u32 = 4;
/// Click length in seconds.
pub const CLICK_S: f64 = 0.020;
/// Default of [`ClickSound::accent_hz`]: frequency of accented clicks in Hz.
pub const ACCENT_HZ: f32 = 1500.0;
/// Default of [`ClickSound::beat_hz`]: frequency of other clicks in Hz.
pub const BEAT_HZ: f32 = 1000.0;
/// Lowest accepted tempo in BPM.
pub const MIN_BPM: f64 = 20.0;
/// Highest accepted tempo in BPM.
pub const MAX_BPM: f64 = 300.0;

/// Default of [`ClickSound::volume`]: peak amplitude of a click.
const CLICK_AMPLITUDE: f32 = 0.5;
/// Tolerance in samples when a beat position is rounded up to a sample.
const POSITION_EPSILON: f64 = 1e-6;

/// The sound of the metronome clicks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickSound {
    /// Peak amplitude of a click in 0.0..=1.0.
    pub volume: f32,
    /// Frequency of accented clicks in Hz.
    pub accent_hz: f32,
    /// Frequency of other clicks in Hz.
    pub beat_hz: f32,
}

impl Default for ClickSound {
    /// A volume of `CLICK_AMPLITUDE` (0.5), an accent of [`ACCENT_HZ`]
    /// (1500 Hz) and a beat of [`BEAT_HZ`] (1000 Hz).
    fn default() -> Self {
        Self {
            volume: CLICK_AMPLITUDE,
            accent_hz: ACCENT_HZ,
            beat_hz: BEAT_HZ,
        }
    }
}

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
    /// Starts a recording: one count-in bar, then recording beats.
    StartRecording {
        /// Tempo in BPM, clamped into [`MIN_BPM`]..=[`MAX_BPM`].
        bpm: f64,
        /// Whether the recording beats are audible.
        clicks: bool,
    },
    /// Sets the click sound; a ringing click keeps its frequency.
    SetClick(ClickSound),
    /// Ends a count-in or recording.
    StopRecording,
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
/// The free-running and recording sequences it plays are described in the
/// [module documentation](crate::metronome).
pub struct Metronome {
    /// The sequence that plays, or `None` while idle.
    mode: Option<BeatPhase>,
    /// Tempo of the free sequence in BPM.
    free_bpm: f64,
    /// Whether the free sequence is enabled.
    free_enabled: bool,
    /// Tempo of the count-in and recording beats in BPM.
    record_bpm: f64,
    /// Whether the recording beats are audible.
    clicks: bool,
    /// Index of the next beat.
    index: u32,
    /// Exact sample position of the last fired beat; negative infinity
    /// makes the next beat fire at the start of the next block.
    last_beat: f64,
    /// Absolute sample index of the next block's first sample, starting at 0.
    position: u64,
    /// The sound of the clicks.
    sound: ClickSound,
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
            mode: None,
            free_bpm: 120.0,
            free_enabled: false,
            record_bpm: 120.0,
            clicks: false,
            index: 0,
            last_beat: f64::NEG_INFINITY,
            position: 0,
            sound: ClickSound::default(),
            click: None,
        }
    }

    /// Applies `command`; a tempo that is not finite is ignored, a click
    /// volume that is not finite is ignored and an accepted one is clamped
    /// into 0.0..=1.0, and a click frequency that is not finite or not
    /// positive keeps the old value.
    pub fn apply(&mut self, command: MetronomeCommand) {
        match command {
            MetronomeCommand::SetFree { bpm, enabled } => {
                if !bpm.is_finite() {
                    return;
                }
                self.free_bpm = bpm.clamp(MIN_BPM, MAX_BPM);
                self.free_enabled = enabled;
                if matches!(self.mode, None | Some(BeatPhase::Free)) {
                    if !enabled {
                        self.mode = None;
                    } else if self.mode.is_none() {
                        self.start(BeatPhase::Free);
                    }
                }
            }
            MetronomeCommand::StartRecording { bpm, clicks } => {
                if !bpm.is_finite() {
                    return;
                }
                self.record_bpm = bpm.clamp(MIN_BPM, MAX_BPM);
                self.clicks = clicks;
                self.start(BeatPhase::CountIn);
            }
            MetronomeCommand::SetClick(sound) => {
                if sound.volume.is_finite() {
                    self.sound.volume = sound.volume.clamp(0.0, 1.0);
                }
                if sound.accent_hz.is_finite() && sound.accent_hz > 0.0 {
                    self.sound.accent_hz = sound.accent_hz;
                }
                if sound.beat_hz.is_finite() && sound.beat_hz > 0.0 {
                    self.sound.beat_hz = sound.beat_hz;
                }
            }
            MetronomeCommand::StopRecording => {
                // The free sequence continues the grid from the last beat.
                if matches!(self.mode, Some(BeatPhase::CountIn | BeatPhase::Recording)) {
                    self.mode = self.free_enabled.then_some(BeatPhase::Free);
                    self.index = 0;
                }
            }
        }
    }

    /// Renders one output block and reports its beats through `on_beat`.
    ///
    /// `out` arrives zero-filled and receives the ringing clicks; `info`
    /// times the first sample of the block on the engine clock. Samples
    /// before a beat belong to the earlier click and samples from the beat
    /// on belong to the new click, which continues across blocks. A beat
    /// whose click is not audible leaves a ringing click alone. Never
    /// allocates, locks or blocks.
    pub fn render(
        &mut self,
        out: &mut [f32],
        info: OutputBlockInfo,
        mut on_beat: impl FnMut(Beat),
    ) {
        let block_start = self.position as f64;
        let rate = f64::from(info.sample_rate);
        let click_len = (CLICK_S * rate).round() as usize;
        let tau = CLICK_S / 100.0_f64.ln();
        let mut next = self.next_beat(block_start, rate);
        for (i, sample) in out.iter_mut().enumerate() {
            let here = block_start + i as f64;
            while let Some((exact, fire, phase)) = next.filter(|&(_, fire, _)| fire <= here) {
                self.last_beat = exact;
                let index = self.index;
                let (accent, audible) = self.advance_beat(phase);
                next = self.next_beat(block_start, rate);
                on_beat(Beat {
                    index,
                    accent,
                    phase,
                    time_s: info.time_s + (fire - block_start) / rate,
                    audible,
                });
                if audible {
                    self.click = Some(Click {
                        freq_hz: if accent {
                            self.sound.accent_hz
                        } else {
                            self.sound.beat_hz
                        },
                        n: 0,
                    });
                }
            }
            if let Some(mut click) = self.click.take() {
                let t = click.n as f64 / rate;
                let wave = (-t / tau).exp()
                    * (2.0 * std::f64::consts::PI * f64::from(click.freq_hz) * t).sin();
                *sample += (self.sound.volume as f64 * wave) as f32;
                click.n += 1;
                if click.n < click_len {
                    self.click = Some(click);
                }
            }
        }
        self.position += out.len() as u64;
    }

    /// Starts `phase` with its first beat at the start of the next block.
    fn start(&mut self, phase: BeatPhase) {
        self.mode = Some(phase);
        self.index = 0;
        self.last_beat = f64::NEG_INFINITY;
    }

    /// Returns the exact position, the firing sample and the phase of the
    /// next beat, or `None` while idle; the beat is no earlier than
    /// `block_start`.
    fn next_beat(&self, block_start: f64, rate: f64) -> Option<(f64, f64, BeatPhase)> {
        let phase = self.mode?;
        let bpm = match phase {
            BeatPhase::Free => self.free_bpm,
            BeatPhase::CountIn | BeatPhase::Recording => self.record_bpm,
        };
        let exact = (self.last_beat + 60.0 * rate / bpm).max(block_start);
        Some((exact, (exact - POSITION_EPSILON).ceil(), phase))
    }

    /// Advances past one beat of `phase` and returns its accent and
    /// audibility.
    ///
    /// A count-in ends after [`BEATS_PER_BAR`] beats, and the recording
    /// starts at the next beat.
    fn advance_beat(&mut self, phase: BeatPhase) -> (bool, bool) {
        let index = self.index;
        self.index += 1;
        match phase {
            BeatPhase::Free => (index.is_multiple_of(BEATS_PER_BAR), true),
            BeatPhase::CountIn => {
                // The recording beats continue the count-in's grid.
                if self.index >= BEATS_PER_BAR {
                    self.mode = Some(BeatPhase::Recording);
                    self.index = 0;
                }
                (index == 0, true)
            }
            BeatPhase::Recording => (index.is_multiple_of(BEATS_PER_BAR), self.clicks),
        }
    }
}

/// Capacity of the command ring.
const COMMAND_CAPACITY: usize = 64;

/// Capacity of the beat ring.
const BEAT_CAPACITY: usize = 256;

/// A lock-free handle that controls a metronome running as an output source.
///
/// The handle is `Send` and lives on the thread that drives the engine. It
/// sends commands to the render callback on the audio thread and receives
/// the beats that the metronome plays, without locks, allocation or
/// blocking.
pub struct MetronomeControl {
    commands: rtrb::Producer<MetronomeCommand>,
    beats: rtrb::Consumer<Beat>,
    dropped: Arc<AtomicU64>,
}

impl MetronomeControl {
    /// Queues `command` for the next rendered block; returns false when the
    /// queue is full and the command is lost.
    pub fn send(&mut self, command: MetronomeCommand) -> bool {
        self.commands.push(command).is_ok()
    }

    /// Returns the next rendered beat, if any, in the order they play.
    pub fn try_recv(&mut self) -> Option<Beat> {
        self.beats.pop().ok()
    }

    /// Returns the number of beats lost because they were not received in
    /// time.
    pub fn dropped_beats(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Creates a metronome as an output source and the handle that controls it.
///
/// The source owns the metronome and runs on the audio thread: each call
/// applies every queued command, then renders one block of clicks and
/// forwards every beat to the handle. A call never allocates, locks or
/// blocks.
pub fn metronome() -> (MetronomeControl, OutputSource) {
    let (command_producer, mut commands) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
    let (mut beats, beat_consumer) = rtrb::RingBuffer::new(BEAT_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let control = MetronomeControl {
        commands: command_producer,
        beats: beat_consumer,
        dropped: Arc::clone(&dropped),
    };
    let mut metronome = Metronome::new();
    let source: OutputSource = Box::new(move |out, info| {
        while let Ok(command) = commands.pop() {
            metronome.apply(command);
        }
        metronome.render(out, info, |beat| {
            if beats.push(beat).is_err() {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        });
    });
    (control, source)
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
        let mut beats = Vec::new();
        let output = render_span(&mut metronome, 0, 96_000, rate, &mut beats);
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

    /// Renders samples `from`..`to` in blocks of 256 and returns them.
    fn render_span(
        metronome: &mut Metronome,
        from: usize,
        to: usize,
        rate: u32,
        beats: &mut Vec<Beat>,
    ) -> Vec<f32> {
        render_span_in_blocks(metronome, from, to, rate, 256, beats)
    }

    /// Renders samples `from`..`to` in blocks of `block_size` and returns them.
    fn render_span_in_blocks(
        metronome: &mut Metronome,
        from: usize,
        to: usize,
        rate: u32,
        block_size: usize,
        beats: &mut Vec<Beat>,
    ) -> Vec<f32> {
        let mut output = vec![0.0_f32; to - from];
        let mut start = from;
        while start < to {
            let len = (to - start).min(block_size);
            let info = OutputBlockInfo {
                time_s: start as f64 / f64::from(rate),
                sample_rate: rate,
            };
            metronome.render(
                &mut output[start - from..start - from + len],
                info,
                |beat| beats.push(beat),
            );
            start += len;
        }
        output
    }

    /// Returns the absolute sample index at which `beat` plays.
    fn sample_of(beat: &Beat, rate: u32) -> usize {
        (beat.time_s * f64::from(rate)).round() as usize
    }

    #[test]
    fn count_in_then_silent_recording_beats() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::StartRecording {
            bpm: 120.0,
            clicks: false,
        });
        let mut beats = Vec::new();
        let output = render_span(&mut metronome, 0, 144_000, rate, &mut beats);
        assert_eq!(beats.len(), 6);
        let expected = [
            (0.0, 0, BeatPhase::CountIn, true, true),
            (0.5, 1, BeatPhase::CountIn, false, true),
            (1.0, 2, BeatPhase::CountIn, false, true),
            (1.5, 3, BeatPhase::CountIn, false, true),
            (2.0, 0, BeatPhase::Recording, true, false),
            (2.5, 1, BeatPhase::Recording, false, false),
        ];
        for (beat, &(time_s, index, phase, accent, audible)) in beats.iter().zip(expected.iter()) {
            assert_close(beat.time_s, time_s, 1e-9);
            assert_eq!(beat.index, index);
            assert_eq!(beat.phase, phase);
            assert_eq!(beat.accent, accent);
            assert_eq!(beat.audible, audible);
        }
        assert!(output[72_001] != 0.0);
        assert!(output[72_000 + 960..].iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn stop_recording_continues_the_free_grid() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        });
        metronome.apply(MetronomeCommand::StartRecording {
            bpm: 120.0,
            clicks: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 108_000, rate, &mut beats);
        assert_eq!(beats.len(), 5);
        for (k, beat) in beats.iter().enumerate() {
            assert_close(beat.time_s, 0.5 * k as f64, 1e-9);
            assert!(beat.audible);
            if k < 4 {
                assert_eq!(beat.phase, BeatPhase::CountIn);
                assert_eq!(beat.index, k as u32);
                assert_eq!(beat.accent, k == 0);
            } else {
                assert_eq!(beat.phase, BeatPhase::Recording);
                assert_eq!(beat.index, 0);
                assert!(beat.accent);
            }
        }
        metronome.apply(MetronomeCommand::StopRecording);
        let _ = render_span(&mut metronome, 108_000, 172_800, rate, &mut beats);
        assert_eq!(beats.len(), 8);
        for (k, beat) in beats[5..].iter().enumerate() {
            assert_close(beat.time_s, 2.5 + 0.5 * k as f64, 1e-9);
            assert_eq!(beat.phase, BeatPhase::Free);
            assert_eq!(beat.index, k as u32);
            assert_eq!(beat.accent, k == 0);
            assert!(beat.audible);
        }
    }

    #[test]
    fn stop_recording_without_free_goes_idle() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::StartRecording {
            bpm: 120.0,
            clicks: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 30_000, rate, &mut beats);
        assert_eq!(beats.len(), 2);
        metronome.apply(MetronomeCommand::StopRecording);
        let _ = render_span(&mut metronome, 30_000, 96_000, rate, &mut beats);
        assert_eq!(beats.len(), 2);
    }

    #[test]
    fn control_drives_the_source_through_the_fake_backend() {
        use crate::audio::backend::AudioBackend;
        use crate::audio::fake::{FakeConfig, fake_backend};

        let (backend, handle) = fake_backend(FakeConfig::default());
        let (mut control, source) = metronome();
        let _stream = backend.open_output(None, source, Box::new(|_| {})).unwrap();
        assert!(control.send(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        }));
        handle.step_n(375); // 96_000 samples, 2 s
        let mut beats = Vec::new();
        while let Some(beat) = control.try_recv() {
            beats.push(beat);
        }
        assert_eq!(beats.len(), 4);
        for (k, beat) in beats.iter().enumerate() {
            assert_close(beat.time_s, 0.5 * k as f64, 1e-9);
            assert_eq!(beat.phase, BeatPhase::Free);
        }
        let output = handle.take_output();
        assert_eq!(output.len(), 96_000);
        assert!(output[1] != 0.0);
        assert_eq!(output[23_999], 0.0);
        assert_eq!(control.dropped_beats(), 0);
    }

    #[test]
    fn beats_beyond_the_ring_capacity_are_counted_as_dropped() {
        let (mut control, mut source) = metronome();
        assert!(control.send(MetronomeCommand::SetFree {
            bpm: 300.0,
            enabled: true,
        }));
        let mut block = vec![0.0_f32; 48_000 * 60]; // 60 s
        source(
            &mut block,
            OutputBlockInfo {
                time_s: 0.0,
                sample_rate: 48_000,
            },
        );
        assert_eq!(control.dropped_beats(), 44);
        let mut beats = Vec::new();
        while let Some(beat) = control.try_recv() {
            beats.push(beat);
        }
        assert_eq!(beats.len(), 256);
        assert_eq!(beats[0].index, 0);
    }

    #[test]
    fn a_full_command_queue_rejects_commands() {
        let (mut control, _source) = metronome();
        for _ in 0..64 {
            assert!(control.send(MetronomeCommand::StopRecording));
        }
        assert!(!control.send(MetronomeCommand::StopRecording));
    }

    #[test]
    fn free_mode_at_44_1_khz_fires_every_22050_samples() {
        let rate = 44_100;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 88_200, rate, &mut beats);
        assert_eq!(beats.len(), 4);
        let samples = [0, 22_050, 44_100, 66_150];
        let times = [0.0, 0.5, 1.0, 1.5];
        for (k, beat) in beats.iter().enumerate() {
            assert_eq!(sample_of(beat, rate), samples[k]);
            assert_close(beat.time_s, times[k], 1e-9);
        }
    }

    #[test]
    fn output_does_not_depend_on_the_block_size() {
        let rate = 44_100;
        let total = 286_650; // 6.5 s
        let render = |block_size: usize| {
            let mut metronome = Metronome::new();
            metronome.apply(MetronomeCommand::StartRecording {
                bpm: 130.0,
                clicks: true,
            });
            let mut beats = Vec::new();
            let output =
                render_span_in_blocks(&mut metronome, 0, total, rate, block_size, &mut beats);
            (beats, output)
        };
        let (beats_256, output_256) = render(256);
        let (beats_300, output_300) = render(300);
        let (beats_single, output_single) = render(total);
        // Beat times are the block time plus the offset in the block, so
        // they can differ in the last bit between block sizes.
        for other in [&beats_300, &beats_single] {
            assert_eq!(beats_256.len(), other.len());
            for (a, b) in beats_256.iter().zip(other.iter()) {
                assert_eq!(
                    (a.index, a.accent, a.phase, a.audible),
                    (b.index, b.accent, b.phase, b.audible)
                );
                assert_close(a.time_s, b.time_s, 1e-12);
            }
        }
        assert_eq!(output_256, output_300);
        assert_eq!(output_300, output_single);
        // One beat fires for every k whose rounded-up position lands inside the span.
        let expected = (0..)
            .take_while(|&k| ((k as f64 * 2_646_000.0 / 130.0) - 1e-6).ceil() < total as f64)
            .count();
        assert_eq!(expected, 15); // 4 count-in beats and 11 recording beats
        assert_eq!(beats_256.len(), expected);
        for (k, beat) in beats_256.iter().enumerate() {
            let position = ((k as f64 * 2_646_000.0 / 130.0) - 1e-6).ceil() as usize;
            assert_eq!(sample_of(beat, rate), position);
            if k < BEATS_PER_BAR as usize {
                assert_eq!(beat.phase, BeatPhase::CountIn);
                assert_eq!(beat.index, k as u32);
            } else {
                assert_eq!(beat.phase, BeatPhase::Recording);
                assert_eq!(beat.index, (k - BEATS_PER_BAR as usize) as u32);
            }
        }
    }

    #[test]
    fn a_non_integer_interval_does_not_drift() {
        let rate = 44_100;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 130.0,
            enabled: true,
        });
        let mut beats = Vec::new();
        let output = render_span(&mut metronome, 0, 269_010, rate, &mut beats);
        assert_eq!(beats.len(), 14);
        for (k, beat) in beats.iter().enumerate() {
            // The interval is 2_646_000 / 130 samples, which is not an integer.
            let expected = ((k as f64 * 2_646_000.0 / 130.0) - 1e-6).ceil() as usize;
            assert_eq!(sample_of(beat, rate), expected);
            assert_close(beat.time_s, expected as f64 / 44_100.0, 1e-9);
            if k >= 1 {
                // A click lasts 882 samples, far shorter than the interval.
                assert_eq!(output[expected - 1], 0.0);
                assert!(output[expected + 1] != 0.0);
            }
        }
        assert_eq!(sample_of(&beats[1], rate), 20_354);
        assert_eq!(sample_of(&beats[2], rate), 40_708);
        assert_eq!(sample_of(&beats[3], rate), 61_062);
        assert_eq!(sample_of(&beats[13], rate), 264_600);
    }

    #[test]
    fn a_bpm_change_applies_from_the_last_beat() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 30_000, rate, &mut beats);
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 60.0,
            enabled: true,
        });
        let _ = render_span(&mut metronome, 30_000, 130_000, rate, &mut beats);
        assert_eq!(beats.len(), 4);
        let samples = [0, 24_000, 72_000, 120_000];
        for (k, beat) in beats.iter().enumerate() {
            assert_eq!(sample_of(beat, rate), samples[k]);
            assert_eq!(beat.index, k as u32);
        }
        assert_close(beats[2].time_s, 1.5, 1e-9);
        assert_close(beats[3].time_s, 2.5, 1e-9);
        assert!(!beats[2].accent);
        assert!(!beats[3].accent);
    }

    #[test]
    fn a_late_bpm_change_starts_at_the_block() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 40_000, rate, &mut beats);
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 240.0,
            enabled: true,
        });
        let _ = render_span(&mut metronome, 40_000, 70_000, rate, &mut beats);
        assert_eq!(beats.len(), 5);
        let samples = [0, 24_000, 40_000, 52_000, 64_000];
        for (k, beat) in beats.iter().enumerate() {
            assert_eq!(sample_of(beat, rate), samples[k]);
            assert_close(beat.time_s, samples[k] as f64 / f64::from(rate), 1e-9);
            assert_eq!(beat.index, k as u32);
        }
        assert!(!beats[2].accent);
        assert!(!beats[3].accent);
        assert!(beats[4].accent);
    }

    #[test]
    fn repeating_the_same_free_setting_does_not_restart() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 30_000, rate, &mut beats);
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        });
        let _ = render_span(&mut metronome, 30_000, 100_000, rate, &mut beats);
        assert_eq!(beats.len(), 5);
        let samples = [0, 24_000, 48_000, 72_000, 96_000];
        let accents = [true, false, false, false, true];
        for (k, beat) in beats.iter().enumerate() {
            assert_eq!(sample_of(beat, rate), samples[k]);
            assert_eq!(beat.index, k as u32);
            assert_eq!(beat.accent, accents[k]);
        }
    }

    #[test]
    fn non_finite_tempos_are_ignored_and_others_clamped() {
        let rate = 48_000;
        // Non-finite tempos change nothing, so the metronome stays idle.
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: f64::NAN,
            enabled: true,
        });
        metronome.apply(MetronomeCommand::SetFree {
            bpm: f64::INFINITY,
            enabled: true,
        });
        metronome.apply(MetronomeCommand::StartRecording {
            bpm: f64::NAN,
            clicks: true,
        });
        let mut beats = Vec::new();
        let output = render_span(&mut metronome, 0, 48_000, rate, &mut beats);
        assert!(beats.is_empty());
        assert!(output.iter().all(|&sample| sample == 0.0));

        // 1000 BPM is clamped to 300 BPM, one beat every 9_600 samples.
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 1000.0,
            enabled: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 48_000, rate, &mut beats);
        assert_eq!(beats.len(), 5);
        let samples = [0, 9_600, 19_200, 28_800, 38_400];
        for (beat, &sample) in beats.iter().zip(samples.iter()) {
            assert_eq!(sample_of(beat, rate), sample);
        }

        // 1 BPM is clamped to 20 BPM, one beat every 144_000 samples.
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 1.0,
            enabled: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 150_000, rate, &mut beats);
        assert_eq!(beats.len(), 2);
        assert_eq!(sample_of(&beats[0], rate), 0);
        assert_eq!(sample_of(&beats[1], rate), 144_000);
    }

    #[test]
    fn starting_a_recording_again_restarts_the_count_in() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::StartRecording {
            bpm: 120.0,
            clicks: true,
        });
        let mut beats = Vec::new();
        let _ = render_span(&mut metronome, 0, 60_000, rate, &mut beats);
        assert_eq!(beats.len(), 3);
        assert_eq!(sample_of(&beats[0], rate), 0);
        assert_eq!(sample_of(&beats[1], rate), 24_000);
        assert_eq!(sample_of(&beats[2], rate), 48_000);
        metronome.apply(MetronomeCommand::StartRecording {
            bpm: 60.0,
            clicks: false,
        });
        let _ = render_span(&mut metronome, 60_000, 300_000, rate, &mut beats);
        assert_eq!(beats.len(), 8);
        let expected = [
            (60_000, 0, BeatPhase::CountIn, true, true),
            (108_000, 1, BeatPhase::CountIn, false, true),
            (156_000, 2, BeatPhase::CountIn, false, true),
            (204_000, 3, BeatPhase::CountIn, false, true),
            (252_000, 0, BeatPhase::Recording, true, false),
        ];
        for (beat, &(sample, index, phase, accent, audible)) in
            beats[3..].iter().zip(expected.iter())
        {
            assert_eq!(sample_of(beat, rate), sample);
            assert_eq!(beat.index, index);
            assert_eq!(beat.phase, phase);
            assert_eq!(beat.accent, accent);
            assert_eq!(beat.audible, audible);
        }
    }

    #[test]
    fn a_silent_click_sound_renders_silence() {
        let rate = 48_000;
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetClick(ClickSound {
            volume: 0.0,
            ..Default::default()
        }));
        metronome.apply(MetronomeCommand::SetFree {
            bpm: 120.0,
            enabled: true,
        });
        let mut beats = Vec::new();
        let output = render_span(&mut metronome, 0, 96_000, rate, &mut beats);
        assert_eq!(beats.len(), 4);
        assert!(
            output.iter().all(|&sample| sample == 0.0),
            "a zero volume renders silence"
        );
    }

    #[test]
    fn the_volume_scales_the_click_peak() {
        let rate = 48_000;
        let render = |volume: f32| {
            let mut metronome = Metronome::new();
            metronome.apply(MetronomeCommand::SetClick(ClickSound {
                volume,
                ..Default::default()
            }));
            metronome.apply(MetronomeCommand::SetFree {
                bpm: 120.0,
                enabled: true,
            });
            let mut beats = Vec::new();
            render_span(&mut metronome, 0, 960, rate, &mut beats)
        };
        let peak = |output: &[f32]| {
            output
                .iter()
                .fold(0.0_f32, |peak, &sample| peak.max(sample.abs()))
        };
        let default_peak = peak(&render(ClickSound::default().volume));
        let quiet_peak = peak(&render(0.25));
        assert!(default_peak > 0.0, "the default sound renders a click");
        let relative = (quiet_peak - 0.5 * default_peak).abs() / default_peak;
        assert!(
            relative < 1e-4,
            "the peak {quiet_peak} is not half of {default_peak} (relative {relative})"
        );
    }

    #[test]
    fn the_accent_frequency_sets_the_first_click_pitch() {
        let rate = 48_000;
        let render = |accent_hz: f32| {
            let mut metronome = Metronome::new();
            metronome.apply(MetronomeCommand::SetClick(ClickSound {
                accent_hz,
                ..Default::default()
            }));
            metronome.apply(MetronomeCommand::SetFree {
                bpm: 120.0,
                enabled: true,
            });
            let mut beats = Vec::new();
            render_span(&mut metronome, 0, 960, rate, &mut beats)
        };
        // Sign changes of the click: samples whose sign differs from the
        // previous non-zero sample, ignoring exact zeros.
        let sign_changes = |output: &[f32]| {
            let mut changes = 0;
            let mut previous: Option<f32> = None;
            for &sample in output {
                if sample == 0.0 {
                    continue;
                }
                if previous.is_some_and(|previous| sample.signum() != previous.signum()) {
                    changes += 1;
                }
                previous = Some(sample);
            }
            changes
        };
        let at_3000_hz = sign_changes(&render(3000.0));
        assert!(
            (at_3000_hz as i64 - 120).abs() <= 4,
            "about 120 sign changes at 3000 Hz, got {at_3000_hz}"
        );
        let at_default = sign_changes(&render(ACCENT_HZ));
        assert!(
            (at_default as i64 - 60).abs() <= 4,
            "about 60 sign changes at {ACCENT_HZ} Hz, got {at_default}"
        );
    }

    #[test]
    fn invalid_click_values_are_ignored() {
        let mut metronome = Metronome::new();
        metronome.apply(MetronomeCommand::SetClick(ClickSound {
            volume: 0.3,
            accent_hz: 2000.0,
            beat_hz: 800.0,
        }));
        metronome.apply(MetronomeCommand::SetClick(ClickSound {
            volume: f32::NAN,
            accent_hz: -1.0,
            beat_hz: f32::INFINITY,
        }));
        assert_eq!(metronome.sound.volume, 0.3);
        assert_eq!(metronome.sound.accent_hz, 2000.0);
        assert_eq!(metronome.sound.beat_hz, 800.0);

        metronome.apply(MetronomeCommand::SetClick(ClickSound {
            volume: 2.0,
            ..Default::default()
        }));
        assert_eq!(metronome.sound.volume, 1.0);
    }
}
