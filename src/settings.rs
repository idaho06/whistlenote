//! The user's settings: the session restored at start and the tuning values of the preferences window, in the units the window shows (dBFS for levels, milliseconds for short times, Hz for frequencies). [`Settings::sanitized`] brings every value into its range. The settings live in [`FILE_NAME`] in the user's configuration directory ([`default_path`]); [`Settings::load_from`] and [`Settings::save_to`] read and write them as TOML.

use std::fs;
use std::io;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::music::quantize::Grid;

/// Version of the settings format.
pub const VERSION: u32 = 1;

/// Name of the settings file in the user's configuration directory.
pub const FILE_NAME: &str = "settings.toml";

/// Returns the path of the settings file in the user's configuration directory, or `None` when there is no home directory.
///
/// The path is `$XDG_CONFIG_HOME/whistlenote/settings.toml` or `~/.config/whistlenote/settings.toml` on Linux, `%APPDATA%\whistlenote\config\settings.toml` on Windows, and `~/Library/Application Support/whistlenote/settings.toml` on macOS.
pub fn default_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "whistlenote").map(|dirs| dirs.config_dir().join(FILE_NAME))
}

/// The user's settings: the session restored at start and the tuning values of the preferences window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Version of the settings format; [`Settings::sanitized`] sets it to [`VERSION`].
    pub version: u32,
    /// The session values restored at start.
    pub session: Session,
    /// The tuning values of the preferences window.
    pub tuning: Tuning,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            session: Session::default(),
            tuning: Tuning::default(),
        }
    }
}

/// The session values restored at start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Name of the input device, or `None` for the default device.
    pub input: Option<String>,
    /// Name of the output device, or `None` for the default device.
    pub output: Option<String>,
    /// Latency offset in milliseconds added to the origin of a recording.
    pub latency_ms: f64,
    /// Tempo used while recording, in beats per minute.
    pub record_bpm: f64,
    /// Beat grid the recorded notes are quantized to.
    pub grid: GridSetting,
    /// Whether the recording beats are audible.
    pub metronome: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            latency_ms: 0.0,
            record_bpm: 120.0,
            grid: GridSetting::default(),
            metronome: true,
        }
    }
}

/// Beat grid the recorded notes are quantized to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GridSetting {
    /// No snapping to a grid.
    Off,
    /// Snap to quarter notes.
    Quarter,
    /// Snap to eighth notes.
    Eighth,
    /// Snap to sixteenth notes.
    #[default]
    Sixteenth,
}

impl From<GridSetting> for Grid {
    fn from(grid: GridSetting) -> Self {
        match grid {
            GridSetting::Off => Grid::Off,
            GridSetting::Quarter => Grid::Quarter,
            GridSetting::Eighth => Grid::Eighth,
            GridSetting::Sixteenth => Grid::Sixteenth,
        }
    }
}

impl From<Grid> for GridSetting {
    fn from(grid: Grid) -> Self {
        match grid {
            Grid::Off => GridSetting::Off,
            Grid::Quarter => GridSetting::Quarter,
            Grid::Eighth => GridSetting::Eighth,
            Grid::Sixteenth => GridSetting::Sixteenth,
        }
    }
}

/// The tuning values of the preferences window.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    /// Reference pitch and voiced-frame confidence.
    pub voicing: Voicing,
    /// Pitch tracking.
    pub pitch: Pitch,
    /// Clap and tap detection.
    pub onsets: Onsets,
    /// Segmentation of pitch frames into notes.
    pub notes: Notes,
    /// Recording timing.
    pub timing: Timing,
    /// The click sound.
    pub metronome: Metronome,
    /// The tempo estimate from claps.
    pub tempo: Tempo,
    /// The live note readout.
    pub live: Live,
    /// MIDI export.
    pub export: Export,
}

