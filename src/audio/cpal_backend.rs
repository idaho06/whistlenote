//! The audio backend for real devices, built on `cpal`.
//!
//! Streams use each device's default configuration, except that input asks
//! for a supported configuration at 44.1 kHz or higher when the default
//! input rate is lower. Input is averaged to mono and output is copied to
//! every channel; any sample format is converted to and from `f32`. The
//! callbacks neither allocate nor lock.
//!
//! Stream timestamps map onto the engine clock through a per-stream anchor:
//! during the first callbacks of a stream, the pair of host callback instant
//! and engine-clock time with the smallest difference is kept, and every later
//! host instant is shifted by that difference. A host whose callback instants
//! do not increase falls back to the engine-clock time of the callback, minus
//! the block duration for input and plus it for output.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Device, ErrorKind, FromSample, I24, Sample, SampleFormat, SizedSample, StreamConfig,
    SupportedStreamConfig, SupportedStreamConfigRange, U24,
};

use super::AudioError;
use super::backend::{
    AudioBackend, AudioStream, ErrorSink, InputBlockInfo, InputSink, OutputBlockInfo, OutputSource,
    STANDARD_INPUT_RATES_HZ,
};

/// Largest number of frames handed to a sink or source in one call; longer
/// host blocks are split.
const MAX_BLOCK_FRAMES: usize = 8192;

/// Number of callbacks over which a stream's clock anchor is chosen.
const ANCHOR_CALLBACKS: u32 = 8;

/// Name used for a device whose description is not available.
const UNNAMED_DEVICE: &str = "Unnamed device";

/// Lowest input sample rate the backend asks for when the device's default is lower.
const MIN_INPUT_RATE_HZ: u32 = 44_100;

/// Input sample rates the backend prefers, in order.
const PREFERRED_INPUT_RATES_HZ: [u32; 2] = [48_000, 44_100];

impl From<cpal::Error> for AudioError {
    fn from(error: cpal::Error) -> Self {
        match error.kind() {
            ErrorKind::DeviceNotAvailable => AudioError::NoDevice,
            ErrorKind::UnsupportedConfig => AudioError::UnsupportedFormat(error.to_string()),
            _ => AudioError::Backend(error.to_string()),
        }
    }
}

/// The `cpal` backend on the host's default audio API.
///
/// The engine clock starts when the backend is created.
pub struct CpalBackend {
    epoch: Instant,
}

impl CpalBackend {
    /// Creates a backend whose engine clock starts now.
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }

    /// Returns the instant at which the engine clock reads 0 seconds.
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    /// Opens and starts a stream on the device `name`, with the data
    /// callback `callback`, whose kind sets the stream's direction.
    ///
    /// Output uses the device's default configuration and ignores `rate`.
    /// Input uses the default too, unless its sample rate is below 44.1 kHz;
    /// then a supported input configuration at 44.1 kHz or higher is chosen.
    /// `rate` `Some(rate)` asks for that input rate instead when the device
    /// supports it.
    fn open(
        &self,
        name: Option<&str>,
        rate: Option<u32>,
        callback: StreamCallback,
        on_error: ErrorSink,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let direction = callback.direction();
        let device = find_device(direction, name)?;
        let supported = match direction {
            Direction::Input => input_config(&device, rate)?,
            Direction::Output => device.default_output_config()?,
        };
        let config = supported.config();
        log::debug!(
            "open {direction:?} on {}: {} Hz, {} channels, format {:?}, buffer {:?}",
            name.unwrap_or("default"),
            config.sample_rate,
            config.channels,
            supported.sample_format(),
            supported.buffer_size(),
        );
        let sample_rate = config.sample_rate;
        let epoch = self.epoch;
        let failed = Arc::new(AtomicBool::new(false));
        let on_error = error_callback(on_error, Arc::clone(&failed));
        macro_rules! build {
            ($t:ty) => {
                build_stream::<$t>(&device, config, callback, on_error, epoch)
            };
        }
        let stream = match supported.sample_format() {
            SampleFormat::I8 => build!(i8),
            SampleFormat::I16 => build!(i16),
            SampleFormat::I24 => build!(I24),
            SampleFormat::I32 => build!(i32),
            SampleFormat::I64 => build!(i64),
            SampleFormat::U8 => build!(u8),
            SampleFormat::U16 => build!(u16),
            SampleFormat::U24 => build!(U24),
            SampleFormat::U32 => build!(u32),
            SampleFormat::U64 => build!(u64),
            SampleFormat::F32 => build!(f32),
            SampleFormat::F64 => build!(f64),
            other => return Err(AudioError::UnsupportedFormat(other.to_string())),
        }?;
        stream.play()?;
        Ok(Box::new(CpalStream {
            _stream: stream,
            sample_rate,
            failed,
        }))
    }
}

