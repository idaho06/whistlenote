//! The engine thread that runs the audio streams and the analysis.
//!
//! The engine thread owns the audio streams and the signal processing. It
//! turns microphone input into live pitch frames on a lossy channel and
//! onsets, device lists and metronome beats on a reliable channel, and it
//! takes commands from the GUI through another channel. The metronome
//! renders its clicks on the output stream.

use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TryRecvError};

use crate::audio::backend::{AudioBackend, AudioStream, ErrorSink};
use crate::audio::ring::{
    input_ring, InputReader, DEFAULT_HEADER_CAPACITY, DEFAULT_SAMPLE_CAPACITY,
};
use crate::audio::rms;
use crate::dsp::onset::{OnsetConfig, OnsetDetector};
use crate::dsp::pitch::{PitchConfig, PitchTracker};
use crate::metronome::{metronome, MetronomeCommand, MetronomeControl};
use crate::music::segment::PitchFrame;

/// What a metronome beat belongs to, as reported in [`EngineEvent::Beat`].
pub use crate::metronome::BeatPhase;

/// Capacity of the live frame channel; frames are dropped when it is full.
pub const LIVE_CAPACITY: usize = 256;

/// One live pitch frame with the level of the input block that produced it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveFrame {
    /// Engine-clock time of the frame in seconds.
    pub time_s: f64,
    /// Detected frequency in Hz, or `None` when the frame is unvoiced.
    pub freq_hz: Option<f32>,
    /// Confidence of the detection in 0.0..=1.0.
    pub confidence: f32,
    /// RMS of the input block that completed the frame, in 0.0..=1.0.
    pub level: f32,
}

/// An event the engine reports on its reliable event channel.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// An onset was detected in the input.
    Onset {
        /// Engine-clock time of the onset in seconds.
        time_s: f64,
    },
    /// The audio device lists, reported at start and on request.
    Devices {
        /// Names of the input devices.
        inputs: Vec<String>,
        /// Names of the output devices.
        outputs: Vec<String>,
    },
    /// A metronome beat starts playing on the output stream at `time_s`.
    Beat {
        /// Index of the beat within its sequence, starting at 0.
        index: u32,
        /// Whether the beat is accented, which the first beat of a bar is.
        accent: bool,
        /// What the beat belongs to.
        phase: BeatPhase,
        /// Engine-clock time at which the beat plays, in seconds.
        time_s: f64,
    },
    /// A non-fatal error occurred.
    Error(
        /// Description of the error.
        String,
    ),
}

/// A command the GUI sends to the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineCommand {
    /// Starts listening: opens the input stream when none is open.
    StartListening,
    /// Stops listening and closes the input stream.
    StopListening,
    /// Selects the input device by name, or the default device for `None`;
    /// an open stream is reopened on the new device.
    SelectInput(Option<String>),
    /// Sets the tempo and the on/off state of the free-running metronome.
    SetMetronome {
        /// Tempo in BPM.
        bpm: f64,
        /// Whether the metronome runs.
        enabled: bool,
    },
    /// Selects the output device by name, or the default device for `None`;
    /// an open output stream is reopened on the new device.
    SelectOutput(Option<String>),
    /// Reports the current device lists as an [`EngineEvent::Devices`] event.
    RefreshDevices,
    /// Stops the engine thread.
    Shutdown,
}

/// A handle to the engine thread and its channels.
pub struct Engine {
    commands: Sender<EngineCommand>,
    live: Receiver<LiveFrame>,
    events: Receiver<EngineEvent>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    /// Spawns the engine thread with `backend` and returns its handle.
    pub fn spawn(backend: impl AudioBackend) -> Engine {
        let (commands, command_rx) = unbounded();
        let (live, live_rx) = bounded(LIVE_CAPACITY);
        let (events, event_rx) = unbounded();
        let thread = std::thread::Builder::new()
            .name("whistlenote-engine".to_string())
            .spawn(move || {
                Worker {
                    backend: Box::new(backend),
                    commands: command_rx,
                    live,
                    events,
                    input_name: None,
                    listening: false,
                    input: None,
                    output_name: None,
                    free_bpm: 120.0,
                    free_enabled: false,
                    output: None,
                }
                .run();
            })
            .expect("the engine thread spawns");
        Engine {
            commands,
            live: live_rx,
            events: event_rx,
            thread: Some(thread),
        }
    }