/// Reference pitch and the confidence a pitch frame needs to count as voiced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Voicing {
    /// Reference pitch of A4 in Hz.
    pub a4_hz: f64,
    /// Confidence in 0..=1 that a pitch frame needs to count as voiced.
    pub min_confidence: f64,
}

impl Default for Voicing {
    fn default() -> Self {
        Self {
            a4_hz: 440.0,
            min_confidence: 0.8,
        }
    }
}

/// Pitch tracking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pitch {
    /// Window level in dBFS, after band-passing, below which a window is silent.
    pub silence_dbfs: f64,
    /// Clarity in 0..=1 a pitch peak needs to be considered by the detector.
    pub clarity: f64,
    /// Lowest reported frequency in Hz.
    pub min_hz: f64,
    /// Highest reported frequency in Hz; the tracker also caps it at a quarter of the sample rate.
    pub max_hz: f64,
    /// Lower edge of the band-pass filter in Hz.
    pub band_low_hz: f64,
    /// Upper edge of the band-pass filter in Hz.
    pub band_high_hz: f64,
}

impl Default for Pitch {
    fn default() -> Self {
        Self {
            silence_dbfs: -54.0,
            clarity: 0.5,
            min_hz: 400.0,
            max_hz: 4000.0,
            band_low_hz: 300.0,
            band_high_hz: 4000.0,
        }
    }
}

/// Clap and tap detection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Onsets {
    /// Window level in dBFS, after high-passing, below which no onset is reported.
    pub gate_dbfs: f64,
    /// Offset added to the median log-energy flux to form the detection threshold; lower is more sensitive.
    pub threshold: f64,
    /// Minimum spacing between reported onsets in milliseconds.
    pub refractory_ms: f64,
    /// Cutoff in Hz of the high-pass applied before onset detection.
    pub high_pass_hz: f64,
    /// Length of the flux history the threshold is derived from, in milliseconds.
    pub history_ms: f64,
}

impl Default for Onsets {
    fn default() -> Self {
        Self {
            gate_dbfs: -40.0,
            threshold: 1.0,
            refractory_ms: 100.0,
            high_pass_hz: 1000.0,
            history_ms: 500.0,
        }
    }
}

/// Segmentation of pitch frames into notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notes {
    /// Minimum duration of a note in milliseconds; shorter notes are dropped.
    pub min_note_ms: f64,
    /// Length of the median filter window, in voiced frames.
    pub median_frames: u32,
    /// Longest unvoiced gap in milliseconds that still joins two equal notes.
    pub max_gap_ms: f64,
    /// Duration in milliseconds below which a note that touches a neighbouring note merges into it.
    pub merge_note_ms: f64,
}

impl Default for Notes {
    fn default() -> Self {
        Self {
            min_note_ms: 60.0,
            median_frames: 5,
            max_gap_ms: 40.0,
            merge_note_ms: 100.0,
        }
    }
}

/// Recording timing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timing {
    /// How far before the recording origin the frames of a recording are kept, in milliseconds.
    pub pre_roll_ms: f64,
}

impl Default for Timing {
    fn default() -> Self {
        Self { pre_roll_ms: 100.0 }
    }
}

/// The click sound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metronome {
    /// Click volume in percent of full scale.
    pub volume_percent: f64,
    /// Frequency of the accented click in Hz.
    pub accent_hz: f64,
    /// Frequency of the other clicks in Hz.
    pub beat_hz: f64,
}

impl Default for Metronome {
    fn default() -> Self {
        Self {
            volume_percent: 50.0,
            accent_hz: 1500.0,
            beat_hz: 1000.0,
        }
    }
}

/// The tempo estimate from claps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tempo {
    /// Most recent onsets the tempo estimate uses.
    pub onset_window: u32,
    /// Gap between onsets in milliseconds that starts a new run.
    pub run_gap_ms: f64,
    /// Lowest tempo reported, in beats per minute.
    pub min_bpm: f64,
    /// Highest tempo reported, in beats per minute.
    pub max_bpm: f64,
}