impl Default for CpalBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Input,
    Output,
}

/// The data callback of the stream being opened.
enum StreamCallback {
    /// Receives mono input blocks.
    Input(InputSink),
    /// Fills mono output blocks.
    Output(OutputSource),
}

impl StreamCallback {
    /// Returns the direction of the stream that the callback serves.
    fn direction(&self) -> Direction {
        match self {
            StreamCallback::Input(_) => Direction::Input,
            StreamCallback::Output(_) => Direction::Output,
        }
    }
}

/// Returns the devices of `direction` with their unique names.
fn devices(direction: Direction) -> Result<Vec<(String, Device)>, AudioError> {
    let host = cpal::default_host();
    let devices: Vec<Device> = match direction {
        Direction::Input => host.input_devices()?.collect(),
        Direction::Output => host.output_devices()?.collect(),
    };
    let described = devices
        .iter()
        .map(|device| match device.description() {
            Ok(description) => {
                let name = description.name().to_string();
                let detail = description
                    .extended()
                    .nth(1)
                    .filter(|line| *line != name)
                    .map(str::to_string);
                (name, detail)
            }
            Err(_) => (UNNAMED_DEVICE.to_string(), None),
        })
        .collect();
    Ok(device_labels(described).into_iter().zip(devices).collect())
}

/// Turns `(name, detail)` pairs into unique labels.
///
/// A name that occurs more than once gets its detail line appended as
/// "name — detail"; labels that still repeat get " (2)", " (3)", ... in order.
fn device_labels(described: Vec<(String, Option<String>)>) -> Vec<String> {
    let labels = described
        .iter()
        .map(|(name, detail)| {
            let repeated = described.iter().filter(|(other, _)| other == name).count() > 1;
            match detail {
                Some(detail) if repeated => format!("{name} — {detail}"),
                _ => name.clone(),
            }
        })
        .collect();
    unique_names(labels)
}

/// Makes `names` unique by appending " (2)", " (3)", ... to repeats, in order.
fn unique_names(names: Vec<String>) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(
            |(index, name)| match names[..index].iter().filter(|n| *n == name).count() {
                0 => name.clone(),
                repeats => format!("{name} ({})", repeats + 1),
            },
        )
        .collect()
}

/// Finds the device named `name`, or the host default for `None`.
fn find_device(direction: Direction, name: Option<&str>) -> Result<Device, AudioError> {
    match name {
        None => {
            let host = cpal::default_host();
            match direction {
                Direction::Input => host.default_input_device(),
                Direction::Output => host.default_output_device(),
            }
            .ok_or(AudioError::NoDevice)
        }
        Some(name) => devices(direction)?
            .into_iter()
            .find(|(device_name, _)| device_name == name)
            .map(|(_, device)| device)
            .ok_or_else(|| AudioError::UnknownDevice(name.to_string())),
    }
}

