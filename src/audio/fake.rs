//! A deterministic audio backend for tests.
//!
//! [`fake_backend`] returns a [`FakeBackend`], which the engine owns, and a
//! [`FakeHandle`], which the test keeps. Nothing runs on its own: every call
//! to [`FakeHandle::step`] advances the clock by one block, delivers one block
//! of queued input to the open input stream, and renders one block from the
//! open output stream into a capture buffer. The clock starts at 0 seconds and
//! advances at the config's sample rate; the input stream may open at one of
//! the configured input rates.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::AudioError;
use super::backend::{
    AudioBackend, AudioStream, ErrorSink, InputBlockInfo, InputSink, OutputBlockInfo, OutputSource,
};

/// Settings of a fake backend.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeConfig {
    /// Sample rate of both directions in Hz.
    pub sample_rate: u32,
    /// Number of samples per block.
    pub block_size: usize,
    /// Input device names; the first one is the default.
    pub inputs: Vec<String>,
    /// Output device names; the first one is the default.
    pub outputs: Vec<String>,
    /// Input rates the fake input devices support.
    pub input_rates: Vec<u32>,
}

impl Default for FakeConfig {
    /// 48 kHz, 256-sample blocks, one input and one output device, and 48 kHz
    /// input rates.
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            block_size: 256,
            inputs: vec!["Fake Microphone".to_string()],
            outputs: vec!["Fake Speakers".to_string()],
            input_rates: vec![48_000],
        }
    }
}

/// An open stream slot: its id, the device name, its sample rate, the
/// callbacks and whether the stream failed.
struct Slot<F> {
    id: u64,
    device: String,
    sample_rate: u32,
    callback: F,
    on_error: ErrorSink,
    failed: bool,
}

struct State {
    config: FakeConfig,
    frames: u64,
    queued_input: VecDeque<f32>,
    output: Vec<f32>,
    next_id: u64,
    input: Option<Slot<InputSink>>,
    output_stream: Option<Slot<OutputSource>>,
}

impl State {
    /// Returns the engine-clock time of the next block in seconds.
    fn clock_s(&self) -> f64 {
        self.frames as f64 / f64::from(self.config.sample_rate)
    }
}

type Shared = Arc<Mutex<State>>;

fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Creates a fake backend and the handle that drives it.
pub fn fake_backend(config: FakeConfig) -> (FakeBackend, FakeHandle) {
    let state = Arc::new(Mutex::new(State {
        config,
        frames: 0,
        queued_input: VecDeque::new(),
        output: Vec::new(),
        next_id: 0,
        input: None,
        output_stream: None,
    }));
    (
        FakeBackend {
            state: Arc::clone(&state),
        },
        FakeHandle { state },
    )
}

/// Picks the device `name` from `devices`, or the first one for `None`.
fn resolve(devices: &[String], name: Option<&str>) -> Result<String, AudioError> {
    match name {
        None => devices.first().cloned().ok_or(AudioError::NoDevice),
        Some(name) => devices
            .iter()
            .find(|device| *device == name)
            .cloned()
            .ok_or_else(|| AudioError::UnknownDevice(name.to_string())),
    }
}

/// Resolves the device, takes the next stream id and installs the slot at
/// `sample_rate`.
fn open_slot<F>(
    next_id: &mut u64,
    devices: &[String],
    slot: &mut Option<Slot<F>>,
    name: Option<&str>,
    sample_rate: u32,
    callback: F,
    on_error: ErrorSink,
) -> Result<u64, AudioError> {
    let device = resolve(devices, name)?;
    let id = *next_id;
    *next_id += 1;
    *slot = Some(Slot {
        id,
        device,
        sample_rate,
        callback,
        on_error,
        failed: false,
    });
    Ok(id)
}

/// Returns whether `slot` holds the stream with `id` and that stream failed.
fn slot_failed<F>(slot: &Option<Slot<F>>, id: u64) -> bool {
    slot.as_ref()
        .is_some_and(|slot| slot.id == id && slot.failed)
}

/// Clears `slot` when it holds the stream with `id`.
fn clear_slot<F>(slot: &mut Option<Slot<F>>, id: u64) {
    if slot.as_ref().is_some_and(|slot| slot.id == id) {
        *slot = None;
    }
}

/// The backend side of a fake audio system.
pub struct FakeBackend {
    state: Shared,
}