impl Default for Tempo {
    fn default() -> Self {
        Self {
            onset_window: 12,
            run_gap_ms: 4000.0,
            min_bpm: 40.0,
            max_bpm: 220.0,
        }
    }
}

/// The live note readout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Live {
    /// Voiced frames that must agree in a row before a new note is shown.
    pub agree_frames: u32,
    /// Time in milliseconds after the last voiced frame at which the shown note clears.
    pub clear_ms: f64,
    /// Deviation in cents within which the needle shows in tune.
    pub in_tune_cents: f64,
    /// Distance in cents from the shown note within which the shown note stays.
    pub hold_cents: f64,
    /// Bottom of the input level meter in dB.
    pub level_floor_db: f64,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            agree_frames: 3,
            clear_ms: 300.0,
            in_tune_cents: 5.0,
            hold_cents: 65.0,
            level_floor_db: -60.0,
        }
    }
}

/// MIDI export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Export {
    /// MIDI velocity of the exported notes, in 1..=127.
    pub velocity: u32,
    /// MIDI channel of the exported notes, 1-based in 1..=16.
    pub channel: u32,
}

impl Default for Export {
    fn default() -> Self {
        Self {
            velocity: 100,
            channel: 1,
        }
    }
}

/// Valid ranges of the settings values, used by [`Settings::sanitized`] and by the preferences widgets.
pub mod range {
    use std::ops::RangeInclusive;

    /// Latency offset in milliseconds.
    pub const LATENCY_MS: RangeInclusive<f64> = -200.0..=200.0;
    /// Recording tempo in beats per minute.
    pub const RECORD_BPM: RangeInclusive<f64> = 40.0..=220.0;

    /// Reference pitch of A4 in Hz.
    pub const A4_HZ: RangeInclusive<f64> = 415.0..=466.0;
    /// Confidence a pitch frame needs to count as voiced.
    pub const MIN_CONFIDENCE: RangeInclusive<f64> = 0.3..=0.99;

    /// Silence level in dBFS.
    pub const SILENCE_DBFS: RangeInclusive<f64> = -80.0..=-20.0;
    /// Pitch clarity.
    pub const CLARITY: RangeInclusive<f64> = 0.2..=0.95;
    /// Lowest reported frequency in Hz.
    pub const MIN_HZ: RangeInclusive<f64> = 50.0..=4000.0;
    /// Highest reported frequency in Hz.
    pub const MAX_HZ: RangeInclusive<f64> = 200.0..=8000.0;
    /// Lower edge of the band-pass filter in Hz.
    pub const BAND_LOW_HZ: RangeInclusive<f64> = 50.0..=1000.0;
    /// Upper edge of the band-pass filter in Hz.
    pub const BAND_HIGH_HZ: RangeInclusive<f64> = 1000.0..=8000.0;

    /// Onset gate level in dBFS.
    pub const GATE_DBFS: RangeInclusive<f64> = -70.0..=-10.0;
    /// Onset threshold offset.
    pub const THRESHOLD: RangeInclusive<f64> = 0.2..=4.0;
    /// Minimum spacing between onsets in milliseconds.
    pub const REFRACTORY_MS: RangeInclusive<f64> = 30.0..=500.0;
    /// Onset high-pass cutoff in Hz.
    pub const HIGH_PASS_HZ: RangeInclusive<f64> = 200.0..=4000.0;
    /// Length of the flux history the threshold is derived from, in milliseconds.
    pub const HISTORY_MS: RangeInclusive<f64> = 100.0..=2000.0;

    /// Shortest note in milliseconds.
    pub const MIN_NOTE_MS: RangeInclusive<f64> = 0.0..=500.0;
    /// Median filter window in voiced frames.
    pub const MEDIAN_FRAMES: RangeInclusive<u32> = 1..=15;
    /// Longest joined gap in milliseconds.
    pub const MAX_GAP_MS: RangeInclusive<f64> = 0.0..=200.0;
    /// Short-note merge threshold in milliseconds.
    pub const MERGE_NOTE_MS: RangeInclusive<f64> = 0.0..=250.0;