/// Returns the config to open the input of `device` with: its default
/// config, or, when the default rate is below `MIN_INPUT_RATE_HZ`, the one
/// [`choose_input_config`] picks from the supported configs. With
/// `Some(rate)`, [`choose_rate_config`] picks a supported config at that
/// rate when a range covers it; otherwise the rate is refused and the
/// choice falls back to the `None` behaviour.
fn input_config(device: &Device, rate: Option<u32>) -> Result<SupportedStreamConfig, AudioError> {
    let default = device.default_input_config()?;
    if let Some(rate) = rate {
        let chosen = match device.supported_input_configs() {
            Ok(ranges) => {
                let ranges: Vec<_> = ranges.collect();
                choose_rate_config(default, &ranges, rate)
            }
            Err(error) => {
                log::debug!("supported input configs unavailable: {error}");
                None
            }
        };
        match chosen {
            Some(config) => {
                log::debug!("input at {rate} Hz: {config:?}");
                return Ok(config);
            }
            None => log::warn!("input does not support {rate} Hz; choosing the rate automatically"),
        }
    }
    if default.sample_rate() >= MIN_INPUT_RATE_HZ {
        return Ok(default);
    }
    let ranges: Vec<_> = match device.supported_input_configs() {
        Ok(ranges) => ranges.collect(),
        Err(error) => {
            log::debug!("supported input configs unavailable: {error}");
            return Ok(default);
        }
    };
    let chosen = choose_input_config(default, &ranges);
    if chosen != default {
        log::info!(
            "input default {} Hz is low; using {} Hz",
            default.sample_rate(),
            chosen.sample_rate()
        );
    }
    Ok(chosen)
}

/// Ranks `range` against the `default` config: 0 for the default's sample
/// format and channel count, 1 for the same sample format, 2 otherwise.
fn rank(range: &SupportedStreamConfigRange, default: &SupportedStreamConfig) -> u8 {
    if range.sample_format() != default.sample_format() {
        2
    } else if range.channels() != default.channels() {
        1
    } else {
        0
    }
}

/// Returns the rates of [`STANDARD_INPUT_RATES_HZ`] that at least one range
/// of `ranges` covers, ascending.
fn supported_rates(ranges: &[SupportedStreamConfigRange]) -> Vec<u32> {
    STANDARD_INPUT_RATES_HZ
        .iter()
        .copied()
        .filter(|&rate| {
            ranges
                .iter()
                .any(|range| range.min_sample_rate() <= rate && rate <= range.max_sample_rate())
        })
        .collect()
}

/// Returns the config at `rate` from the best-ranked range of `ranges` that
/// covers it: the lowest [`rank`] wins, with ties going to the earliest
/// range; `None` when no range covers `rate`.
fn choose_rate_config(
    default: SupportedStreamConfig,
    ranges: &[SupportedStreamConfigRange],
    rate: u32,
) -> Option<SupportedStreamConfig> {
    ranges
        .iter()
        .filter(|range| range.min_sample_rate() <= rate && rate <= range.max_sample_rate())
        .min_by_key(|range| rank(range, &default))
        .map(|range| range.with_sample_rate(rate))
}

/// Chooses the input config from the device's supported `ranges` when the
/// `default` config's sample rate is below `MIN_INPUT_RATE_HZ`.
///
/// Ranges with the default's sample format and channel count rank first,
/// then ranges with the same sample format, then any range. Within the best
/// rank that covers one of the preferred rates, the first preferred rate a
/// range covers wins. When no range covers a preferred rate, the range
/// whose lowest rate at or above `MIN_INPUT_RATE_HZ` is the lowest wins,
/// with ties going to the better rank. A default at or above
/// `MIN_INPUT_RATE_HZ`, or a device with no range at or above it, keeps the
/// default.
fn choose_input_config(
    default: SupportedStreamConfig,
    ranges: &[SupportedStreamConfigRange],
) -> SupportedStreamConfig {
    if default.sample_rate() >= MIN_INPUT_RATE_HZ {
        return default;
    }
    let preferred = (0..3).find_map(|best| {
        PREFERRED_INPUT_RATES_HZ.iter().find_map(|&rate| {
            ranges
                .iter()
                .filter(|range| rank(range, &default) == best)
                .find_map(|range| range.try_with_sample_rate(rate))
        })
    });
    let lowest_above = || {
        ranges
            .iter()
            .filter(|range| range.max_sample_rate() >= MIN_INPUT_RATE_HZ)
            .map(|range| {
                (
                    range.min_sample_rate().max(MIN_INPUT_RATE_HZ),
                    rank(range, &default),
                    range,
                )
            })
            .min_by_key(|&(rate, rank, _)| (rate, rank))
            .map(|(rate, _, range)| range.with_sample_rate(rate))
    };
    preferred.or_else(lowest_above).unwrap_or(default)
}

/// Returns whether a stream error is worth reporting; glitches, reroutes
/// and refused real-time scheduling leave the stream running.
fn is_reportable(error: &cpal::Error) -> bool {
    !matches!(
        error.kind(),
        ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied
    )
}

