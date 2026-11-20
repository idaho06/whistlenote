//! The engine thread that runs the audio streams and the analysis.
//!
//! The engine thread owns the audio streams and the signal processing. It
//! turns microphone input into live pitch frames on a lossy channel and
//! onsets, device lists and metronome beats on a reliable channel, and it
//! takes commands from the GUI through another channel. The metronome
//! renders its clicks on the output stream.
//!
//! A recording plays one count-in bar on the metronome and then collects
//! the pitch frames of the input until it is stopped; a recording that
//! reaches its origin is reported with its frames on the reliable event
//! channel, and one that ends before its origin is reported as
//! [`EngineEvent::RecordingAborted`].

use std::fmt::Display;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded, unbounded};

use crate::audio::backend::{AudioBackend, AudioStream, ErrorSink};
use crate::audio::ring::{
    DEFAULT_HEADER_CAPACITY, DEFAULT_SAMPLE_CAPACITY, InputReader, input_ring,
};
use crate::audio::rms;
use crate::dsp::onset::{OnsetConfig, OnsetDetector};
use crate::dsp::pitch::{PitchConfig, PitchTracker};
use crate::metronome::{CLICK_S, MetronomeCommand, MetronomeControl, metronome};
use crate::music::segment::PitchFrame;

/// What a metronome beat belongs to, as reported in [`EngineEvent::Beat`].
pub use crate::metronome::BeatPhase;

/// The sound of the metronome clicks, set with
/// [`EngineCommand::SetClickSound`].
pub use crate::metronome::ClickSound;

/// Capacity of the live frame channel; frames are dropped when it is full.
pub const LIVE_CAPACITY: usize = 256;

/// Default of [`AnalysisConfig::pre_roll_s`]: how far before the recording
/// origin the frames of a recording are kept, in seconds.
pub const PRE_ROLL_S: f64 = 0.1;

/// The largest absolute latency offset in milliseconds.
pub const MAX_LATENCY_MS: f64 = 200.0;

/// Settings of the analysis on the engine thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalysisConfig {
    /// Settings of the pitch tracker.
    pub pitch: PitchConfig,
    /// Settings of the onset detector.
    pub onset: OnsetConfig,
    /// How far before the recording origin the frames of a recording are kept, in seconds.
    pub pre_roll_s: f64,
}

impl Default for AnalysisConfig {
    /// The default pitch and onset settings, and a pre-roll of
    /// [`PRE_ROLL_S`] seconds.
    fn default() -> Self {
        Self {
            pitch: PitchConfig::default(),
            onset: OnsetConfig::default(),
            pre_roll_s: PRE_ROLL_S,
        }
    }
}

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
    /// The audio device lists, reported at start and on request; an
    /// [`EngineEvent::InputRates`] event follows each report.
    Devices {
        /// Names of the input devices.
        inputs: Vec<String>,
        /// Names of the output devices.
        outputs: Vec<String>,
    },
    /// The sample rate of the input stream in Hz, or `None` when no input is open; sent whenever it changes.
    InputRate(Option<u32>),
    /// The standard sample rates the selected input supports, ascending; sent after each `Devices` event and after an input selection, empty when they cannot be read. A chosen rate the list does not contain becomes the automatic choice.
    InputRates(Vec<u32>),
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
    /// The first recording beat of a recording plays; the recording starts
    /// at `origin_s`.
    RecordingStarted {
        /// Engine-clock time of the recording origin in seconds, including
        /// the latency offset.
        origin_s: f64,
    },
    /// A recording that reached its origin finished and its pitch frames
    /// are reported.
    RecordingFinished {
        /// Pitch frames of the recording, from [`PRE_ROLL_S`] before the
        /// origin; frames that overlap an audible click are left out.
        frames: Vec<PitchFrame>,
        /// Engine-clock time of the recording origin in seconds.
        origin_s: f64,
    },
    /// The recording ended before its origin; it has no frames.
    RecordingAborted,
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
    /// Stops listening, closes the input stream, and cancels and discards a
    /// running recording.
    StopListening,
    /// Selects the input device by name, or the default device for `None`;
    /// an open stream is reopened on the new device, and the input rate is
    /// reset to the automatic choice because a rate chosen for one device
    /// may not exist on another.
    SelectInput(Option<String>),
    /// Sets the sample rate the input opens at, or the automatic choice for
    /// `None`; a change cancels and discards a running recording and reopens
    /// an open input.
    SetInputRate(
        /// The sample rate in Hz.
        Option<u32>,
    ),
    /// Sets the tempo and the on/off state of the free-running metronome.
    SetMetronome {
        /// Tempo in BPM.
        bpm: f64,
        /// Whether the metronome runs.
        enabled: bool,
    },
    /// Sets the sound of the metronome clicks; it applies to the open
    /// output and to every output opened later.
    SetClickSound(
        /// The click sound.
        ClickSound,
    ),
    /// Selects the output device by name, or the default device for `None`;
    /// an open output stream is reopened on the new device.
    SelectOutput(Option<String>),
    /// Starts a recording: one count-in bar at `bpm`, then the recording;
    /// `metronome` makes the recording beats audible. The input and output
    /// streams open when none are open, so a recording works without
    /// [`EngineCommand::StartListening`]. The recording ends with one
    /// [`EngineEvent::RecordingFinished`] or one
    /// [`EngineEvent::RecordingAborted`]; a start that fails sends an
    /// [`EngineEvent::Error`] and then [`EngineEvent::RecordingAborted`].
    StartRecording {
        /// Tempo in BPM.
        bpm: f64,
        /// Whether the recording beats are audible.
        metronome: bool,
    },
    /// Ends the recording: reports its frames as
    /// [`EngineEvent::RecordingFinished`] when its origin was reached, or
    /// sends [`EngineEvent::RecordingAborted`] during the count-in; without
    /// a recording it sends nothing.
    StopRecording,
    /// Sets the latency offset in milliseconds that is added to the origin
    /// of a recording, clamped to ±[`MAX_LATENCY_MS`]; it applies to
    /// recordings whose origin is not set yet.
    SetLatencyMs(
        /// Latency offset in milliseconds.
        f64,
    ),
    /// Sets the analysis settings. An open input applies the pitch and
    /// onset settings in place, without losing its partial analysis
    /// windows; a running recording keeps the pre-roll it started with.
    SetAnalysis(
        /// The analysis settings.
        AnalysisConfig,
    ),
    /// Reports the current device lists as an [`EngineEvent::Devices`]
    /// event followed by an [`EngineEvent::InputRates`] event; before the
    /// lists are emitted, a lost input is reopened while listening and a
    /// lost output is reopened while the free-running metronome is on.
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
                    input_rate: None,
                    listening: false,
                    input: None,
                    analysis: AnalysisConfig::default(),
                    reported_input_rate: None,
                    output_name: None,
                    free_bpm: 120.0,
                    free_enabled: false,
                    click_sound: ClickSound::default(),
                    output: None,
                    latency_s: 0.0,
                    recording: None,
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

/// How long the engine thread parks while a stream is open and no input
/// block and no command is waiting; each input block also unparks the
/// thread through the input ring.
const PARK_TIMEOUT: Duration = Duration::from_millis(5);