    /// Pre-roll in milliseconds.
    pub const PRE_ROLL_MS: RangeInclusive<f64> = 0.0..=500.0;

    /// Click volume in percent.
    pub const VOLUME_PERCENT: RangeInclusive<f64> = 0.0..=100.0;
    /// Click frequency in Hz, used for both the accented and the other clicks.
    pub const CLICK_HZ: RangeInclusive<f64> = 200.0..=4000.0;

    /// Onsets the tempo estimate uses.
    pub const ONSET_WINDOW: RangeInclusive<u32> = 3..=32;
    /// Gap that starts a new onset run, in milliseconds.
    pub const RUN_GAP_MS: RangeInclusive<f64> = 1000.0..=10000.0;
    /// Lowest tempo in beats per minute.
    pub const MIN_BPM: RangeInclusive<f64> = 40.0..=110.0;
    /// Highest tempo in beats per minute.
    pub const MAX_BPM: RangeInclusive<f64> = 80.0..=220.0;

    /// Agreeing frames before a new note is shown.
    pub const AGREE_FRAMES: RangeInclusive<u32> = 1..=10;
    /// Time in milliseconds after which the shown note clears.
    pub const CLEAR_MS: RangeInclusive<f64> = 100.0..=2000.0;
    /// In-tune deviation in cents.
    pub const IN_TUNE_CENTS: RangeInclusive<f64> = 1.0..=25.0;
    /// Hold range in cents.
    pub const HOLD_CENTS: RangeInclusive<f64> = 50.0..=100.0;
    /// Bottom of the level meter in dB.
    pub const LEVEL_FLOOR_DB: RangeInclusive<f64> = -90.0..=-30.0;

    /// MIDI velocity.
    pub const VELOCITY: RangeInclusive<u32> = 1..=127;
    /// MIDI channel, 1-based.
    pub const CHANNEL: RangeInclusive<u32> = 1..=16;
}

impl Settings {
    /// Reads the settings from the TOML file at `path` and returns them together with a warning for the user.
    ///
    /// A missing file gives the defaults and no warning; any other read error gives the defaults and a warning. A file whose text does not parse as `Settings` is renamed to a sibling file with ".invalid" appended and the defaults are used with a warning. A parsed file is returned [`sanitized`](Settings::sanitized) and gives no warning.
    pub fn load_from(path: &Path) -> (Settings, Option<String>) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (Settings::default(), None);
            }
            Err(error) => {
                return (
                    Settings::default(),
                    Some(format!(
                        "Cannot read the settings file {}: {error}; using the defaults",
                        path.display()
                    )),
                );
            }
        };
        match toml::from_str::<Settings>(&text) {
            Ok(settings) => (settings.sanitized(), None),
            Err(error) => {
                let message = error.to_string().replace('\n', " ");
                let message = message.trim();
                let invalid = invalid_path(path);
                match fs::rename(path, &invalid) {
                    Ok(()) => (
                        Settings::default(),
                        Some(format!(
                            "The settings file {} is invalid ({message}); it is kept as {} and the defaults are used",
                            path.display(),
                            invalid.display()
                        )),
                    ),
                    Err(rename_error) => (
                        Settings::default(),
                        Some(format!(
                            "The settings file {} is invalid ({message}) and cannot be renamed ({rename_error}); the defaults are used",
                            path.display()
                        )),
                    ),
                }
            }
        }
    }

    /// Writes the settings as pretty TOML to the file at `path`, creating the parent directory when it is missing.
    ///
    /// The text is written to a temporary sibling file with ".tmp" appended and renamed onto `path`, so the file is replaced in one step.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = tmp_path(path);
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }

    /// Returns the settings with every value in its range.
    ///
    /// A float that is not finite takes its field's default, then it is clamped into its range; an integer is clamped. `version` becomes [`VERSION`]. An input or output that is `Some` of an empty or whitespace-only string becomes `None`. After the per-field clamps, `band_low_hz` and `band_high_hz` both take their defaults when `band_low_hz >= band_high_hz`, `min_hz` is clamped up to `band_low_hz` and `max_hz` down to `band_high_hz`, and when `min_hz >= max_hz` all four take their defaults. `min_bpm` and `max_bpm` both take their defaults when `max_bpm < 2.0 * min_bpm`.
    pub fn sanitized(self) -> Self {
        Self {
            version: VERSION,
            session: self.session.sanitized(),
            tuning: self.tuning.sanitized(),
        }
    }
}