/// Whether every stream error ends the stream on this platform: true on
/// Windows, where the WASAPI stream worker stops after any error, and false
/// elsewhere, where `cpal` keeps a stream running after errors other than a
/// lost device, an invalidated stream or a lost host.
const EVERY_ERROR_ENDS_STREAM: bool = cfg!(windows);

/// Returns whether a stream error ends the stream for good. When
/// `every_error_ends` holds, every error ends the stream; otherwise only a
/// lost device, an invalidated stream or a lost host does, and `cpal` keeps
/// the stream running after any other error.
fn ends_stream(error: &cpal::Error, every_error_ends: bool) -> bool {
    every_error_ends
        || matches!(
            error.kind(),
            ErrorKind::DeviceNotAvailable
                | ErrorKind::StreamInvalidated
                | ErrorKind::HostUnavailable
        )
}

/// Wraps `on_error` into a `cpal` error callback that also sets `failed`
/// when an error ends the stream.
fn error_callback(
    mut on_error: ErrorSink,
    failed: Arc<AtomicBool>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    move |error| {
        if ends_stream(&error, EVERY_ERROR_ENDS_STREAM) {
            failed.store(true, Ordering::Relaxed);
        }
        if is_reportable(&error) {
            on_error(AudioError::from(error));
        } else {
            // Rare events on cpal's stream thread, not per audio block.
            log::debug!("stream error ignored: {error}");
        }
    }
}

impl AudioBackend for CpalBackend {
    fn list_inputs(&self) -> Result<Vec<String>, AudioError> {
        Ok(devices(Direction::Input)?
            .into_iter()
            .map(|(name, _)| name)
            .collect())
    }

    fn list_outputs(&self) -> Result<Vec<String>, AudioError> {
        Ok(devices(Direction::Output)?
            .into_iter()
            .map(|(name, _)| name)
            .collect())
    }

    fn input_rates(&self, name: Option<&str>) -> Result<Vec<u32>, AudioError> {
        let device = find_device(Direction::Input, name)?;
        let ranges: Vec<_> = device.supported_input_configs()?.collect();
        Ok(supported_rates(&ranges))
    }

    fn open_input(
        &self,
        name: Option<&str>,
        rate: Option<u32>,
        sink: InputSink,
        on_error: ErrorSink,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        self.open(name, rate, StreamCallback::Input(sink), on_error)
    }

    fn open_output(
        &self,
        name: Option<&str>,
        source: OutputSource,
        on_error: ErrorSink,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        self.open(name, None, StreamCallback::Output(source), on_error)
    }
}

/// A running `cpal` stream.
struct CpalStream {
    _stream: cpal::Stream,
    sample_rate: u32,
    /// Set by the error callback when an error ends the stream.
    failed: Arc<AtomicBool>,
}