/// The engine thread's state: the backend, the channels, the open input and
/// the open output.
struct Worker {
    backend: Box<dyn AudioBackend>,
    commands: Receiver<EngineCommand>,
    live: Sender<LiveFrame>,
    events: Sender<EngineEvent>,
    input_name: Option<String>,
    /// The sample rate the input opens at in Hz, or `None` for the backend's
    /// automatic choice; reset when the input device changes.
    input_rate: Option<u32>,
    listening: bool,
    input: Option<Input>,
    /// The analysis settings applied to the input and to new recordings.
    analysis: AnalysisConfig,
    /// The input rate last sent as an event.
    reported_input_rate: Option<u32>,
    output_name: Option<String>,
    free_bpm: f64,
    free_enabled: bool,
    /// The sound of the metronome clicks, queued on every opened output.
    click_sound: ClickSound,
    output: Option<Output>,
    /// Latency offset added to the origin of a recording, in seconds.
    latency_s: f64,
    /// The recording in progress, if any.
    recording: Option<Recording>,
}

/// One open input stream with its processing state.
struct Input {
    /// The stream; dropping it stops the stream.
    stream: Box<dyn AudioStream>,
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
    stream: Box<dyn AudioStream>,
    /// The handle that controls the metronome and receives its beats.
    control: MetronomeControl,
    /// Number of dropped beats already reported as an error event.
    reported_dropped_beats: u64,
}

/// A recording in progress on the engine thread.
struct Recording {
    /// Duration of one pitch analysis window of the input, in seconds.
    window_s: f64,
    /// How far before the origin the frames of this recording are kept, in
    /// seconds; set when the recording starts.
    pre_roll_s: f64,
    /// The CountIn beat with index 0 of this recording arrived.
    count_in_seen: bool,
    /// Engine-clock time of the recording origin, set when the Recording
    /// beat with index 0 arrives.
    origin_s: Option<f64>,
    /// Start times of the audible clicks of this recording.
    clicks: Vec<f64>,
    /// Every pitch frame since the recording started.
    frames: Vec<PitchFrame>,
}

/// Keeps the frames from `pre_roll_s` before `origin_s` and leaves out
/// every kept frame whose analysis window overlaps a click of `clicks`.
fn finish_frames(
    frames: Vec<PitchFrame>,
    origin_s: f64,
    pre_roll_s: f64,
    clicks: &[f64],
    window_s: f64,
) -> Vec<PitchFrame> {
    frames
        .into_iter()
        .filter(|frame| frame.time_s >= origin_s - pre_roll_s)
        .filter(|frame| {
            !clicks.iter().any(|&click| {
                click - window_s / 2.0 <= frame.time_s
                    && frame.time_s <= click + CLICK_S + window_s / 2.0
            })
        })
        .collect()
}

/// Returns the log label of a selected device: its name, or `default` when
/// no device is selected.
fn device_label(name: Option<&str>) -> &str {
    name.unwrap_or("default")
}

/// Sends an [`EngineEvent::Error`] with `message` on `events`; a send error
/// is ignored because the receiver is gone.
fn send_error(events: &Sender<EngineEvent>, message: impl Display) {
    log::warn!("error: {message}");
    let _ = events.send(EngineEvent::Error(message.to_string()));
}

/// Sends an [`EngineEvent::RecordingAborted`] on `events`; a send error is
/// ignored because the receiver is gone.
fn send_aborted(events: &Sender<EngineEvent>) {
    let _ = events.send(EngineEvent::RecordingAborted);
}

/// Returns an [`ErrorSink`] that reports every stream error as an
/// [`EngineEvent::Error`] event on `events`.
fn error_sink(events: Sender<EngineEvent>) -> ErrorSink {
    Box::new(move |error| send_error(&events, error))
}

/// Returns how much the counter `total` grew since `reported`, if it grew,
/// and records `total` as reported.
fn new_drops(total: u64, reported: &mut u64) -> Option<u64> {
    let grown = total.checked_sub(*reported).filter(|&grown| grown > 0)?;
    *reported = total;
    Some(grown)
}

/// Returns the pitch tracker and the onset detector for input at
/// `sample_rate` Hz with the settings of `analysis`.
fn analysers(sample_rate: u32, analysis: &AnalysisConfig) -> (PitchTracker, OnsetDetector) {
    (
        PitchTracker::new(sample_rate, analysis.pitch),
        OnsetDetector::new(sample_rate, analysis.onset),
    )
}

impl Worker {
    /// Emits the device lists, then handles commands until shutdown or a
    /// disconnected command channel.
    ///
    /// The input blocks and beats delivered before a command are processed
    /// before it, and failed streams are dropped after them. Once the
    /// streams settle, the input rate is reported when it changed, before
    /// the command is handled. While no stream
    /// is open the worker waits for the next command; otherwise it parks
    /// between input blocks.
    fn run(mut self) {
        self.emit_devices();
        loop {
            let command = if self.input.is_none() && self.output.is_none() {
                self.commands.recv().map_err(|_| TryRecvError::Disconnected)
            } else {
                self.commands.try_recv()
            };
            self.drain_input();
            self.drain_beats();
            self.drop_failed_streams();
            self.close_unwanted_input();
            self.report_input_rate();
            match command {
                Ok(command) => {
                    if self.handle(command) {
                        break;
                    }
                }
                Err(TryRecvError::Empty) => std::thread::park_timeout(PARK_TIMEOUT),
                Err(TryRecvError::Disconnected) => break,
            }
        }
        log::debug!("engine stopped");
    }

    /// Handles `command`; returns true when the worker stops.
    fn handle(&mut self, command: EngineCommand) -> bool {
        log::debug!("command: {command:?}");
        match command {
            EngineCommand::StartListening => {
                self.listening = true;
                if self.input.is_none() {
                    self.open_input();
                }
            }
            EngineCommand::StopListening => {
                self.listening = false;
                self.cancel_recording();
            }
            EngineCommand::SelectInput(name) => {
                self.input_name = name;
                // A rate chosen for one device may not exist on another.
                self.input_rate = None;
                self.reopen_input();
                self.emit_input_rates();
            }
            EngineCommand::SetInputRate(rate) => {
                if rate != self.input_rate {
                    self.input_rate = rate;
                    self.reopen_input();
                }
            }
            EngineCommand::SetMetronome { bpm, enabled } => {
                // A tempo that is not finite is ignored. Once open, the
                // output stream stays open; disabling only silences the
                // metronome.
                if bpm.is_finite() {
                    self.free_bpm = bpm;
                    self.free_enabled = enabled;
                    if self.output.is_some() {
                        self.send_metronome(MetronomeCommand::SetFree { bpm, enabled });
                    } else if enabled {
                        self.open_output(None);
                    }
                }
            }
            EngineCommand::SetClickSound(sound) => {
                self.click_sound = sound;
                self.send_metronome(MetronomeCommand::SetClick(sound));
            }
            EngineCommand::SelectOutput(name) => {
                self.cancel_recording();
                self.output_name = name;
                if self.output.take().is_some() {
                    self.open_output(None);
                }
            }
            EngineCommand::StartRecording { bpm, metronome } => {
                self.start_recording(bpm, metronome);
            }
            EngineCommand::StopRecording => self.finish_recording(),
            EngineCommand::SetLatencyMs(ms) => {
                if ms.is_finite() {
                    self.latency_s = ms.clamp(-MAX_LATENCY_MS, MAX_LATENCY_MS) / 1000.0;
                }
            }
            EngineCommand::SetAnalysis(config) => {
                self.analysis = config;
                if let Some(input) = &mut self.input {
                    input.pitch.set_config(config.pitch);
                    input.onset.set_config(config.onset);
                }
            }
            EngineCommand::RefreshDevices => {
                // Reopening first makes an open error arrive before the
                // Devices event.
                if self.listening && self.input.is_none() {
                    self.open_input();
                }
                if self.free_enabled && self.output.is_none() {
                    self.open_output(None);
                }
                self.emit_devices();
            }
            EngineCommand::Shutdown => return true,
        }
        self.close_unwanted_input();
        self.report_input_rate();
        false
    }