impl Session {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            input: device_name(self.input),
            output: device_name(self.output),
            latency_ms: float(self.latency_ms, d.latency_ms, &range::LATENCY_MS),
            record_bpm: float(self.record_bpm, d.record_bpm, &range::RECORD_BPM),
            grid: self.grid,
            metronome: self.metronome,
        }
    }
}

impl Tuning {
    fn sanitized(self) -> Self {
        Self {
            voicing: self.voicing.sanitized(),
            pitch: self.pitch.sanitized(),
            onsets: self.onsets.sanitized(),
            notes: self.notes.sanitized(),
            timing: self.timing.sanitized(),
            metronome: self.metronome.sanitized(),
            tempo: self.tempo.sanitized(),
            live: self.live.sanitized(),
            export: self.export.sanitized(),
        }
    }
}

impl Voicing {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            a4_hz: float(self.a4_hz, d.a4_hz, &range::A4_HZ),
            min_confidence: float(
                self.min_confidence,
                d.min_confidence,
                &range::MIN_CONFIDENCE,
            ),
        }
    }
}

impl Pitch {
    fn sanitized(self) -> Self {
        let d = Self::default();
        let mut s = Self {
            silence_dbfs: float(self.silence_dbfs, d.silence_dbfs, &range::SILENCE_DBFS),
            clarity: float(self.clarity, d.clarity, &range::CLARITY),
            min_hz: float(self.min_hz, d.min_hz, &range::MIN_HZ),
            max_hz: float(self.max_hz, d.max_hz, &range::MAX_HZ),
            band_low_hz: float(self.band_low_hz, d.band_low_hz, &range::BAND_LOW_HZ),
            band_high_hz: float(self.band_high_hz, d.band_high_hz, &range::BAND_HIGH_HZ),
        };
        if s.band_low_hz >= s.band_high_hz {
            s.band_low_hz = d.band_low_hz;
            s.band_high_hz = d.band_high_hz;
        }
        s.min_hz = s.min_hz.max(s.band_low_hz);
        s.max_hz = s.max_hz.min(s.band_high_hz);
        if s.min_hz >= s.max_hz {
            s.band_low_hz = d.band_low_hz;
            s.band_high_hz = d.band_high_hz;
            s.min_hz = d.min_hz;
            s.max_hz = d.max_hz;
        }
        s
    }
}

impl Onsets {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            gate_dbfs: float(self.gate_dbfs, d.gate_dbfs, &range::GATE_DBFS),
            threshold: float(self.threshold, d.threshold, &range::THRESHOLD),
            refractory_ms: float(self.refractory_ms, d.refractory_ms, &range::REFRACTORY_MS),
            high_pass_hz: float(self.high_pass_hz, d.high_pass_hz, &range::HIGH_PASS_HZ),
            history_ms: float(self.history_ms, d.history_ms, &range::HISTORY_MS),
        }
    }
}