impl AudioStream for CpalStream {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

/// Returns the engine-clock time of now in nanoseconds.
fn engine_now_ns(epoch: Instant) -> i128 {
    epoch.elapsed().as_nanos() as i128
}

/// Returns the seconds from the start of a host block to the start of its
/// chunk `index`; a chunk holds at most MAX_BLOCK_FRAMES frames.
fn chunk_offset_s(index: usize, sample_rate: u32) -> f64 {
    (index * MAX_BLOCK_FRAMES) as f64 / f64::from(sample_rate)
}

/// Builds the stream that runs `callback`, converting between the device's
/// sample format `T` and mono `f32` blocks.
fn build_stream<T>(
    device: &Device,
    config: StreamConfig,
    callback: StreamCallback,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
    epoch: Instant,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let sample_rate = config.sample_rate;
    let mut mono = vec![0.0f32; MAX_BLOCK_FRAMES];
    match callback {
        StreamCallback::Input(mut sink) => {
            let mut clock = StreamClock::new(Direction::Input);
            device.build_input_stream(
                config,
                move |data: &[T], info: &cpal::InputCallbackInfo| {
                    let now_ns = engine_now_ns(epoch);
                    let timestamp = info.timestamp();
                    let frames = data.len() / channels;
                    let start_s = clock.engine_time_s(
                        timestamp.callback.as_nanos(),
                        timestamp.capture.as_nanos(),
                        now_ns,
                        frames as f64 / f64::from(sample_rate),
                    );
                    for (index, chunk) in data.chunks(channels * MAX_BLOCK_FRAMES).enumerate() {
                        let written = downmix(chunk, channels, &mut mono);
                        sink(
                            &mono[..written],
                            InputBlockInfo {
                                time_s: start_s + chunk_offset_s(index, sample_rate),
                                sample_rate,
                            },
                        );
                    }
                },
                on_error,
                None,
            )
        }
        StreamCallback::Output(mut source) => {
            let mut clock = StreamClock::new(Direction::Output);
            device.build_output_stream(
                config,
                move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                    data.fill(T::EQUILIBRIUM);
                    let now_ns = engine_now_ns(epoch);
                    let timestamp = info.timestamp();
                    let frames = data.len() / channels;
                    let start_s = clock.engine_time_s(
                        timestamp.callback.as_nanos(),
                        timestamp.playback.as_nanos(),
                        now_ns,
                        frames as f64 / f64::from(sample_rate),
                    );
                    for (index, chunk) in data.chunks_mut(channels * MAX_BLOCK_FRAMES).enumerate() {
                        let block = &mut mono[..chunk.len() / channels];
                        block.fill(0.0);
                        source(
                            block,
                            OutputBlockInfo {
                                time_s: start_s + chunk_offset_s(index, sample_rate),
                                sample_rate,
                            },
                        );
                        upmix(block, channels, chunk);
                    }
                },
                on_error,
                None,
            )
        }
    }
}

/// Maps one stream's host instants onto the engine clock.
#[derive(Debug, Clone, Copy, PartialEq)]
struct StreamClock {
    direction: Direction,
    /// Engine-clock nanoseconds minus host nanoseconds: the smallest offset
    /// seen over the first callbacks.
    offset_ns: i128,
    callbacks: u32,
    last_callback_ns: Option<u128>,
    fallback: bool,
}

impl StreamClock {
    fn new(direction: Direction) -> Self {
        Self {
            direction,
            offset_ns: i128::MAX,
            callbacks: 0,
            last_callback_ns: None,
            fallback: false,
        }
    }

    /// Returns the engine-clock time in seconds of the host instant
    /// `target_ns` (capture or playback of the block's first sample), given
    /// the host instant `callback_ns` of the callback, the engine-clock time
    /// `now_ns` measured in the callback, and the block duration `block_s`.
    fn engine_time_s(
        &mut self,
        callback_ns: u128,
        target_ns: u128,
        now_ns: i128,
        block_s: f64,
    ) -> f64 {
        if self
            .last_callback_ns
            .is_some_and(|last| callback_ns <= last)
        {
            self.fallback = true;
        }
        self.last_callback_ns = Some(callback_ns);
        if self.fallback {
            let now_s = now_ns as f64 / 1e9;
            return match self.direction {
                Direction::Input => now_s - block_s,
                Direction::Output => now_s + block_s,
            };
        }

        if self.callbacks < ANCHOR_CALLBACKS {
            self.callbacks += 1;
            self.offset_ns = self.offset_ns.min(now_ns - callback_ns as i128);
        }
        (target_ns as i128 + self.offset_ns) as f64 / 1e9
    }
}

/// Averages each frame of `interleaved` into one `f32` sample of `out`.
///
/// A frame is `channels` consecutive samples. The number of frames written is
/// the smaller of the complete frames in `interleaved` and `out.len()`, and is
/// returned. `channels` is at least 1.
fn downmix<T>(interleaved: &[T], channels: usize, out: &mut [f32]) -> usize
where
    T: Sample,
    f32: FromSample<T>,
{
    let mut frames = 0;
    for (frame, slot) in interleaved.chunks_exact(channels).zip(out.iter_mut()) {
        let sum: f32 = frame.iter().map(|&s| f32::from_sample(s)).sum();
        *slot = sum / channels as f32;
        frames += 1;
    }
    frames
}