    /// Cancels and discards a running recording, closes the input and opens
    /// it again while listening, so a new device or rate takes effect.
    fn reopen_input(&mut self) {
        self.cancel_recording();
        self.input = None;
        if self.listening {
            self.open_input();
        }
    }

    /// Closes the input when neither listening nor recording needs it.
    fn close_unwanted_input(&mut self) {
        if !self.listening && self.recording.is_none() && self.input.take().is_some() {
            log::debug!("input closed");
        }
    }

    /// Sends the sample rate of the open input as an [`EngineEvent::InputRate`]
    /// event when it differs from the last reported one, storing it as the
    /// reported rate; a send error is ignored because the receiver is gone.
    fn report_input_rate(&mut self) {
        let rate = self.input.as_ref().map(|input| input.pitch.sample_rate());
        if rate != self.reported_input_rate {
            self.reported_input_rate = rate;
            let _ = self.events.send(EngineEvent::InputRate(rate));
        }
    }

    /// Sends the current device lists as a `Devices` event followed by the
    /// supported input rates as an `InputRates` event, or the first listing
    /// error as an `Error` event.
    fn emit_devices(&mut self) {
        match (self.backend.list_inputs(), self.backend.list_outputs()) {
            (Ok(inputs), Ok(outputs)) => {
                log::debug!(
                    "devices: {} inputs, {} outputs",
                    inputs.len(),
                    outputs.len()
                );
                log::trace!("input names: {inputs:?}");
                log::trace!("output names: {outputs:?}");
                let _ = self.events.send(EngineEvent::Devices { inputs, outputs });
                self.emit_input_rates();
            }
            (Err(error), _) | (_, Err(error)) => send_error(&self.events, error),
        }
    }

    /// Sends the standard sample rates the selected input supports as an
    /// [`EngineEvent::InputRates`] event, ascending; a listing error is
    /// logged and reported as an empty list. A chosen rate the list does not
    /// offer becomes the automatic choice without reopening the input, since
    /// the backend already opened it automatically. A send error is ignored
    /// because the receiver is gone.
    fn emit_input_rates(&mut self) {
        let name = self.input_name.as_deref();
        let rates = self.backend.input_rates(name).unwrap_or_else(|error| {
            log::debug!("input rates of {} unavailable: {error}", device_label(name));
            Vec::new()
        });
        log::debug!("input rates of {}: {rates:?}", device_label(name));
        if self.input_rate.is_some_and(|rate| !rates.contains(&rate)) {
            self.input_rate = None;
        }
        let _ = self.events.send(EngineEvent::InputRates(rates));
    }

    /// Opens the input stream on the selected device at the chosen rate and
    /// builds the processing state for its sample rate; an error becomes an
    /// event.
    fn open_input(&mut self) {
        let (writer, reader) = input_ring(
            DEFAULT_SAMPLE_CAPACITY,
            DEFAULT_HEADER_CAPACITY,
            std::thread::current(),
        );
        let on_error = error_sink(self.events.clone());
        let stream = self.backend.open_input(
            self.input_name.as_deref(),
            self.input_rate,
            writer.into_sink(),
            on_error,
        );
        match stream {
            Ok(stream) => {
                log::info!(
                    "input open: {} at {} Hz",
                    device_label(self.input_name.as_deref()),
                    stream.sample_rate()
                );
                let (pitch, onset) = analysers(stream.sample_rate(), &self.analysis);
                self.input = Some(Input {
                    stream,
                    reader,
                    pitch,
                    onset,
                    block: Vec::new(),
                    frames: Vec::new(),
                    onsets: Vec::new(),
                    reported_drops: 0,
                });
            }
            Err(error) => send_error(&self.events, error),
        }
    }

    /// Opens the output stream with the metronome on the selected device,
    /// with the click sound and the free-running setting queued before
    /// `first`; an error becomes an event.
    fn open_output(&mut self, first: Option<MetronomeCommand>) {
        let (mut control, source) = metronome();
        // Queue the commands before the stream starts, so the metronome
        // applies them with its first rendered block.
        let _ = control.send(MetronomeCommand::SetClick(self.click_sound));
        let _ = control.send(MetronomeCommand::SetFree {
            bpm: self.free_bpm,
            enabled: self.free_enabled,
        });
        if let Some(command) = first {
            let _ = control.send(command);
        }
        let on_error = error_sink(self.events.clone());
        let stream = self
            .backend
            .open_output(self.output_name.as_deref(), source, on_error);
        match stream {
            Ok(stream) => {
                log::info!(
                    "output open: {} at {} Hz",
                    device_label(self.output_name.as_deref()),
                    stream.sample_rate()
                );
                self.output = Some(Output {
                    stream,
                    control,
                    reported_dropped_beats: 0,
                });
            }
            Err(error) => send_error(&self.events, error),
        }
    }

    /// Sends `command` to the metronome of the open output and returns
    /// whether it is queued; without an open output it does nothing, and a
    /// full queue becomes an error event.
    fn send_metronome(&mut self, command: MetronomeCommand) -> bool {
        let Some(output) = &mut self.output else {
            return false;
        };
        let queued = output.control.send(command);
        if !queued {
            send_error(&self.events, "metronome command queue full");
        }
        queued
    }

    /// Starts a recording at `bpm`: opens the streams it needs and queues
    /// the count-in; `clicks` makes the recording beats audible. A failure
    /// becomes an error event followed by an [`EngineEvent::RecordingAborted`]
    /// and starts nothing.
    fn start_recording(&mut self, bpm: f64, clicks: bool) {
        // A running recording is discarded without an event.
        self.cancel_recording();
        if !bpm.is_finite() {
            send_error(&self.events, "invalid tempo");
            send_aborted(&self.events);
            return;
        }
        if self.input.is_none() {
            self.open_input();
        }
        // An open that failed already emitted an error event.
        let Some(input) = &self.input else {
            send_aborted(&self.events);
            return;
        };
        let window_s = input.pitch.window_s();
        let command = MetronomeCommand::StartRecording { bpm, clicks };
        let queued = if self.output.is_some() {
            self.send_metronome(command)
        } else {
            // A newly opened stream starts with the count-in.
            self.open_output(Some(command));
            self.output.is_some()
        };
        if queued {
            log::debug!("recording count-in: {bpm} bpm, clicks {clicks}");
            self.recording = Some(Recording {
                window_s,
                pre_roll_s: self.analysis.pre_roll_s,
                count_in_seen: false,
                origin_s: None,
                clicks: Vec::new(),
                frames: Vec::new(),
            });
        } else {
            // The output could not be opened or the metronome command queue
            // is full; the open or queue failure already emitted an error.
            send_aborted(&self.events);
        }
    }