impl AudioBackend for FakeBackend {
    fn list_inputs(&self) -> Result<Vec<String>, AudioError> {
        Ok(lock(&self.state).config.inputs.clone())
    }

    fn list_outputs(&self) -> Result<Vec<String>, AudioError> {
        Ok(lock(&self.state).config.outputs.clone())
    }

    fn input_rates(&self, name: Option<&str>) -> Result<Vec<u32>, AudioError> {
        let state = lock(&self.state);
        resolve(&state.config.inputs, name)?;
        Ok(state.config.input_rates.clone())
    }

    fn open_input(
        &self,
        name: Option<&str>,
        rate: Option<u32>,
        sink: InputSink,
        on_error: ErrorSink,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let mut state = lock(&self.state);
        let State {
            config,
            next_id,
            input,
            ..
        } = &mut *state;
        let sample_rate = match rate {
            Some(rate) if config.input_rates.contains(&rate) => rate,
            _ => config.sample_rate,
        };
        let id = open_slot(
            next_id,
            &config.inputs,
            input,
            name,
            sample_rate,
            sink,
            on_error,
        )?;
        Ok(Box::new(FakeStream {
            state: Arc::clone(&self.state),
            id,
            sample_rate,
        }))
    }

    fn open_output(
        &self,
        name: Option<&str>,
        source: OutputSource,
        on_error: ErrorSink,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let mut state = lock(&self.state);
        let State {
            config,
            next_id,
            output_stream,
            ..
        } = &mut *state;
        let id = open_slot(
            next_id,
            &config.outputs,
            output_stream,
            name,
            config.sample_rate,
            source,
            on_error,
        )?;
        Ok(Box::new(FakeStream {
            state: Arc::clone(&self.state),
            id,
            sample_rate: config.sample_rate,
        }))
    }
}

/// An open fake stream; dropping it closes its slot.
struct FakeStream {
    state: Shared,
    id: u64,
    sample_rate: u32,
}

impl AudioStream for FakeStream {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn is_failed(&self) -> bool {
        let state = lock(&self.state);
        slot_failed(&state.input, self.id) || slot_failed(&state.output_stream, self.id)
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        clear_slot(&mut state.input, self.id);
        clear_slot(&mut state.output_stream, self.id);
    }
}

/// The test side of a fake audio system.
#[derive(Clone)]
pub struct FakeHandle {
    state: Shared,
}

impl FakeHandle {
    /// Runs `f` with the open input slot, if there is one.
    fn with_input<R>(&self, f: impl FnOnce(&mut Slot<InputSink>) -> R) -> Option<R> {
        Some(f(lock(&self.state).input.as_mut()?))
    }

    /// Runs `f` with the open output slot, if there is one.
    fn with_output<R>(&self, f: impl FnOnce(&mut Slot<OutputSource>) -> R) -> Option<R> {
        Some(f(lock(&self.state).output_stream.as_mut()?))
    }

    /// Queues `samples` as microphone input, after any input already queued.
    pub fn push_input(&self, samples: &[f32]) {
        lock(&self.state).queued_input.extend(samples);
    }

    /// Returns the number of queued input samples not yet delivered.
    pub fn queued_input(&self) -> usize {
        lock(&self.state).queued_input.len()
    }

    /// Advances the clock by one block.
    ///
    /// The clock advances by `block_size` samples at the config's sample
    /// rate. The next block of queued input, padded with zeros, goes to the
    /// open input stream with the open input's rate in its
    /// [`InputBlockInfo::sample_rate`]; the samples are consumed even when
    /// no input stream is open. The open output stream renders one block at
    /// the config's sample rate, which is appended to the capture buffer.
    pub fn step(&self) {
        let mut state = lock(&self.state);
        let block_size = state.config.block_size;
        let sample_rate = state.config.sample_rate;
        let time_s = state.clock_s();

        let available = block_size.min(state.queued_input.len());
        let mut block = Vec::with_capacity(block_size);
        block.extend(state.queued_input.drain(..available));
        block.resize(block_size, 0.0);
        if let Some(slot) = state.input.as_mut() {
            (slot.callback)(
                &block,
                InputBlockInfo {
                    time_s,
                    sample_rate: slot.sample_rate,
                },
            );
        }

        if let Some(slot) = state.output_stream.as_mut() {
            let mut rendered = vec![0.0; block_size];
            (slot.callback)(
                &mut rendered,
                OutputBlockInfo {
                    time_s,
                    sample_rate,
                },
            );
            state.output.extend(rendered);
        }

        state.frames += block_size as u64;
    }