impl Notes {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            min_note_ms: float(self.min_note_ms, d.min_note_ms, &range::MIN_NOTE_MS),
            median_frames: int(self.median_frames, &range::MEDIAN_FRAMES),
            max_gap_ms: float(self.max_gap_ms, d.max_gap_ms, &range::MAX_GAP_MS),
            merge_note_ms: float(self.merge_note_ms, d.merge_note_ms, &range::MERGE_NOTE_MS),
        }
    }
}

impl Timing {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            pre_roll_ms: float(self.pre_roll_ms, d.pre_roll_ms, &range::PRE_ROLL_MS),
        }
    }
}

impl Metronome {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            volume_percent: float(
                self.volume_percent,
                d.volume_percent,
                &range::VOLUME_PERCENT,
            ),
            accent_hz: float(self.accent_hz, d.accent_hz, &range::CLICK_HZ),
            beat_hz: float(self.beat_hz, d.beat_hz, &range::CLICK_HZ),
        }
    }
}

impl Tempo {
    fn sanitized(self) -> Self {
        let d = Self::default();
        let mut s = Self {
            onset_window: int(self.onset_window, &range::ONSET_WINDOW),
            run_gap_ms: float(self.run_gap_ms, d.run_gap_ms, &range::RUN_GAP_MS),
            min_bpm: float(self.min_bpm, d.min_bpm, &range::MIN_BPM),
            max_bpm: float(self.max_bpm, d.max_bpm, &range::MAX_BPM),
        };
        if s.max_bpm < 2.0 * s.min_bpm {
            s.min_bpm = d.min_bpm;
            s.max_bpm = d.max_bpm;
        }
        s
    }
}

impl Live {
    fn sanitized(self) -> Self {
        let d = Self::default();
        Self {
            agree_frames: int(self.agree_frames, &range::AGREE_FRAMES),
            clear_ms: float(self.clear_ms, d.clear_ms, &range::CLEAR_MS),
            in_tune_cents: float(self.in_tune_cents, d.in_tune_cents, &range::IN_TUNE_CENTS),
            hold_cents: float(self.hold_cents, d.hold_cents, &range::HOLD_CENTS),
            level_floor_db: float(
                self.level_floor_db,
                d.level_floor_db,
                &range::LEVEL_FLOOR_DB,
            ),
        }
    }
}

impl Export {
    fn sanitized(self) -> Self {
        Self {
            velocity: int(self.velocity, &range::VELOCITY),
            channel: int(self.channel, &range::CHANNEL),
        }
    }
}

/// Returns `value` clamped into `range`, taking `default` first when `value` is not finite.
fn float(value: f64, default: f64, range: &RangeInclusive<f64>) -> f64 {
    let value = if value.is_finite() { value } else { default };
    value.clamp(*range.start(), *range.end())
}

/// Returns `value` clamped into `range`.
fn int(value: u32, range: &RangeInclusive<u32>) -> u32 {
    value.clamp(*range.start(), *range.end())
}

/// Returns `None` for a device name that is empty or whitespace-only.
fn device_name(name: Option<String>) -> Option<String> {
    match name {
        Some(name) if name.trim().is_empty() => None,
        other => other,
    }
}

/// Returns the path an invalid settings file is kept as: the file name of `path` with ".invalid" appended (`settings.toml.invalid`).
fn invalid_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| FILE_NAME.into(), |name| name.to_string_lossy());
    path.with_file_name(format!("{name}.invalid"))
}

