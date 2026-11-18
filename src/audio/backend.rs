//! Traits and callback types that connect the engine to an audio backend.
//!
//! All times are seconds on the engine clock. Input is delivered as mono
//! `f32` blocks; output is rendered as mono `f32` blocks that the backend
//! copies to every channel of the device.

use super::AudioError;

/// Timing of one block of input samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputBlockInfo {
    /// Engine-clock time at which the first sample of the block is captured.
    pub time_s: f64,
    /// Sample rate of the input stream in Hz.
    pub sample_rate: u32,
}

/// Timing of one block of output samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputBlockInfo {
    /// Engine-clock time at which the first sample of the block plays.
    pub time_s: f64,
    /// Sample rate of the output stream in Hz.
    pub sample_rate: u32,
}

/// Receives each block of mono input samples.
///
/// It runs on the audio callback thread, so it must neither block nor
/// allocate.
pub type InputSink = Box<dyn FnMut(&[f32], InputBlockInfo) + Send>;

/// Fills each block of mono output samples.
///
/// The slice arrives filled with zeros. It runs on the audio callback thread,
/// so it must neither block nor allocate.
pub type OutputSource = Box<dyn FnMut(&mut [f32], OutputBlockInfo) + Send>;

/// Receives errors that a running stream reports.
pub type ErrorSink = Box<dyn FnMut(AudioError) + Send>;

/// A running audio stream; dropping it stops the stream.
///
/// Streams are not `Send`: they stay on the thread that opens them.
pub trait AudioStream {
    /// Returns the sample rate of the stream in Hz.
    fn sample_rate(&self) -> u32;
}

/// A source of audio devices and streams.
///
/// Device names are unique within one list. `None` as a device name selects
/// the host's default device.
pub trait AudioBackend: Send + 'static {
    /// Returns the names of the input devices.
    fn list_inputs(&self) -> Result<Vec<String>, AudioError>;

    /// Returns the names of the output devices.
    fn list_outputs(&self) -> Result<Vec<String>, AudioError>;

    /// Opens and starts an input stream on the named device.
    fn open_input(
        &self,
        name: Option<&str>,
        sink: InputSink,
        on_error: ErrorSink,
    ) -> Result<Box<dyn AudioStream>, AudioError>;

    /// Opens and starts an output stream on the named device.
    fn open_output(
        &self,
        name: Option<&str>,
        source: OutputSource,
        on_error: ErrorSink,
    ) -> Result<Box<dyn AudioStream>, AudioError>;
}