    /// Calls [`step`](Self::step) `blocks` times.
    pub fn step_n(&self, blocks: usize) {
        for _ in 0..blocks {
            self.step();
        }
    }

    /// Returns the engine-clock time of the next block in seconds.
    pub fn time_s(&self) -> f64 {
        lock(&self.state).clock_s()
    }

    /// Returns and clears the captured output samples.
    pub fn take_output(&self) -> Vec<f32> {
        std::mem::take(&mut lock(&self.state).output)
    }

    /// Returns the device name of the open input stream.
    pub fn input_device(&self) -> Option<String> {
        self.with_input(|slot| slot.device.clone())
    }

    /// Returns the device name of the open output stream.
    pub fn output_device(&self) -> Option<String> {
        self.with_output(|slot| slot.device.clone())
    }

    /// Replaces the device lists.
    pub fn set_devices(&self, inputs: Vec<String>, outputs: Vec<String>) {
        let mut state = lock(&self.state);
        state.config.inputs = inputs;
        state.config.outputs = outputs;
    }

    /// Replaces the input rates the fake input devices support.
    pub fn set_input_rates(&self, rates: Vec<u32>) {
        lock(&self.state).config.input_rates = rates;
    }

    /// Returns the rate of the open input stream in Hz, or `None` when no
    /// input is open.
    pub fn input_rate(&self) -> Option<u32> {
        lock(&self.state)
            .input
            .as_ref()
            .map(|slot| slot.sample_rate)
    }

    /// Fails the open input stream: it reports `error` through its error
    /// sink and [`AudioStream::is_failed`] returns true from then on.
    pub fn fail_input(&self, error: AudioError) {
        let _ = self.with_input(|slot| {
            slot.failed = true;
            (slot.on_error)(error);
        });
    }

    /// Fails the open output stream the same way as [`Self::fail_input`].
    pub fn fail_output(&self, error: AudioError) {
        let _ = self.with_output(|slot| {
            slot.failed = true;
            (slot.on_error)(error);
        });
    }