/// Writes each sample of `mono` to all `channels` samples of one frame of
/// `interleaved`.
///
/// The number of frames written is the smaller of `mono.len()` and the
/// complete frames in `interleaved`, and is returned. `channels` is at least 1.
fn upmix<T>(mono: &[f32], channels: usize, interleaved: &mut [T]) -> usize
where
    T: Sample + FromSample<f32>,
{
    let mut frames = 0;
    for (frame, &s) in interleaved.chunks_exact_mut(channels).zip(mono) {
        frame.fill(T::from_sample(s));
        frames += 1;
    }
    frames
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::music::assert_close;

    #[test]
    fn without_every_error_ending_only_lost_devices_and_hosts_end_a_stream() {
        for kind in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::HostUnavailable,
        ] {
            assert!(ends_stream(&cpal::Error::new(kind), false), "{kind:?}");
        }
        for kind in [
            ErrorKind::Xrun,
            ErrorKind::DeviceBusy,
            ErrorKind::BackendError,
            ErrorKind::DeviceChanged,
        ] {
            assert!(!ends_stream(&cpal::Error::new(kind), false), "{kind:?}");
        }
    }

    #[test]
    fn with_every_error_ending_every_kind_ends_a_stream() {
        for kind in [
            ErrorKind::DeviceNotAvailable,
            ErrorKind::StreamInvalidated,
            ErrorKind::HostUnavailable,
            ErrorKind::Xrun,
            ErrorKind::DeviceBusy,
            ErrorKind::BackendError,
            ErrorKind::DeviceChanged,
        ] {
            assert!(ends_stream(&cpal::Error::new(kind), true), "{kind:?}");
        }
    }

    #[test]
    fn an_error_that_ends_the_stream_sets_the_flag_and_is_reported() {
        let reported = Arc::new(Mutex::new(Vec::new()));
        let sink_reported = Arc::clone(&reported);
        let failed = Arc::new(AtomicBool::new(false));
        let mut callback = error_callback(
            Box::new(move |error| sink_reported.lock().unwrap().push(error)),
            Arc::clone(&failed),
        );
        callback(cpal::Error::new(ErrorKind::DeviceBusy));
        assert!(
            !failed.load(Ordering::Relaxed),
            "a busy device keeps the stream"
        );
        callback(cpal::Error::new(ErrorKind::Xrun));
        callback(cpal::Error::new(ErrorKind::DeviceNotAvailable));
        assert!(
            failed.load(Ordering::Relaxed),
            "a lost device ends the stream"
        );
        assert_eq!(
            reported.lock().unwrap().len(),
            2,
            "the xrun is not reported, the other two are"
        );
    }

    #[test]
    fn stereo_frames_are_averaged() {
        let mut out = [0.0; 3];
        let frames = downmix(&[1.0f32, 0.0, 0.5, 0.5, -1.0, 1.0], 2, &mut out);
        assert_eq!(frames, 3);
        assert_eq!(out, [0.5, 0.5, 0.0]);
    }

    #[test]
    fn six_channel_frames_are_averaged_and_partial_frames_ignored() {
        let mut input = vec![0.6f32; 12];
        input.extend([1.0, 1.0]);
        let mut out = [0.0; 4];
        let frames = downmix(&input, 6, &mut out);
        assert_eq!(frames, 2);
        assert!((out[0] - 0.6).abs() < 1e-6 && (out[1] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn downmix_stops_at_the_output_length() {
        let mut out = [0.0; 1];
        assert_eq!(downmix(&[0.25f32, 0.25, 0.75, 0.75], 2, &mut out), 1);
        assert_eq!(out, [0.25]);
    }

    #[test]
    fn integer_formats_scale_to_unit_range() {
        let mut out = [0.0; 3];
        downmix(&[i16::MIN, 0, i16::MAX], 1, &mut out);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[1], 0.0);
        assert!((out[2] - 1.0).abs() < 1e-4);

        downmix(&[0u16, 32768, u16::MAX], 1, &mut out);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[1], 0.0);
        assert!((out[2] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn upmix_duplicates_each_sample_to_every_channel() {
        let mut out = [0i16; 6];
        let frames = upmix(&[0.0, -1.0], 3, &mut out);
        assert_eq!(frames, 2);
        assert_eq!(out, [0, 0, 0, i16::MIN, i16::MIN, i16::MIN]);
    }

    #[test]
    fn repeated_names_get_numbered_suffixes() {
        let names = ["Mic", "USB", "Mic", "Mic"].map(String::from).to_vec();
        assert_eq!(unique_names(names), ["Mic", "USB", "Mic (2)", "Mic (3)"]);
    }

    #[test]
    fn repeated_names_get_their_detail_line() {
        let entry = |name: &str, detail: Option<&str>| (name.to_string(), detail.map(String::from));
        let labels = device_labels(vec![
            entry("PipeWire Sound Server", None),
            entry("HDA Analog", Some("Default Audio Device")),
            entry("HDA Analog", Some("Front output / input")),
            entry("HDA", Some("USB Stream Output")),
            entry("Mic", Some("Same")),
            entry("Mic", Some("Same")),
            entry("Mic", None),
        ]);
        assert_eq!(
            labels,
            [
                "PipeWire Sound Server",
                "HDA Analog — Default Audio Device",
                "HDA Analog — Front output / input",
                "HDA",
                "Mic — Same",
                "Mic — Same (2)",
                "Mic",
            ]
        );
    }

    #[test]
    fn anchor_maps_capture_before_the_callback() {
        // Host clock 1000 s ahead of the engine clock; capture 10 ms before
        // the callback.
        let mut clock = StreamClock::new(Direction::Input);
        let host_ns = 1_000_000_000_000u128 + 5_000_000_000;
        let t = clock.engine_time_s(host_ns, host_ns - 10_000_000, 5_000_000_000, 0.01);
        assert_close(t, 4.99, 1e-9);
    }

    #[test]
    fn anchor_keeps_the_least_delayed_callback() {
        let mut clock = StreamClock::new(Direction::Output);
        // First callback runs 2 ms late, second on time.
        clock.engine_time_s(1_000_000_000, 1_000_000_000, 1_002_000_000, 0.01);
        clock.engine_time_s(2_000_000_000, 2_000_000_000, 2_000_000_000, 0.01);
        // Later callbacks leave the anchor unchanged.
        for i in 3..20u128 {
            clock.engine_time_s(
                i * 1_000_000_000,
                i * 1_000_000_000,
                i as i128 * 1_000_000_000 + 7_000_000,
                0.01,
            );
        }
        let t = clock.engine_time_s(30_000_000_000, 30_020_000_000, 30_000_000_000, 0.01);
        assert_close(t, 30.02, 1e-9);
    }

    #[test]
    fn non_increasing_callback_instants_fall_back_to_now() {
        let mut input = StreamClock::new(Direction::Input);
        input.engine_time_s(0, 0, 1_000_000_000, 0.01);
        let t = input.engine_time_s(0, 0, 2_000_000_000, 0.01);
        assert_close(t, 1.99, 1e-9);

        let mut output = StreamClock::new(Direction::Output);
        output.engine_time_s(5, 5, 1_000_000_000, 0.01);
        let t = output.engine_time_s(5, 5, 2_000_000_000, 0.01);
        assert_close(t, 2.01, 1e-9);
    }

    fn range(
        channels: u16,
        min_sample_rate: u32,
        max_sample_rate: u32,
        sample_format: SampleFormat,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(
            channels,
            min_sample_rate,
            max_sample_rate,
            cpal::SupportedBufferSize::Unknown,
            sample_format,
        )
    }

    fn config(
        channels: u16,
        sample_rate: u32,
        sample_format: SampleFormat,
    ) -> SupportedStreamConfig {
        SupportedStreamConfig::new(
            channels,
            sample_rate,
            cpal::SupportedBufferSize::Unknown,
            sample_format,
        )
    }

    #[test]
    fn a_default_at_or_above_44_1_khz_is_kept() {
        let ranges = [range(2, 8_000, 192_000, SampleFormat::F32)];
        let default = config(2, 48_000, SampleFormat::F32);
        assert_eq!(choose_input_config(default, &ranges), default);
        let default = config(2, 44_100, SampleFormat::F32);
        assert_eq!(choose_input_config(default, &ranges), default);
    }

    #[test]
    fn a_low_default_moves_to_48_khz_when_a_range_covers_it() {
        let default = config(1, 8_000, SampleFormat::I16);
        let ranges = [
            range(1, 8_000, 8_000, SampleFormat::I16),
            range(1, 8_000, 96_000, SampleFormat::I16),
        ];
        let chosen = choose_input_config(default, &ranges);
        assert_eq!(chosen.sample_rate(), 48_000);
        assert_eq!(chosen.sample_format(), SampleFormat::I16);
        assert_eq!(chosen.channels(), 1);
    }

    #[test]
    fn a_low_default_takes_44_1_khz_when_48_is_missing() {
        let default = config(1, 8_000, SampleFormat::I16);
        let ranges = [
            range(1, 44_100, 44_100, SampleFormat::I16),
            range(1, 8_000, 16_000, SampleFormat::I16),
        ];
        assert_eq!(choose_input_config(default, &ranges).sample_rate(), 44_100);
    }

    #[test]
    fn a_low_default_prefers_its_own_format_and_channels() {
        let default = config(1, 16_000, SampleFormat::I16);
        let ranges = [
            range(2, 8_000, 192_000, SampleFormat::F32),
            range(1, 8_000, 48_000, SampleFormat::I16),
        ];
        let chosen = choose_input_config(default, &ranges);
        assert_eq!(chosen.sample_rate(), 48_000);
        assert_eq!(chosen.sample_format(), SampleFormat::I16);
        assert_eq!(chosen.channels(), 1);
    }

    #[test]
    fn a_low_default_takes_the_lowest_rate_above_44_1_khz_without_a_preferred_rate() {
        let default = config(1, 8_000, SampleFormat::I16);
        let ranges = [
            range(1, 88_200, 96_000, SampleFormat::I16),
            range(1, 64_000, 64_000, SampleFormat::I16),
        ];
        assert_eq!(choose_input_config(default, &ranges).sample_rate(), 64_000);
    }

    #[test]
    fn a_low_default_without_higher_rates_is_kept() {
        let default = config(1, 8_000, SampleFormat::I16);
        let ranges = [range(1, 8_000, 16_000, SampleFormat::I16)];
        assert_eq!(choose_input_config(default, &ranges), default);
    }

    #[test]
    fn supported_rates_lists_the_standard_rates_that_some_range_covers() {
        let ranges = [
            range(2, 8_000, 48_000, SampleFormat::F32),
            range(1, 88_200, 96_000, SampleFormat::F32),
        ];
        assert_eq!(supported_rates(&ranges), [44_100, 48_000, 88_200, 96_000]);
        assert!(supported_rates(&[]).is_empty());
        assert!(supported_rates(&[range(2, 8_000, 22_050, SampleFormat::F32)]).is_empty());
    }

    #[test]
    fn choose_rate_config_prefers_the_default_format_and_channels() {
        let default = config(2, 48_000, SampleFormat::F32);
        let ranges = [
            range(1, 8_000, 192_000, SampleFormat::I16),
            range(1, 8_000, 192_000, SampleFormat::F32),
            range(2, 8_000, 192_000, SampleFormat::F32),
        ];
        let chosen = choose_rate_config(default, &ranges, 96_000).expect("a range covers 96 kHz");
        assert_eq!(chosen.sample_rate(), 96_000);
        assert_eq!(chosen.channels(), 2);
        assert_eq!(chosen.sample_format(), SampleFormat::F32);
    }

    #[test]
    fn choose_rate_config_prefers_the_same_format_over_another_format() {
        let default = config(2, 48_000, SampleFormat::F32);
        let ranges = [
            range(1, 8_000, 192_000, SampleFormat::I16),
            range(1, 8_000, 192_000, SampleFormat::F32),
        ];
        let chosen = choose_rate_config(default, &ranges, 96_000).expect("a range covers 96 kHz");
        assert_eq!(chosen.sample_rate(), 96_000);
        assert_eq!(chosen.channels(), 1);
        assert_eq!(chosen.sample_format(), SampleFormat::F32);
    }

    #[test]
    fn choose_rate_config_returns_none_for_an_uncovered_rate() {
        let default = config(2, 48_000, SampleFormat::F32);
        let ranges = [range(2, 8_000, 48_000, SampleFormat::F32)];
        assert_eq!(choose_rate_config(default, &ranges, 96_000), None);
    }
}