    /// Sends `command` to the engine; a send error is ignored because the
    /// engine thread is gone.
    pub fn send(&self, command: EngineCommand) {
        let _ = self.commands.send(command);
    }

    /// Returns the lossy channel of live pitch frames.
    pub fn live(&self) -> &Receiver<LiveFrame> {
        &self.live
    }

    /// Returns the reliable channel of events; it never drops an event.
    pub fn events(&self) -> &Receiver<EngineEvent> {
        &self.events
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.send(EngineCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How long the engine thread parks when no input block and no command is
/// waiting; each input block also unparks the thread through the input ring.
const PARK_TIMEOUT: Duration = Duration::from_millis(5);

/// The engine thread's state: the backend, the channels, the open input and
/// the open output.
struct Worker {
    backend: Box<dyn AudioBackend>,
    commands: Receiver<EngineCommand>,
    live: Sender<LiveFrame>,
    events: Sender<EngineEvent>,
    input_name: Option<String>,
    listening: bool,
    input: Option<Input>,
    output_name: Option<String>,
    free_bpm: f64,
    free_enabled: bool,
    output: Option<Output>,
}

/// One open input stream with its processing state.
struct Input {
    /// The stream; dropping it stops the stream.
    _stream: Box<dyn AudioStream>,
    reader: InputReader,
    pitch: PitchTracker,
    onset: OnsetDetector,
    block: Vec<f32>,
    frames: Vec<PitchFrame>,
    onsets: Vec<f64>,
    reported_drops: u64,
}

/// One open output stream with the metronome that renders into it.
struct Output {
    /// The stream; dropping it stops the stream.
    _stream: Box<dyn AudioStream>,
    /// The handle that controls the metronome and receives its beats.
    control: MetronomeControl,
    /// Number of dropped beats already reported as an error event.
    reported_dropped_beats: u64,
}

impl Worker {
    /// Emits the device lists, then drains input and beats and handles
    /// commands until shutdown or a disconnected command channel.
    fn run(mut self) {
        self.emit_devices();
        loop {
            self.drain_input();
            self.drain_beats();
            loop {
                match self.commands.try_recv() {
                    Ok(command) => {
                        if self.handle(command) {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            std::thread::park_timeout(PARK_TIMEOUT);
        }
    }

    /// Handles `command`; returns true when the worker stops.
    fn handle(&mut self, command: EngineCommand) -> bool {
        match command {
            EngineCommand::StartListening => {
                self.listening = true;
                if self.input.is_none() {
                    self.open_input();
                }
            }
            EngineCommand::StopListening => {
                if self.listening {
                    self.listening = false;
                    self.input = None;
                }
            }
            EngineCommand::SelectInput(name) => {
                self.input_name = name;
                if self.input.is_some() {
                    self.input = None;
                    self.open_input();
                }
            }
            EngineCommand::SetMetronome { bpm, enabled } => {
                // A tempo that is not finite is ignored.
                if bpm.is_finite() {
                    self.free_bpm = bpm;
                    self.free_enabled = enabled;
                    if let Some(output) = &mut self.output {
                        let sent = output
                            .control
                            .send(MetronomeCommand::SetFree { bpm, enabled });
                        if !sent {
                            let _ = self
                                .events
                                .send(EngineEvent::Error("metronome command queue full".into()));
                        }
                    } else if enabled {
                        self.open_output();
                    }
                    // Once open, the output stream stays open; disabling
                    // only silences the metronome.
                }
            }
            EngineCommand::SelectOutput(name) => {
                self.output_name = name;
                if self.output.is_some() {
                    self.output = None;
                    self.open_output();
                }
            }
            EngineCommand::RefreshDevices => self.emit_devices(),
            EngineCommand::Shutdown => return true,
        }
        false
    }

    /// Sends the current device lists as a `Devices` event, or the first
    /// listing error as an `Error` event.
    fn emit_devices(&self) {
        match (self.backend.list_inputs(), self.backend.list_outputs()) {
            (Ok(inputs), Ok(outputs)) => {
                let _ = self.events.send(EngineEvent::Devices { inputs, outputs });
            }
            (Err(error), _) | (_, Err(error)) => {
                let _ = self.events.send(EngineEvent::Error(error.to_string()));
            }
        }
    }

    /// Opens the input stream on the selected device and builds the
    /// processing state for its sample rate; an error becomes an event.
    fn open_input(&mut self) {
        let (writer, reader) = input_ring(
            DEFAULT_SAMPLE_CAPACITY,
            DEFAULT_HEADER_CAPACITY,
            std::thread::current(),
        );
        let events = self.events.clone();
        let on_error: ErrorSink = Box::new(move |error| {
            let _ = events.send(EngineEvent::Error(error.to_string()));
        });
        let stream =
            self.backend
                .open_input(self.input_name.as_deref(), writer.into_sink(), on_error);
        match stream {
            Ok(stream) => {
                let sample_rate = stream.sample_rate();
                self.input = Some(Input {
                    _stream: stream,
                    reader,
                    pitch: PitchTracker::new(sample_rate, PitchConfig::default()),
                    onset: OnsetDetector::new(sample_rate, OnsetConfig::default()),
                    block: Vec::new(),
                    frames: Vec::new(),
                    onsets: Vec::new(),
                    reported_drops: 0,
                });
            }
            Err(error) => {
                let _ = self.events.send(EngineEvent::Error(error.to_string()));
            }
        }
    }

    /// Opens the output stream with the metronome on the selected device;
    /// an error becomes an event.
    fn open_output(&mut self) {
        let (mut control, source) = metronome();
        // Queue the current free-running setting before the stream starts,
        // so the metronome applies it with its first rendered block.
        let _ = control.send(MetronomeCommand::SetFree {
            bpm: self.free_bpm,
            enabled: self.free_enabled,
        });
        let events = self.events.clone();
        let on_error: ErrorSink = Box::new(move |error| {
            let _ = events.send(EngineEvent::Error(error.to_string()));
        });
        let stream = self
            .backend
            .open_output(self.output_name.as_deref(), source, on_error);
        match stream {
            Ok(stream) => {
                self.output = Some(Output {
                    _stream: stream,
                    control,
                    reported_dropped_beats: 0,
                });
            }
            Err(error) => {
                let _ = self.events.send(EngineEvent::Error(error.to_string()));
            }
        }
    }

    /// Reads every waiting input block, turns it into live frames and onset
    /// events, and reports dropped samples once per increase.
    fn drain_input(&mut self) {
        let Some(input) = &mut self.input else {
            return;
        };
        while let Some(header) = input.reader.read_block(&mut input.block) {
            if header.after_drop {
                input.pitch.reset();
                input.onset.reset();
            }
            if header.sample_rate != input.pitch.sample_rate() {
                input.pitch = PitchTracker::new(header.sample_rate, PitchConfig::default());
                input.onset = OnsetDetector::new(header.sample_rate, OnsetConfig::default());
            }
            let level = rms(&input.block);
            input
                .pitch
                .process(&input.block, header.time_s, &mut input.frames);
            for frame in input.frames.drain(..) {
                let live = LiveFrame {
                    time_s: frame.time_s,
                    freq_hz: frame.freq_hz,
                    confidence: frame.confidence,
                    level,
                };
                // A full or closed live channel drops the frame.
                let _ = self.live.try_send(live);
            }
            input
                .onset
                .process(&input.block, header.time_s, &mut input.onsets);
            for time_s in input.onsets.drain(..) {
                let _ = self.events.send(EngineEvent::Onset { time_s });
            }
            input.block.clear();
        }
        let dropped = input.reader.dropped_samples();
        if dropped > input.reported_drops {
            let _ = self.events.send(EngineEvent::Error(format!(
                "input overflow: {} samples dropped",
                dropped - input.reported_drops
            )));
            input.reported_drops = dropped;
        }
    }

    /// Forwards every rendered metronome beat as a `Beat` event and reports
    /// dropped beats once per increase.
    fn drain_beats(&mut self) {
        let Some(output) = &mut self.output else {
            return;
        };
        while let Some(beat) = output.control.try_recv() {
            let _ = self.events.send(EngineEvent::Beat {
                index: beat.index,
                accent: beat.accent,
                phase: beat.phase,
                time_s: beat.time_s,
            });
        }
        let dropped = output.control.dropped_beats();
        if dropped > output.reported_dropped_beats {
            let _ = self.events.send(EngineEvent::Error(format!(
                "metronome beats dropped: {}",
                dropped - output.reported_dropped_beats
            )));
            output.reported_dropped_beats = dropped;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crossbeam_channel::Receiver;

    use super::*;
    use crate::audio::fake::{fake_backend, FakeConfig};
    use crate::audio::AudioError;
    use crate::dsp::filter::tests::sine;
    use crate::music::assert_close;

    /// Receives items from `rx` with `recv_timeout` until one matches `pred`
    /// or `timeout` passes; returns the matching item.
    fn wait_for<T>(
        rx: &Receiver<T>,
        timeout: Duration,
        mut pred: impl FnMut(&T) -> bool,
    ) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(item) => {
                    if pred(&item) {
                        return Some(item);
                    }
                }
                Err(_) => return None,
            }
        }
    }

    /// Polls `cond` every 1 ms until it holds or `timeout` passes; returns
    /// whether `cond` held.
    fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Sends `RefreshDevices` and waits for the `Devices` event; commands
    /// are handled in order, so every command sent before the call has been
    /// handled when it returns.
    fn sync(engine: &Engine) {
        engine.send(EngineCommand::RefreshDevices);
        let devices = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Devices { .. })
        });
        assert!(devices.is_some(), "the engine answers RefreshDevices");
    }

    #[test]
    fn devices_are_reported_at_start_and_on_refresh() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);

        let first = engine
            .events()
            .recv_timeout(Duration::from_secs(2))
            .expect("the first event is the device list");
        assert_eq!(
            first,
            EngineEvent::Devices {
                inputs: vec!["Fake Microphone".to_string()],
                outputs: vec!["Fake Speakers".to_string()],
            }
        );

        handle.set_devices(
            vec!["A".to_string(), "B".to_string()],
            vec!["C".to_string()],
        );
        engine.send(EngineCommand::RefreshDevices);
        let refreshed = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Devices { .. })
        });
        assert_eq!(
            refreshed,
            Some(EngineEvent::Devices {
                inputs: vec!["A".to_string(), "B".to_string()],
                outputs: vec!["C".to_string()],
            })
        );
    }

    #[test]
    fn a_440_hz_tone_gives_live_frames_near_440_hz() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::StartListening);
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_some()
        }));

        handle.push_input(&sine(440.0, 0.3, 48_000, 24_000));
        handle.step_n(94);
        let frame = wait_for(engine.live(), Duration::from_secs(2), |frame| {
            frame.freq_hz.is_some()
        })
        .expect("a voiced live frame");
        let freq_hz = frame.freq_hz.expect("the frame is voiced");
        assert!(
            (freq_hz - 440.0).abs() / 440.0 < 0.01,
            "{freq_hz} Hz instead of 440 Hz"
        );
        assert!(
            (0.15..0.25).contains(&frame.level),
            "level {} instead of the 0.212 RMS of a 0.3 sine",
            frame.level
        );
    }

    #[test]
    fn a_tap_gives_one_onset() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::StartListening);
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_some()
        }));

        // 0.3 s of silence, a 5 ms tap at 3 kHz, then 0.3 s of silence.
        let mut signal = vec![0.0_f32; 14_400];
        signal.extend(sine(3000.0, 0.5, 48_000, 240));
        signal.extend(vec![0.0_f32; 14_400]);
        handle.push_input(&signal);
        handle.step_n(117);

        let onset = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Onset { .. })
        })
        .expect("an onset event");
        match onset {
            EngineEvent::Onset { time_s } => assert_close(time_s, 0.3, 0.015),
            other => panic!("expected an onset, got {other:?}"),
        }
    }

    #[test]
    fn listening_commands_are_idempotent() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::StartListening);
        engine.send(EngineCommand::StartListening);
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_some()
        }));
        let error = wait_for(engine.events(), Duration::from_millis(200), |event| {
            matches!(event, EngineEvent::Error(_))
        });
        assert!(error.is_none(), "unexpected error event: {error:?}");

        engine.send(EngineCommand::StopListening);
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_none()
        }));
        engine.send(EngineCommand::StopListening);
        engine.send(EngineCommand::RefreshDevices);
        let devices = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Devices { .. })
        });
        assert!(devices.is_some(), "the engine still answers commands");
    }

    #[test]
    fn stream_errors_become_error_events() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::StartListening);
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_some()
        }));

        handle.fail_input(AudioError::Backend("boom".to_string()));
        let event = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Error(_))
        })
        .expect("an error event");
        match event {
            EngineEvent::Error(message) => assert!(message.contains("boom"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn dropping_the_engine_stops_the_thread_and_the_stream() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::StartListening);
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_some()
        }));

        drop(engine);
        assert_eq!(handle.input_device(), None);
    }

    #[test]
    fn metronome_beats_become_events() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        assert!(wait_until(Duration::from_secs(2), || {
            handle.output_device().is_some()
        }));

        handle.step_n(375); // 96_000 samples, 2 s
        let mut beats = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while beats.len() < 4 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match engine.events().recv_timeout(remaining) {
                Ok(EngineEvent::Beat {
                    index,
                    accent,
                    phase,
                    time_s,
                }) => beats.push((index, accent, phase, time_s)),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(beats.len(), 4, "four beat events, got {beats:?}");
        for (k, &(index, accent, phase, time_s)) in beats.iter().enumerate() {
            assert_eq!(index, k as u32);
            assert_eq!(phase, BeatPhase::Free);
            assert_eq!(accent, k == 0);
            assert_close(time_s, 0.5 * k as f64, 1e-9);
        }

        let output = handle.take_output();
        assert_eq!(output.len(), 96_000);
        assert!(
            output.iter().any(|&sample| sample != 0.0),
            "the metronome rendered audible clicks"
        );
    }

    #[test]
    fn disabling_the_metronome_keeps_the_stream_and_stops_beats() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        assert!(wait_until(Duration::from_secs(2), || {
            handle.output_device().is_some()
        }));
        handle.step_n(10);
        // Beat 0 plays in the first block; once its event arrives, no beat
        // is in flight.
        let first = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Beat { .. })
        });
        assert!(first.is_some(), "the first beat event arrives");

        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: false,
        });
        sync(&engine);

        handle.step_n(375); // 2 s more of silence
        engine.send(EngineCommand::RefreshDevices);
        let mut beat_seen = false;
        let devices = wait_for(engine.events(), Duration::from_secs(2), |event| {
            beat_seen |= matches!(event, EngineEvent::Beat { .. });
            matches!(event, EngineEvent::Devices { .. })
        });
        assert!(devices.is_some(), "the engine answers RefreshDevices");
        assert!(
            !beat_seen,
            "a beat event arrived while the metronome is disabled"
        );
        assert_eq!(handle.output_device(), Some("Fake Speakers".to_string()));
    }

    #[test]
    fn select_output_reopens_on_the_named_device() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        handle.set_devices(
            vec!["Fake Microphone".to_string()],
            vec!["Fake Speakers".to_string(), "Headphones".to_string()],
        );
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        assert!(wait_until(Duration::from_secs(2), || {
            handle.output_device().as_deref() == Some("Fake Speakers")
        }));

        engine.send(EngineCommand::SelectOutput(Some("Headphones".to_string())));
        assert!(wait_until(Duration::from_secs(2), || {
            handle.output_device().as_deref() == Some("Headphones")
        }));

        handle.step_n(10);
        let beat = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Beat { .. })
        })
        .expect("a beat event after the reopen");
        match beat {
            EngineEvent::Beat { index, phase, .. } => {
                assert_eq!(index, 0);
                assert_eq!(phase, BeatPhase::Free);
            }
            other => panic!("expected a beat, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_output_device_gives_an_error() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::SelectOutput(Some("Nope".to_string())));
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        let error = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Error(_))
        })
        .expect("an error event for the unknown output device");
        match error {
            EngineEvent::Error(message) => assert!(message.contains("Nope"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
        assert_eq!(handle.output_device(), None);
    }

    #[test]
    fn output_errors_become_error_events() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        assert!(wait_until(Duration::from_secs(2), || {
            handle.output_device().is_some()
        }));

        handle.fail_output(AudioError::Backend("crackle".to_string()));
        let event = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Error(_))
        })
        .expect("an error event");
        match event {
            EngineEvent::Error(message) => assert!(message.contains("crackle"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn a_non_finite_tempo_is_ignored() {
        let (backend, handle) = fake_backend(FakeConfig::default());
        let engine = Engine::spawn(backend);
        engine.send(EngineCommand::SetMetronome {
            bpm: f64::NAN,
            enabled: true,
        });
        sync(&engine);
        assert_eq!(handle.output_device(), None);
    }
}