    /// Reports `error` through the open input stream's error sink without
    /// failing the stream, as a backend does for an error it survives.
    pub fn report_input_error(&self, error: AudioError) {
        let _ = self.with_input(|slot| (slot.on_error)(error));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn small_config() -> FakeConfig {
        FakeConfig {
            sample_rate: 1000,
            block_size: 4,
            ..FakeConfig::default()
        }
    }

    fn no_errors() -> ErrorSink {
        Box::new(|_| {})
    }

    #[test]
    fn input_blocks_carry_queued_samples_and_clock_times() {
        let (backend, handle) = fake_backend(small_config());
        let (tx, rx) = mpsc::channel();
        let sink: InputSink = Box::new(move |block, info| {
            tx.send((block.to_vec(), info)).expect("receiver alive");
        });
        let stream = backend
            .open_input(None, None, sink, no_errors())
            .expect("default input opens");
        assert_eq!(stream.sample_rate(), 1000);
        assert_eq!(handle.input_device().as_deref(), Some("Fake Microphone"));

        handle.push_input(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        handle.step_n(2);

        let (first, first_info) = rx.recv().expect("first block");
        let (second, second_info) = rx.recv().expect("second block");
        assert_eq!(first, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(second, [5.0, 6.0, 0.0, 0.0]);
        assert_eq!(first_info.time_s, 0.0);
        assert_eq!(second_info.time_s, 0.004);
        assert_eq!(second_info.sample_rate, 1000);
        assert_eq!(handle.time_s(), 0.008);
    }

    #[test]
    fn output_is_rendered_and_captured_with_block_times() {
        let (backend, handle) = fake_backend(small_config());
        let source: OutputSource = Box::new(|block, info| block.fill(info.time_s as f32));
        let _stream = backend
            .open_output(Some("Fake Speakers"), source, no_errors())
            .expect("named output opens");
        handle.step_n(2);
        assert_eq!(
            handle.take_output(),
            [0.0, 0.0, 0.0, 0.0, 0.004, 0.004, 0.004, 0.004]
        );
        assert!(handle.take_output().is_empty());
    }

    #[test]
    fn dropping_a_stream_stops_its_callbacks_and_input_keeps_flowing() {
        let (backend, handle) = fake_backend(small_config());
        let (tx, rx) = mpsc::channel();
        let sink: InputSink = Box::new(move |block, _| {
            let _ = tx.send(block.to_vec());
        });
        let stream = backend
            .open_input(None, None, sink, no_errors())
            .expect("input opens");
        drop(stream);
        assert_eq!(handle.input_device(), None);

        handle.push_input(&[1.0; 4]);
        handle.step();
        assert!(rx.try_recv().is_err());
        assert_eq!(handle.queued_input(), 0);
    }

    #[test]
    fn device_selection_and_errors() {
        let (backend, handle) = fake_backend(small_config());
        let open = |name| backend.open_input(name, None, Box::new(|_, _| {}), no_errors());
        assert_eq!(
            open(Some("Nope")).err(),
            Some(AudioError::UnknownDevice("Nope".to_string()))
        );

        handle.set_devices(vec!["A".to_string(), "B".to_string()], Vec::new());
        assert_eq!(
            backend.list_inputs(),
            Ok(vec!["A".to_string(), "B".to_string()])
        );
        let _b = open(Some("B")).expect("B opens");
        assert_eq!(handle.input_device().as_deref(), Some("B"));
        assert_eq!(
            backend
                .open_output(None, Box::new(|_, _| {}), no_errors())
                .err(),
            Some(AudioError::NoDevice)
        );
    }

    #[test]
    fn stream_errors_reach_the_error_sink() {
        let (backend, handle) = fake_backend(small_config());
        let (tx, rx) = mpsc::channel();
        let _stream = backend
            .open_input(
                None,
                None,
                Box::new(|_, _| {}),
                Box::new(move |error| {
                    let _ = tx.send(error);
                }),
            )
            .expect("input opens");
        handle.fail_input(AudioError::Backend("unplugged".to_string()));
        assert_eq!(
            rx.recv().expect("error delivered"),
            AudioError::Backend("unplugged".to_string())
        );
    }

    #[test]
    fn opening_at_a_supported_rate_opens_and_reports_that_rate() {
        let (backend, handle) = fake_backend(FakeConfig {
            input_rates: vec![48_000, 96_000],
            ..small_config()
        });
        let (tx, rx) = mpsc::channel();
        let sink: InputSink = Box::new(move |block, info| {
            tx.send((block.to_vec(), info)).expect("receiver alive");
        });
        let stream = backend
            .open_input(None, Some(96_000), sink, no_errors())
            .expect("input opens");
        assert_eq!(stream.sample_rate(), 96_000);
        assert_eq!(handle.input_rate(), Some(96_000));

        handle.push_input(&[1.0, 2.0, 3.0, 4.0]);
        handle.step();
        let (_block, info) = rx.recv().expect("block delivered");
        assert_eq!(info.sample_rate, 96_000);
        assert_eq!(handle.time_s(), 0.004);
    }

    #[test]
    fn opening_at_an_unsupported_rate_opens_at_the_config_rate() {
        let (backend, handle) = fake_backend(FakeConfig {
            input_rates: vec![48_000, 96_000],
            ..small_config()
        });
        let stream = backend
            .open_input(None, Some(44_100), Box::new(|_, _| {}), no_errors())
            .expect("input opens");
        assert_eq!(stream.sample_rate(), 1000);
        assert_eq!(handle.input_rate(), Some(1000));
    }

    #[test]
    fn input_rates_returns_the_configured_list_for_a_known_device() {
        let (backend, _handle) = fake_backend(FakeConfig {
            input_rates: vec![48_000, 96_000],
            ..FakeConfig::default()
        });
        assert_eq!(backend.input_rates(None), Ok(vec![48_000, 96_000]));
        assert_eq!(
            backend.input_rates(Some("Fake Microphone")),
            Ok(vec![48_000, 96_000])
        );
    }

    #[test]
    fn input_rates_of_an_unknown_device_is_an_unknown_device_error() {
        let (backend, _handle) = fake_backend(small_config());
        assert_eq!(
            backend.input_rates(Some("Nope")),
            Err(AudioError::UnknownDevice("Nope".to_string()))
        );
    }
}
