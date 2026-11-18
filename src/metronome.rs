//! A sample-accurate metronome that renders decaying sine clicks.
//!
//! [`Metronome`] synthesizes clicks whose amplitude decays exponentially,
//! accents the first beat of every bar with a higher frequency, schedules
//! the beats sample-accurately inside the output render callback and
//! reports each beat with its engine-clock time.
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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::audio::backend::{OutputBlockInfo, OutputSource};

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
    /// Starts a recording: one count-in bar, then recording beats.
    StartRecording {
        /// Tempo in BPM, clamped into [`MIN_BPM`]..=[`MAX_BPM`].
        bpm: f64,
        /// Whether the recording beats are audible.
        clicks: bool,
    },
    /// Ends a count-in or recording.
    StopRecording,
}

/// What the metronome plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Nothing plays.
    Idle,
    /// The free-running sequence plays.
    Free,
    /// The count-in bar of a recording plays.
    CountIn,
    /// The recording beats play.
    Recording,
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
/// accents the first beat of each bar. A recording starts with one
/// count-in bar of [`BEATS_PER_BAR`] beats, accented on its first beat and
/// always audible, and continues with recording beats that are audible
/// only when clicks are requested; the first recording beat is the
/// recording origin.
pub struct Metronome {
    /// What the metronome plays.
    mode: Mode,
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
            mode: Mode::Idle,
            free_bpm: 120.0,
            free_enabled: false,
            record_bpm: 120.0,
            clicks: false,
            index: 0,
            start_pending: false,
            last_beat: 0.0,
            position: 0,
            click: None,
        }
    }

    /// Applies `command`; a tempo that is not finite is ignored.
    pub fn apply(&mut self, command: MetronomeCommand) {
        match command {
            MetronomeCommand::SetFree { bpm, enabled } => {
                if !bpm.is_finite() {
                    return;
                }
                self.free_bpm = bpm.clamp(MIN_BPM, MAX_BPM);
                self.free_enabled = enabled;
                if matches!(self.mode, Mode::Idle | Mode::Free) {
                    if !enabled {
                        self.mode = Mode::Idle;
                    } else if self.mode == Mode::Idle {
                        self.mode = Mode::Free;
                        self.index = 0;
                        self.start_pending = true;
                    }
                }
            }
            MetronomeCommand::StartRecording { bpm, clicks } => {
                if !bpm.is_finite() {
                    return;
                }
                self.record_bpm = bpm.clamp(MIN_BPM, MAX_BPM);
                self.clicks = clicks;
                self.mode = Mode::CountIn;
                self.index = 0;
                self.start_pending = true;
            }
            MetronomeCommand::StopRecording => {
                if matches!(self.mode, Mode::CountIn | Mode::Recording) {
                    if self.free_enabled {
                        self.mode = Mode::Free;
                        self.index = 0;
                    } else {
                        self.mode = Mode::Idle;
                    }
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
        let block_end = block_start + out.len() as f64;
        let click_len = (CLICK_S * rate).round() as usize;
        let tau = CLICK_S / 100.0_f64.ln();
        for (i, sample) in out.iter_mut().enumerate() {
            let here = block_start + i as f64;
            while self.mode != Mode::Idle {
                let interval = 60.0 * rate / self.current_bpm();
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
                let index = self.index;
                let (phase, accent, audible) = self.advance_beat();
                on_beat(Beat {
                    index,
                    accent,
                    phase,
                    time_s: info.time_s + (fire - block_start) / rate,
                    audible,
                });
                if audible {
                    self.click = Some(Click {
                        freq_hz: if accent { ACCENT_HZ } else { BEAT_HZ },
                        n: 0,
                    });
                }
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

    /// Tempo of the sequence that is running, in BPM.
    fn current_bpm(&self) -> f64 {
        match self.mode {
            Mode::Idle | Mode::Free => self.free_bpm,
            Mode::CountIn | Mode::Recording => self.record_bpm,
        }
    }

    /// Advances past one beat and returns its phase, accent and audibility.
    ///
    /// A count-in ends after [`BEATS_PER_BAR`] beats, and the recording
    /// starts at the next beat.
    fn advance_beat(&mut self) -> (BeatPhase, bool, bool) {
        let index = self.index;
        let (phase, accent, audible) = match self.mode {
            Mode::Free => (BeatPhase::Free, index.is_multiple_of(BEATS_PER_BAR), true),
            Mode::CountIn => (BeatPhase::CountIn, index == 0, true),
            Mode::Recording => (
                BeatPhase::Recording,
                index.is_multiple_of(BEATS_PER_BAR),
                self.clicks,
            ),
            Mode::Idle => unreachable!("no beat fires while idle"),
        };
        self.index = index + 1;
        if self.mode == Mode::CountIn && self.index >= BEATS_PER_BAR {
            self.mode = Mode::Recording;
            self.index = 0;
        }
        (phase, accent, audible)
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
    let (command_producer, command_consumer) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
    let (beat_producer, beat_consumer) = rtrb::RingBuffer::new(BEAT_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let control = MetronomeControl {
        commands: command_producer,
        beats: beat_consumer,
        dropped: Arc::clone(&dropped),
    };
    let mut metronome = Metronome::new();
    let mut commands = command_consumer;
    let mut beats = beat_producer;
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

    /// Renders samples `from`..`to` in blocks of 256 and returns them.
    fn render_span(
        metronome: &mut Metronome,
        from: usize,
        to: usize,
        rate: u32,
        beats: &mut Vec<Beat>,
    ) -> Vec<f32> {
        let mut output = vec![0.0_f32; to - from];
        let mut start = from;
        while start < to {
            let len = (to - start).min(256);
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
        use crate::audio::fake::{fake_backend, FakeConfig};

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
}