    /// Ends the recording, if one runs: reports its frames as a
    /// [`EngineEvent::RecordingFinished`] when its origin was reached, and
    /// sends an [`EngineEvent::RecordingAborted`] when it is still in its
    /// count-in.
    fn finish_recording(&mut self) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        self.send_metronome(MetronomeCommand::StopRecording);
        match recording.origin_s {
            Some(origin_s) => {
                let frames = finish_frames(
                    recording.frames,
                    origin_s,
                    recording.pre_roll_s,
                    &recording.clicks,
                    recording.window_s,
                );
                log::debug!(
                    "recording finished: {} frames, origin {origin_s:.3} s",
                    frames.len()
                );
                let _ = self
                    .events
                    .send(EngineEvent::RecordingFinished { frames, origin_s });
            }
            None => {
                log::debug!("recording aborted before its origin");
                send_aborted(&self.events);
            }
        }
    }

    /// Cancels a running recording: ends the metronome's count-in or
    /// recording sequence and discards the recording without an event.
    fn cancel_recording(&mut self) {
        if self.recording.take().is_some() {
            log::debug!("recording discarded");
            self.send_metronome(MetronomeCommand::StopRecording);
        }
    }

    /// Drops each stream that failed, after ending a running recording so
    /// its frames so far are reported or its end during the count-in is
    /// sent as an [`EngineEvent::RecordingAborted`], and says how to
    /// recover; `listening` and the metronome settings stay, so a refresh
    /// reopens what was lost.
    fn drop_failed_streams(&mut self) {
        let input_failed = self.input.as_ref().is_some_and(|i| i.stream.is_failed());
        let output_failed = self.output.as_ref().is_some_and(|o| o.stream.is_failed());
        if input_failed || output_failed {
            self.finish_recording();
        }
        if input_failed {
            self.input = None;
            send_error(&self.events, "input lost; press Refresh to reopen it");
        }
        if output_failed {
            self.output = None;
            send_error(&self.events, "output lost; press Refresh to reopen it");
        }
    }

    /// Reads every waiting input block, turns it into live frames and onset
    /// events, and reports dropped samples once per increase.
    fn drain_input(&mut self) {
        let analysis = self.analysis;
        let Some(input) = &mut self.input else {
            return;
        };
        while let Some(header) = input.reader.read_block(&mut input.block) {
            if header.after_drop {
                input.pitch.reset();
                input.onset.reset();
            }
            if header.sample_rate != input.pitch.sample_rate() {
                (input.pitch, input.onset) = analysers(header.sample_rate, &analysis);
            }
            let level = rms(&input.block);
            input
                .pitch
                .process(&input.block, header.time_s, &mut input.frames);
            for frame in input.frames.drain(..) {
                if let Some(recording) = &mut self.recording {
                    recording.frames.push(frame);
                }
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
        if let Some(dropped) = new_drops(input.reader.dropped_samples(), &mut input.reported_drops)
        {
            send_error(
                &self.events,
                format!("input overflow: {dropped} samples dropped"),
            );
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
            if let Some(recording) = &mut self.recording {
                if recording.count_in_seen {
                    if beat.audible {
                        recording.clicks.push(beat.time_s);
                    }
                    if beat.phase == BeatPhase::Recording
                        && beat.index == 0
                        && recording.origin_s.is_none()
                    {
                        let origin_s = beat.time_s + self.latency_s;
                        recording.origin_s = Some(origin_s);
                        log::debug!("recording origin: {origin_s:.3} s");
                        let _ = self.events.send(EngineEvent::RecordingStarted { origin_s });
                    }
                } else if beat.phase == BeatPhase::CountIn && beat.index == 0 {
                    // Beats before the count-in belong to an earlier sequence.
                    recording.count_in_seen = true;
                }
            }
        }
        if let Some(dropped) = new_drops(
            output.control.dropped_beats(),
            &mut output.reported_dropped_beats,
        ) {
            send_error(&self.events, format!("metronome beats dropped: {dropped}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crossbeam_channel::Receiver;

    use super::*;
    use crate::audio::AudioError;
    use crate::audio::fake::{FakeConfig, FakeHandle, fake_backend};
    use crate::dsp::filter::tests::sine;
    use crate::music::assert_close;
    use crate::music::quantize::{Grid, quantize};
    use crate::music::segment::{SegmentConfig, segment};

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

    /// Spawns an engine on a fake backend with `config` and consumes the
    /// startup `Devices` event, so that later waits see only later events;
    /// returns the engine and the fake handle.
    fn spawn_engine_on(config: FakeConfig) -> (Engine, FakeHandle) {
        let (backend, handle) = fake_backend(config);
        let engine = Engine::spawn(backend);
        let startup = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Devices { .. })
        });
        assert!(startup.is_some(), "the startup Devices event");
        (engine, handle)
    }

    /// Spawns an engine on a fake backend with the default config; see
    /// [`spawn_engine_on`].
    fn spawn_engine() -> (Engine, FakeHandle) {
        spawn_engine_on(FakeConfig::default())
    }

    /// Sends `RefreshDevices` and waits for the `Devices` answer, returning
    /// every event that arrived before it; commands are handled in order, so
    /// every command sent before the call has been handled when it returns.
    fn sync(engine: &Engine) -> Vec<EngineEvent> {
        engine.send(EngineCommand::RefreshDevices);
        let mut seen = Vec::new();
        let devices = wait_for(engine.events(), Duration::from_secs(2), |event| {
            let is_devices = matches!(event, EngineEvent::Devices { .. });
            if !is_devices {
                seen.push(event.clone());
            }
            is_devices
        });
        assert!(devices.is_some(), "the engine answers RefreshDevices");
        seen
    }

    /// Asserts that no event of `seen` matches `pred`; `what` names the
    /// unwanted event in the failure message, which also lists `seen`.
    fn assert_no_event(seen: &[EngineEvent], pred: impl Fn(&EngineEvent) -> bool, what: &str) {
        assert!(!seen.iter().any(pred), "{what}: {seen:?}");
    }

    /// Waits until the input stream of `handle` is open.
    fn wait_input_open(handle: &FakeHandle) {
        assert!(
            wait_until(Duration::from_secs(2), || handle.input_device().is_some()),
            "the input stream opens"
        );
    }

    /// Waits until the output stream of `handle` is open.
    fn wait_output_open(handle: &FakeHandle) {
        assert!(
            wait_until(Duration::from_secs(2), || handle.output_device().is_some()),
            "the output stream opens"
        );
    }

    /// Waits for the next `RecordingStarted` event and returns its origin.
    fn wait_started(engine: &Engine) -> f64 {
        match wait_for(engine.events(), Duration::from_secs(5), |event| {
            matches!(event, EngineEvent::RecordingStarted { .. })
        }) {
            Some(EngineEvent::RecordingStarted { origin_s }) => origin_s,
            other => panic!("expected RecordingStarted, got {other:?}"),
        }
    }

    /// Waits for the next `RecordingFinished` event and returns its frames
    /// and origin.
    fn wait_finished(engine: &Engine) -> (Vec<PitchFrame>, f64) {
        match wait_for(engine.events(), Duration::from_secs(5), |event| {
            matches!(event, EngineEvent::RecordingFinished { .. })
        }) {
            Some(EngineEvent::RecordingFinished { frames, origin_s }) => (frames, origin_s),
            other => panic!("expected RecordingFinished, got {other:?}"),
        }
    }

    /// Waits for the next `RecordingAborted` event.
    fn wait_aborted(engine: &Engine) {
        match wait_for(engine.events(), Duration::from_secs(5), |event| {
            matches!(event, EngineEvent::RecordingAborted)
        }) {
            Some(EngineEvent::RecordingAborted) => {}
            other => panic!("expected RecordingAborted, got {other:?}"),
        }
    }

    /// Waits for the next `Error` event of `engine` and returns its message.
    fn wait_error(engine: &Engine) -> String {
        match wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::Error(_))
        }) {
            Some(EngineEvent::Error(message)) => message,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    /// Waits until the input stream of `handle` is closed.
    fn wait_input_closed(handle: &FakeHandle) {
        assert!(
            wait_until(Duration::from_secs(2), || handle.input_device().is_none()),
            "the input stream closes"
        );
    }

    /// Waits until the output stream of `handle` is closed.
    fn wait_output_closed(handle: &FakeHandle) {
        assert!(
            wait_until(Duration::from_secs(2), || handle.output_device().is_none()),
            "the output stream closes"
        );
    }

    /// Waits until both the input and the output stream of `handle` are open.
    fn wait_both_streams(handle: &FakeHandle) {
        wait_input_open(handle);
        wait_output_open(handle);
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
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);

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
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);

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
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);
        let error = wait_for(engine.events(), Duration::from_millis(200), |event| {
            matches!(event, EngineEvent::Error(_))
        });
        assert!(error.is_none(), "unexpected error event: {error:?}");

        engine.send(EngineCommand::StopListening);
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_none()
        }));
        engine.send(EngineCommand::StopListening);
        sync(&engine);
    }

    #[test]
    fn the_input_rate_is_reported_when_the_input_opens_and_closes() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);
        let opened = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::InputRate(_))
        });
        assert_eq!(opened, Some(EngineEvent::InputRate(Some(48_000))));

        engine.send(EngineCommand::StopListening);
        wait_input_closed(&handle);
        let closed = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(event, EngineEvent::InputRate(_))
        });
        assert_eq!(closed, Some(EngineEvent::InputRate(None)));

        let seen = sync(&engine);
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::InputRate(_)),
            "an InputRate event arrived without a rate change",
        );
    }

    /// Spawns an engine whose fake input offers 48 and 96 kHz, starts
    /// listening and waits until the input reports 48 kHz.
    fn listening_at_48_khz() -> (Engine, FakeHandle) {
        let (engine, handle) = spawn_engine_on(FakeConfig {
            inputs: vec!["A".to_string(), "B".to_string()],
            input_rates: vec![48_000, 96_000],
            ..FakeConfig::default()
        });
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);
        expect_event(&engine, EngineEvent::InputRate(Some(48_000)));
        (engine, handle)
    }

    /// Waits until `engine` sends `expected`, failing the test after 2 s.
    fn expect_event(engine: &Engine, expected: EngineEvent) {
        let found = wait_for(engine.events(), Duration::from_secs(2), |event| {
            *event == expected
        });
        assert!(found.is_some(), "the event {expected:?}");
    }

    /// Sets the input rate of `engine` to `rate` and waits until the input
    /// reports it.
    fn set_rate(engine: &Engine, handle: &FakeHandle, rate: u32) {
        engine.send(EngineCommand::SetInputRate(Some(rate)));
        expect_event(engine, EngineEvent::InputRate(Some(rate)));
        assert_eq!(handle.input_rate(), Some(rate));
    }

    #[test]
    fn the_input_rates_follow_the_devices_event() {
        let (engine, _handle) = spawn_engine_on(FakeConfig {
            input_rates: vec![48_000, 96_000],
            ..FakeConfig::default()
        });
        engine.send(EngineCommand::RefreshDevices);
        expect_event(&engine, EngineEvent::InputRates(vec![48_000, 96_000]));
    }

    #[test]
    fn set_input_rate_reopens_the_input_at_that_rate() {
        let (engine, handle) = listening_at_48_khz();
        set_rate(&engine, &handle, 96_000);
    }

    #[test]
    fn an_unchanged_input_rate_does_not_reopen() {
        let (engine, handle) = listening_at_48_khz();
        set_rate(&engine, &handle, 96_000);
        engine.send(EngineCommand::SetInputRate(Some(96_000)));
        assert_no_event(
            &sync(&engine),
            |event| matches!(event, EngineEvent::InputRate(_)),
            "the input reopened for an unchanged rate",
        );
        assert_eq!(handle.input_rate(), Some(96_000));
    }

    #[test]
    fn a_rate_the_input_stops_offering_becomes_auto_without_reopening() {
        let (engine, handle) = listening_at_48_khz();
        set_rate(&engine, &handle, 96_000);
        handle.set_input_rates(vec![48_000]);
        engine.send(EngineCommand::RefreshDevices);
        expect_event(&engine, EngineEvent::InputRates(vec![48_000]));
        // The engine already holds Auto, so choosing it changes nothing.
        engine.send(EngineCommand::SetInputRate(None));
        assert_no_event(
            &sync(&engine),
            |event| matches!(event, EngineEvent::InputRate(_)),
            "the input reopened",
        );
        assert_eq!(handle.input_rate(), Some(96_000));
    }

    #[test]
    fn selecting_an_input_resets_the_rate_and_reports_its_rates() {
        let (engine, handle) = listening_at_48_khz();
        set_rate(&engine, &handle, 96_000);
        engine.send(EngineCommand::SelectInput(Some("B".to_string())));
        // The rates are sent while the command is handled, before the
        // reopened input reports its rate.
        expect_event(&engine, EngineEvent::InputRates(vec![48_000, 96_000]));
        expect_event(&engine, EngineEvent::InputRate(Some(48_000)));
        assert_eq!(handle.input_device().as_deref(), Some("B"));
        assert_eq!(handle.input_rate(), Some(48_000));
    }

    #[test]
    fn a_rate_change_during_a_recording_discards_it() {
        let (engine, handle) = listening_at_48_khz();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_output_open(&handle);
        handle.step_n(470); // 2.507 s: past the count-in
        wait_started(&engine);

        set_rate(&engine, &handle, 96_000);
        handle.step_n(100);
        assert_no_event(
            &sync(&engine),
            |event| {
                matches!(
                    event,
                    EngineEvent::RecordingFinished { .. } | EngineEvent::RecordingAborted
                )
            },
            "the discarded recording sent an event",
        );
    }

    #[test]
    fn dropping_the_engine_stops_the_thread_and_the_stream() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);

        drop(engine);
        assert_eq!(handle.input_device(), None);
    }

    #[test]
    fn metronome_beats_become_events() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        wait_output_open(&handle);

        handle.step_n(375); // 96_000 samples, 2 s
        let mut beats = Vec::new();
        wait_for(engine.events(), Duration::from_secs(2), |event| {
            if let EngineEvent::Beat {
                index,
                accent,
                phase,
                time_s,
            } = *event
            {
                beats.push((index, accent, phase, time_s));
            }
            beats.len() == 4
        });
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
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        wait_output_open(&handle);
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
        let seen = sync(&engine);
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::Beat { .. }),
            "a beat event arrived while the metronome is disabled",
        );
        assert_eq!(handle.output_device(), Some("Fake Speakers".to_string()));
    }

    #[test]
    fn select_output_reopens_on_the_named_device() {
        let (engine, handle) = spawn_engine();
        handle.set_devices(
            vec!["Fake Microphone".to_string()],
            vec!["Fake Speakers".to_string(), "Headphones".to_string()],
        );
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
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SelectOutput(Some("Nope".to_string())));
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        let message = wait_error(&engine);
        assert!(message.contains("Nope"), "{message}");
        assert_eq!(handle.output_device(), None);
    }

    #[test]
    fn a_non_finite_tempo_is_ignored() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SetMetronome {
            bpm: f64::NAN,
            enabled: true,
        });
        sync(&engine);
        assert_eq!(handle.output_device(), None);
    }

    #[test]
    fn recording_reports_the_origin_and_leaves_out_clicks() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_both_streams(&handle);

        handle.push_input(&vec![0.0_f32; 96_000]); // 2 s of silence
        handle.push_input(&sine(1046.5, 0.3, 48_000, 96_000));
        handle.step_n(750); // 4 s

        assert_close(wait_started(&engine), 2.0, 1e-9);

        engine.send(EngineCommand::StopRecording);
        let (frames, origin_s) = wait_finished(&engine);
        assert_close(origin_s, 2.0, 1e-9);
        assert!(!frames.is_empty(), "the recording has frames");
        assert!(
            frames.iter().all(|frame| frame.time_s >= 1.9 - 1e-9),
            "every frame is kept from the pre-roll on"
        );
        let clicks = [2.0, 2.5, 3.0, 3.5];
        for frame in &frames {
            for &click in &clicks {
                assert!(
                    !(click - 0.0214..=click + 0.0414).contains(&frame.time_s),
                    "a frame at {} lies over the click at {click}",
                    frame.time_s
                );
            }
        }
        let voiced: Vec<&PitchFrame> = frames
            .iter()
            .filter(|frame| (2.1..2.4).contains(&frame.time_s))
            .collect();
        assert!(
            voiced.len() >= 20,
            "only {} frames in 2.1..2.4",
            voiced.len()
        );
        for frame in voiced {
            let freq_hz = frame.freq_hz.expect("a frame in 2.1..2.4 is voiced");
            assert!(
                (freq_hz - 1046.5).abs() / 1046.5 < 0.01,
                "{freq_hz} Hz instead of 1046.5 Hz"
            );
        }
    }

    #[test]
    fn a_held_tone_over_audible_clicks_is_one_note() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_both_streams(&handle);

        handle.push_input(&vec![0.0_f32; 96_000]); // 2 s of silence, the count-in
        handle.push_input(&sine(1046.5, 0.3, 48_000, 96_000)); // 2 s of tone from the origin
        handle.step_n(750); // 4 s

        assert_close(wait_started(&engine), 2.0, 1e-9);

        engine.send(EngineCommand::StopRecording);
        let (frames, origin_s) = wait_finished(&engine);
        assert_close(origin_s, 2.0, 1e-9);
        let notes = segment(&frames, &SegmentConfig::default());
        assert_eq!(notes.len(), 1, "one note over the clicks, got {notes:?}");
        assert_eq!(notes[0].midi, 84);
        assert!(
            (notes[0].start_s - 2.0).abs() <= 0.05,
            "the note starts at {} s",
            notes[0].start_s
        );
        assert!(
            notes[0].end_s - notes[0].start_s >= 1.9,
            "the note lasts {} s",
            notes[0].end_s - notes[0].start_s
        );
    }

    #[test]
    fn a_note_entering_on_beat_zero_quantizes_to_beat_zero() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 200.0,
            metronome: true,
        });
        wait_both_streams(&handle);

        handle.push_input(&vec![0.0_f32; 57_600]); // 1.2 s of silence, the count-in
        handle.push_input(&sine(1046.5, 0.3, 48_000, 57_600)); // 1.2 s of tone from the origin
        handle.step_n(460); // (1.2 + 1.2) s * 48000 / 256 = 450 blocks, plus a few

        assert_close(wait_started(&engine), 1.2, 1e-9);

        engine.send(EngineCommand::StopRecording);
        let (frames, origin_s) = wait_finished(&engine);
        assert_close(origin_s, 1.2, 1e-9);
        let notes = quantize(
            &segment(&frames, &SegmentConfig::default()),
            200.0,
            Grid::Sixteenth,
            origin_s,
        );
        assert_eq!(notes.len(), 1, "one quantized note, got {notes:?}");
        assert_close(notes[0].start_beats, 0.0, 1e-9);
    }

    #[test]
    fn a_pitch_change_on_a_click_keeps_its_beat() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 200.0,
            metronome: true,
        });
        wait_both_streams(&handle);

        // 1.2 s of silence, the count-in; one beat of 1046.5 Hz from the
        // origin; then, phase-continuous, two beats of 1174.66 Hz; then
        // 0.5 s of silence.
        let mut signal = vec![0.0_f32; 57_600];
        signal.extend(sine(1046.5, 0.3, 48_000, 14_400));
        signal.extend_from_slice(&sine(1174.66, 0.3, 48_000, 14_400 + 28_800)[14_400..]);
        signal.extend(vec![0.0_f32; 24_000]);
        let len = signal.len();
        handle.push_input(&signal);
        handle.step_n(len.div_ceil(256) + 4);

        assert_close(wait_started(&engine), 1.2, 1e-9);

        engine.send(EngineCommand::StopRecording);
        let (frames, origin_s) = wait_finished(&engine);
        assert_close(origin_s, 1.2, 1e-9);
        let notes = quantize(
            &segment(&frames, &SegmentConfig::default()),
            200.0,
            Grid::Sixteenth,
            origin_s,
        );
        assert_eq!(notes.len(), 2, "two quantized notes, got {notes:?}");
        assert_eq!(notes[0].midi, 84);
        assert_close(notes[0].start_beats, 0.0, 1e-9);
        assert_eq!(notes[1].midi, 86);
        assert_close(notes[1].start_beats, 1.0, 1e-9);
    }

    #[test]
    fn the_latency_offset_shifts_the_origin() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SetLatencyMs(50.0));
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: false,
        });
        wait_both_streams(&handle);
        handle.step_n(400); // 2.133 s
        assert_close(wait_started(&engine), 2.05, 1e-9);

        engine.send(EngineCommand::StopRecording);
        engine.send(EngineCommand::SetLatencyMs(1000.0)); // clamped to 200 ms
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: false,
        });
        sync(&engine);
        handle.step_n(400);

        // The first CountIn beat with index 0 after the second StartRecording.
        let count_in_start = wait_for(engine.events(), Duration::from_secs(2), |event| {
            matches!(
                event,
                EngineEvent::Beat {
                    index: 0,
                    phase: BeatPhase::CountIn,
                    ..
                }
            )
        })
        .expect("the second count-in starts");
        let EngineEvent::Beat { time_s: start, .. } = count_in_start else {
            unreachable!("a count-in beat")
        };
        assert_close(wait_started(&engine), start + 2.0 + 0.2, 1e-9);
    }

    #[test]
    fn stopping_during_the_count_in_aborts_the_recording() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_both_streams(&handle);
        handle.step_n(94); // 0.501 s: inside the count-in
        engine.send(EngineCommand::StopRecording);

        let mut started_seen = false;
        let mut finished_seen = false;
        let aborted = wait_for(engine.events(), Duration::from_secs(5), |event| {
            started_seen |= matches!(event, EngineEvent::RecordingStarted { .. });
            finished_seen |= matches!(event, EngineEvent::RecordingFinished { .. });
            matches!(event, EngineEvent::RecordingAborted)
        })
        .expect("a RecordingAborted event");
        assert_eq!(aborted, EngineEvent::RecordingAborted);
        assert!(!started_seen, "no RecordingStarted before the abort");
        assert!(!finished_seen, "no RecordingFinished before the abort");
        let seen = sync(&engine);
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::RecordingFinished { .. }),
            "a RecordingFinished arrived after the abort",
        );
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::RecordingAborted),
            "a second RecordingAborted arrived",
        );
    }

    #[test]
    fn stop_listening_discards_a_recording() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_both_streams(&handle);
        handle.step_n(470); // 2.507 s
        wait_started(&engine);

        engine.send(EngineCommand::StopListening);
        let seen = sync(&engine);
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::RecordingFinished { .. }),
            "a RecordingFinished arrived after StopListening",
        );
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_none()
        }));
    }

    #[test]
    fn a_recording_without_listening_closes_the_input_afterwards() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: false,
        });
        wait_both_streams(&handle);
        handle.step_n(400);
        wait_started(&engine);
        engine.send(EngineCommand::StopRecording);
        wait_finished(&engine);
        assert!(wait_until(Duration::from_secs(2), || {
            handle.input_device().is_none()
        }));

        // While listening, a recording keeps the input open.
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: false,
        });
        sync(&engine); // the recording command is queued before stepping
        handle.step_n(400);
        wait_started(&engine);
        engine.send(EngineCommand::StopRecording);
        wait_finished(&engine);
        sync(&engine);
        assert!(handle.input_device().is_some());
    }

    #[test]
    fn finish_frames_keeps_the_pre_roll_and_leaves_out_clicks() {
        let frames: Vec<PitchFrame> = (0..=300)
            .map(|i| PitchFrame {
                time_s: i as f64 * 0.01,
                freq_hz: Some(1000.0),
                confidence: 0.9,
            })
            .collect();
        let finished = finish_frames(frames, 2.0, PRE_ROLL_S, &[2.0, 2.5], 0.04);
        assert_close(finished[0].time_s, 1.9, 1e-9);
        for t in [1.99, 2.0, 2.03, 2.5] {
            assert!(
                !finished.iter().any(|frame| (frame.time_s - t).abs() < 1e-9),
                "a frame at {t} lies over a click"
            );
        }
        let frame_at = |t: f64| {
            finished
                .iter()
                .find(|frame| (frame.time_s - t).abs() < 1e-9)
                .unwrap_or_else(|| panic!("a frame at {t}"))
        };
        for t in [1.97, 2.05, 2.2, 2.45] {
            let frame = frame_at(t);
            assert_eq!(frame.freq_hz, Some(1000.0), "the frame at {t} is voiced");
            assert_eq!(frame.confidence, 0.9);
        }
    }

    #[test]
    fn finish_frames_keeps_the_given_pre_roll() {
        // Frames at 1.80, 1.85, …, 2.20, in 0.05 s steps.
        let frames: Vec<PitchFrame> = (0..=8)
            .map(|i| PitchFrame {
                time_s: 1.8 + i as f64 * 0.05,
                freq_hz: Some(440.0),
                confidence: 0.9,
            })
            .collect();

        let without_pre_roll = finish_frames(frames.clone(), 2.0, 0.0, &[], 0.04);
        let expected = frames
            .iter()
            .filter(|frame| frame.time_s >= 2.0 - 0.0)
            .count();
        assert_eq!(without_pre_roll.len(), expected);

        let with_pre_roll = finish_frames(frames.clone(), 2.0, 0.12, &[], 0.04);
        let expected = frames
            .iter()
            .filter(|frame| frame.time_s >= 2.0 - 0.12)
            .count();
        assert_eq!(with_pre_roll.len(), expected);
        assert!(
            with_pre_roll.len() > without_pre_roll.len(),
            "the pre-roll keeps more frames: {} vs {}",
            with_pre_roll.len(),
            without_pre_roll.len()
        );
    }

    #[test]
    fn set_analysis_applies_a_silence_level_to_the_open_input() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);

        // A 0.05 sine has a window RMS of about 0.035, above the default
        // silence level of 0.002.
        handle.push_input(&sine(440.0, 0.05, 48_000, 24_000));
        handle.step_n(94);
        sync(&engine);
        let voiced = engine
            .live()
            .try_iter()
            .filter(|frame| frame.freq_hz.is_some())
            .count();
        assert!(voiced > 0, "a voiced live frame before the change");

        engine.send(EngineCommand::SetAnalysis(AnalysisConfig {
            pitch: PitchConfig {
                silence_rms: 0.1,
                ..Default::default()
            },
            ..Default::default()
        }));
        sync(&engine);
        // Drain the live channel, so the frames collected next come from
        // the second tone only.
        let _drained: Vec<LiveFrame> = engine.live().try_iter().collect();

        handle.push_input(&sine(440.0, 0.05, 48_000, 24_000));
        handle.step_n(94);
        sync(&engine);
        let frames: Vec<LiveFrame> = engine.live().try_iter().collect();
        assert!(!frames.is_empty(), "live frames after the change");
        assert!(
            frames.iter().all(|frame| frame.freq_hz.is_none()),
            "every frame is unvoiced: {frames:?}"
        );
    }

    #[test]
    fn set_analysis_before_listening_applies_when_the_input_opens() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SetAnalysis(AnalysisConfig {
            pitch: PitchConfig {
                silence_rms: 0.1,
                ..Default::default()
            },
            ..Default::default()
        }));
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);

        handle.push_input(&sine(440.0, 0.05, 48_000, 24_000));
        handle.step_n(94);
        sync(&engine);
        let frames: Vec<LiveFrame> = engine.live().try_iter().collect();
        assert!(!frames.is_empty(), "live frames");
        assert!(
            frames.iter().all(|frame| frame.freq_hz.is_none()),
            "every frame is unvoiced: {frames:?}"
        );
    }

    #[test]
    fn a_scripted_recording_becomes_three_quantized_notes() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: false,
        });
        wait_both_streams(&handle);

        // 2 s of count-in silence, then three 0.45 s tones separated by
        // 0.05 s of silence, then 0.5 s of silence: 192_000 samples.
        let mut signal = vec![0.0_f32; 96_000];
        for freq in [523.25_f32, 659.26, 783.99] {
            signal.extend(sine(freq, 0.3, 48_000, 21_600));
            signal.extend(vec![0.0_f32; 2_400]);
        }
        signal.extend(vec![0.0_f32; 24_000]);
        assert_eq!(signal.len(), 192_000);
        handle.push_input(&signal);
        handle.step_n(750);

        assert_close(wait_started(&engine), 2.0, 1e-9);

        engine.send(EngineCommand::StopRecording);
        let (frames, origin_s) = wait_finished(&engine);
        assert_close(origin_s, 2.0, 1e-9);

        let notes = quantize(
            &segment(&frames, &SegmentConfig::default()),
            120.0,
            Grid::Sixteenth,
            origin_s,
        );
        assert_eq!(notes.len(), 3, "three quantized notes, got {notes:?}");
        let midis: Vec<u8> = notes.iter().map(|note| note.midi).collect();
        assert_eq!(midis, [72, 76, 79]);
        for (note, expected) in notes.iter().zip([0.0, 1.0, 2.0]) {
            assert_close(note.start_beats, expected, 1e-9);
        }
        for note in &notes {
            assert!(
                (0.75..=1.0).contains(&note.length_beats),
                "length {} beats outside 0.75..=1.0",
                note.length_beats
            );
        }
    }

    #[test]
    fn a_full_live_channel_never_blocks_the_engine() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);

        // 8 rounds of 0.5 s of tone fill the live channel; the input ring
        // holds about 8 s, so nothing overflows while the engine drains it.
        for _ in 0..8 {
            handle.push_input(&sine(1000.0, 0.3, 48_000, 24_000));
            handle.step_n(94);
        }
        assert!(
            wait_until(Duration::from_secs(5), || engine.live().len()
                == LIVE_CAPACITY),
            "the live channel holds {LIVE_CAPACITY} frames"
        );

        // The engine still handles commands while the live channel is full.
        sync(&engine);

        // Reading one frame and running one more round refills the channel
        // up to its capacity, never past it.
        let frame = engine.live().try_recv();
        assert!(
            frame.is_ok(),
            "a live frame is queued while the channel is full"
        );
        handle.push_input(&sine(1000.0, 0.3, 48_000, 24_000));
        handle.step_n(94);
        sync(&engine);
        assert!(engine.live().len() <= LIVE_CAPACITY);
    }

    #[test]
    fn stop_recording_without_a_recording_does_nothing() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StopRecording);
        let seen = sync(&engine);
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::RecordingFinished { .. }),
            "a RecordingFinished arrived without a recording",
        );
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::Error(_)),
            "an Error arrived without a recording",
        );
        assert_eq!(handle.input_device(), None);
        assert_eq!(handle.output_device(), None);
    }

    #[test]
    fn an_invalid_tempo_for_a_recording_is_an_error() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: f64::NAN,
            metronome: true,
        });
        let message = wait_error(&engine);
        assert!(message.contains("tempo"), "{message}");
        wait_aborted(&engine);

        sync(&engine);
        assert_eq!(handle.input_device(), None);
        assert_eq!(handle.output_device(), None);
    }

    #[test]
    fn starting_a_recording_again_restarts_the_count_in() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_both_streams(&handle);
        handle.step_n(200); // 1.07 s: inside the first count-in
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        sync(&engine); // the second StartRecording is handled before the refresh
        handle.step_n(700);

        // Collect events until the engine stays idle for 2 s.
        let mut started: Vec<f64> = Vec::new();
        let mut last_count_in_start: Option<f64> = None;
        let mut idle = Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = idle.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match engine.events().recv_timeout(remaining) {
                Ok(event) => {
                    idle = Instant::now() + Duration::from_secs(2);
                    match event {
                        EngineEvent::RecordingStarted { origin_s } => started.push(origin_s),
                        EngineEvent::Beat {
                            index: 0,
                            phase: BeatPhase::CountIn,
                            time_s,
                            ..
                        } => last_count_in_start = Some(time_s),
                        _ => {}
                    }
                }
                Err(_) => break,
            }
        }

        assert_eq!(
            started.len(),
            1,
            "exactly one RecordingStarted, got {started:?}"
        );
        let origin_s = started[0];
        let count_in_start = last_count_in_start.expect("a count-in beat with index 0");
        assert_close(origin_s, count_in_start + 2.0, 1e-9);
        assert!(origin_s > 2.0, "the origin {origin_s} is not past 2.0 s");
    }

    #[test]
    fn a_failed_input_is_reported_dropped_and_reopened_by_refresh() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);

        handle.fail_input(AudioError::Backend("unplugged".to_string()));
        let message = wait_error(&engine);
        assert!(message.contains("unplugged"), "{message}");
        let message = wait_error(&engine);
        assert!(message.contains("input lost"), "{message}");
        wait_input_closed(&handle);

        engine.send(EngineCommand::RefreshDevices);
        wait_input_open(&handle);
        handle.push_input(&sine(440.0, 0.3, 48_000, 24_000));
        handle.step_n(94);
        let frame = wait_for(engine.live(), Duration::from_secs(2), |frame| {
            frame.freq_hz.is_some()
        })
        .expect("a voiced live frame after the reopen");
        let freq_hz = frame.freq_hz.expect("the frame is voiced");
        assert!(
            (freq_hz - 440.0).abs() / 440.0 < 0.01,
            "{freq_hz} Hz instead of 440 Hz"
        );
    }

    #[test]
    fn an_error_the_stream_survives_keeps_it_open() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartListening);
        wait_input_open(&handle);

        handle.report_input_error(AudioError::Backend("busy".to_string()));
        let message = wait_error(&engine);
        assert!(message.contains("busy"), "{message}");
        let seen = sync(&engine);
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::Error(_)),
            "no lost-stream error follows",
        );
        assert!(handle.input_device().is_some(), "the input stays open");
    }

    #[test]
    fn a_failed_input_ends_a_running_recording() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_both_streams(&handle);

        handle.push_input(&vec![0.0_f32; 96_000]); // 2 s of silence
        handle.push_input(&sine(1046.5, 0.3, 48_000, 96_000));
        handle.step_n(750); // 4 s
        assert_close(wait_started(&engine), 2.0, 1e-9);
        sync(&engine); // the queued input is drained before the failure

        // No StopRecording: the failed input ends the recording by itself.
        handle.fail_input(AudioError::Backend("gone".to_string()));
        let (frames, origin_s) = wait_finished(&engine);
        assert_close(origin_s, 2.0, 1e-9);
        assert!(
            !frames.is_empty(),
            "the recording reports the frames so far"
        );
        wait_input_closed(&handle);
        assert!(handle.output_device().is_some(), "the output stays open");
    }

    #[test]
    fn a_failed_output_is_reported_dropped_and_reopened_by_refresh() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        wait_output_open(&handle);

        handle.fail_output(AudioError::Backend("crackle".to_string()));
        let message = wait_error(&engine);
        assert!(message.contains("crackle"), "{message}");
        let message = wait_error(&engine);
        assert!(message.contains("output lost"), "{message}");
        wait_output_closed(&handle);

        engine.send(EngineCommand::RefreshDevices);
        wait_output_open(&handle);
    }

    #[test]
    fn a_failed_recording_stream_closes_the_input_and_refresh_opens_nothing() {
        // A recording started without listening and without the free
        // metronome: once its output fails, neither stream is wanted.
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_both_streams(&handle);

        handle.fail_output(AudioError::Backend("gone".to_string()));
        wait_aborted(&engine);
        wait_output_closed(&handle);
        wait_input_closed(&handle);

        sync(&engine);
        assert_eq!(handle.input_device(), None);
        assert_eq!(handle.output_device(), None);
    }

    #[test]
    fn a_recording_whose_output_cannot_open_aborts() {
        // No output device: opening the output fails with an error.
        let (engine, handle) = spawn_engine_on(FakeConfig {
            outputs: Vec::new(),
            ..FakeConfig::default()
        });
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        let message = wait_error(&engine);
        assert!(message.contains("no audio device"), "{message}");
        wait_aborted(&engine);

        let seen = sync(&engine);
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::RecordingFinished { .. }),
            "a RecordingFinished arrived after the abort",
        );
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::RecordingAborted),
            "a second RecordingAborted arrived",
        );
        assert_eq!(handle.output_device(), None);
        wait_input_closed(&handle);
    }

    #[test]
    fn stopping_after_the_origin_finishes_the_recording() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::StartRecording {
            bpm: 120.0,
            metronome: true,
        });
        wait_both_streams(&handle);
        handle.step_n(400); // 2.133 s: past the count-in
        assert_close(wait_started(&engine), 2.0, 1e-9);

        engine.send(EngineCommand::StopRecording);
        let (frames, origin_s) = wait_finished(&engine);
        assert_close(origin_s, 2.0, 1e-9);
        assert!(!frames.is_empty(), "the recording has frames");
        let seen = sync(&engine);
        assert_no_event(
            &seen,
            |event| matches!(event, EngineEvent::RecordingAborted),
            "a RecordingAborted arrived after the RecordingFinished",
        );
    }

    #[test]
    fn the_click_sound_applies_to_an_output_opened_later() {
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SetClickSound(ClickSound {
            volume: 0.0,
            ..Default::default()
        }));
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        sync(&engine);
        handle.step_n(200);
        sync(&engine);
        let output = handle.take_output();
        assert!(!output.is_empty(), "the output rendered blocks");
        assert!(
            output.iter().all(|&sample| sample == 0.0),
            "a zero volume renders silence, got {} non-zero samples",
            output.iter().filter(|&&sample| sample != 0.0).count()
        );

        // A contrasting engine with the default click sound renders audible
        // clicks in the same steps.
        let (engine, handle) = spawn_engine();
        engine.send(EngineCommand::SetMetronome {
            bpm: 120.0,
            enabled: true,
        });
        sync(&engine);
        handle.step_n(200);
        sync(&engine);
        let output = handle.take_output();
        assert!(!output.is_empty(), "the output rendered blocks");
        assert!(
            output.iter().any(|&sample| sample != 0.0),
            "the default click sound renders audible clicks"
        );
    }
}