/// Returns the path the settings are temporarily written to before the rename onto `path`: the file name of `path` with ".tmp" appended (`settings.toml.tmp`).
fn tmp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| FILE_NAME.into(), |name| name.to_string_lossy());
    path.with_file_name(format!("{name}.tmp"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    /// Returns a fresh scratch directory for the test `name` in the temporary directory.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "whistlenote-settings-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn defaults_are_already_sanitized() {
        assert_eq!(Settings::default().sanitized(), Settings::default());
    }

    #[test]
    fn non_finite_values_take_their_defaults() {
        let mut s = Settings::default();
        s.tuning.voicing.a4_hz = f64::NAN;
        s.tuning.pitch.silence_dbfs = f64::NEG_INFINITY;
        s.session.latency_ms = f64::INFINITY;
        s.tuning.notes.merge_note_ms = f64::NAN;
        let s = s.sanitized();
        assert_eq!(s.tuning.voicing.a4_hz, 440.0);
        assert_eq!(s.tuning.pitch.silence_dbfs, -54.0);
        assert_eq!(s.session.latency_ms, 0.0);
        assert_eq!(s.tuning.notes.merge_note_ms, 100.0);
    }

    #[test]
    fn values_are_clamped_into_their_ranges() {
        let mut s = Settings::default();
        s.tuning.voicing.a4_hz = 500.0;
        s.tuning.pitch.clarity = 0.0;
        s.tuning.export.channel = 0;
        s.tuning.export.velocity = 0;
        s.tuning.notes.median_frames = 0;
        s.session.record_bpm = 300.0;
        s.session.latency_ms = -1000.0;
        let s = s.sanitized();
        assert_eq!(s.tuning.voicing.a4_hz, 466.0);
        assert_eq!(s.tuning.pitch.clarity, 0.2);
        assert_eq!(s.tuning.export.channel, 1);
        assert_eq!(s.tuning.export.velocity, 1);
        assert_eq!(s.tuning.notes.median_frames, 1);
        assert_eq!(s.session.record_bpm, 220.0);
        assert_eq!(s.session.latency_ms, -200.0);

        let mut s = Settings::default();
        s.tuning.export.channel = 40;
        assert_eq!(s.sanitized().tuning.export.channel, 16);
    }

    #[test]
    fn the_pitch_range_stays_inside_the_band() {
        let mut s = Settings::default();
        s.tuning.pitch.band_low_hz = 900.0;
        s.tuning.pitch.min_hz = 400.0;
        assert_eq!(s.sanitized().tuning.pitch.min_hz, 900.0);

        let mut s = Settings::default();
        s.tuning.pitch.min_hz = 3000.0;
        s.tuning.pitch.max_hz = 2000.0;
        let p = s.sanitized().tuning.pitch;
        let d = Pitch::default();
        assert_eq!(p.band_low_hz, d.band_low_hz);
        assert_eq!(p.band_high_hz, d.band_high_hz);
        assert_eq!(p.min_hz, d.min_hz);
        assert_eq!(p.max_hz, d.max_hz);
    }

    #[test]
    fn a_tempo_range_narrower_than_an_octave_takes_the_defaults() {
        let mut s = Settings::default();
        s.tuning.tempo.min_bpm = 100.0;
        s.tuning.tempo.max_bpm = 150.0;
        let t = s.sanitized().tuning.tempo;
        assert_eq!(t.min_bpm, 40.0);
        assert_eq!(t.max_bpm, 220.0);

        let mut s = Settings::default();
        s.tuning.tempo.min_bpm = 60.0;
        s.tuning.tempo.max_bpm = 120.0;
        let t = s.sanitized().tuning.tempo;
        assert_eq!(t.min_bpm, 60.0);
        assert_eq!(t.max_bpm, 120.0);
    }

    #[test]
    fn blank_device_names_become_none() {
        let mut s = Settings::default();
        s.session.input = Some("  ".to_string());
        s.session.output = Some("USB Mic".to_string());
        let s = s.sanitized();
        assert_eq!(s.session.input, None);
        assert_eq!(s.session.output, Some("USB Mic".to_string()));
    }

    #[test]
    fn settings_round_trip_through_toml() {
        let custom = Settings {
            session: Session {
                input: Some("USB Mic".to_string()),
                grid: GridSetting::Eighth,
                ..Session::default()
            },
            tuning: Tuning {
                voicing: Voicing {
                    a4_hz: 432.5,
                    ..Voicing::default()
                },
                ..Tuning::default()
            },
            ..Settings::default()
        };
        for s in [Settings::default(), custom] {
            let text = toml::to_string_pretty(&s).unwrap();
            assert_eq!(toml::from_str::<Settings>(&text).unwrap(), s);
        }
    }

    #[test]
    fn missing_keys_take_defaults_and_unknown_keys_are_ignored() {
        let text = "[tuning.voicing]\na4_hz = 432.0\n[session]\nfoo = 1\n";
        let parsed: Settings = toml::from_str(text).unwrap();
        let mut expected = Settings::default();
        expected.tuning.voicing.a4_hz = 432.0;
        assert_eq!(parsed, expected);
    }

    #[test]
    fn grid_settings_convert_to_grids() {
        for (setting, grid) in [
            (GridSetting::Off, Grid::Off),
            (GridSetting::Quarter, Grid::Quarter),
            (GridSetting::Eighth, Grid::Eighth),
            (GridSetting::Sixteenth, Grid::Sixteenth),
        ] {
            assert_eq!(Grid::from(setting), grid);
            assert_eq!(GridSetting::from(grid), setting);
        }
        assert_eq!(
            toml::Value::try_from(GridSetting::Sixteenth).unwrap(),
            toml::Value::String("sixteenth".to_string())
        );
    }

    #[test]
    fn a_missing_file_gives_the_defaults_without_a_warning() {
        let dir = scratch_dir("missing");
        let (settings, warning) = Settings::load_from(&dir.join(FILE_NAME));
        assert_eq!(settings, Settings::default());
        assert_eq!(warning, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_settings_load_back() {
        let dir = scratch_dir("load-back");
        let mut settings = Settings::default();
        settings.session.input = Some("USB Mic".to_string());
        settings.tuning.voicing.a4_hz = 432.0;
        settings.tuning.export.channel = 10;
        let path = dir.join("sub").join(FILE_NAME);
        settings.save_to(&path).unwrap();
        assert!(dir.join("sub").is_dir());
        let (loaded, warning) = Settings::load_from(&path);
        assert_eq!(loaded, settings);
        assert_eq!(warning, None);
        assert!(!tmp_path(&path).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_replaces_an_existing_file() {
        let dir = scratch_dir("replace");
        let path = dir.join(FILE_NAME);
        Settings::default().save_to(&path).unwrap();
        let mut settings = Settings::default();
        settings.tuning.voicing.a4_hz = 442.0;
        settings.save_to(&path).unwrap();
        let (loaded, warning) = Settings::load_from(&path);
        assert_eq!(warning, None);
        assert_eq!(loaded.tuning.voicing.a4_hz, 442.0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_invalid_file_is_renamed_and_the_defaults_are_used() {
        let dir = scratch_dir("invalid");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        fs::write(&path, "this is = = not toml").unwrap();
        let (settings, warning) = Settings::load_from(&path);
        assert_eq!(settings, Settings::default());
        let warning = warning.expect("an invalid file gives a warning");
        assert!(
            warning.contains("settings.toml.invalid"),
            "unexpected warning: {warning}"
        );
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(dir.join("settings.toml.invalid")).unwrap(),
            "this is = = not toml"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn out_of_range_values_in_the_file_are_sanitized() {
        let dir = scratch_dir("out-of-range");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        fs::write(&path, "[tuning.voicing]\na4_hz = 1000.0\n").unwrap();
        let (settings, warning) = Settings::load_from(&path);
        assert_eq!(warning, None);
        assert_eq!(settings.tuning.voicing.a4_hz, 466.0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_default_path_ends_in_the_settings_file() {
        if let Some(path) = default_path() {
            assert_eq!(path.file_name(), Some(OsStr::new(FILE_NAME)));
            assert!(
                path.components()
                    .any(|component| component.as_os_str() == "whistlenote")
            );
        }
    }
}
