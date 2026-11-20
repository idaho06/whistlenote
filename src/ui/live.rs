//! Smoothing of live pitch frames into the note the live view shows, and the
//! live view that draws it: the note name with its cents and frequency
//! reading, a cents meter and the input level bar.

use eframe::egui;

use crate::engine::LiveFrame;
use crate::music::note::{A4_HZ, freq_to_midi_float_at, midi_to_name};
use crate::ui::GOOD_COLOR;
use crate::ui::state::AppState;

/// Default of [`LiveConfig::agree_frames`]: voiced frames that must agree in a row before a new note is shown.
pub const AGREE_FRAMES: u32 = 3;

/// Default of [`LiveConfig::hold_cents`]: distance in cents from the shown note within which the shown note stays.
pub const HOLD_CENTS: f32 = 65.0;

/// Default of [`LiveConfig::clear_s`]: seconds after the last voiced frame at which the shown note clears.
pub const CLEAR_S: f64 = 0.3;

/// Default of [`LiveConfig::min_confidence`]: confidence a frame needs to count as voiced; the same as the default minimum confidence of note segmentation.
pub const MIN_CONFIDENCE: f32 = 0.8;

/// Settings of the live note readout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveConfig {
    /// Confidence a frame needs to count as voiced.
    pub min_confidence: f32,
    /// Frequency of A4 in Hz, the reference of the shown note.
    pub a4_hz: f32,
    /// Voiced frames that must agree in a row before a new note is shown.
    pub agree_frames: u32,
    /// Distance in cents from the shown note within which the shown note stays.
    pub hold_cents: f32,
    /// Seconds after the last voiced frame at which the shown note clears.
    pub clear_s: f64,
    /// Distance in cents within which the needle shows in tune.
    pub in_tune_cents: f32,
    /// Level in dBFS at the empty end of the level bar.
    pub level_floor_db: f32,
}

impl Default for LiveConfig {
    /// Returns the default config: the constants of this module, with [`A4_HZ`] as the reference.
    fn default() -> Self {
        Self {
            min_confidence: MIN_CONFIDENCE,
            a4_hz: A4_HZ,
            agree_frames: AGREE_FRAMES,
            hold_cents: HOLD_CENTS,
            clear_s: CLEAR_S,
            in_tune_cents: IN_TUNE_CENTS,
            level_floor_db: LEVEL_FLOOR_DB,
        }
    }
}

impl From<&crate::settings::Tuning> for LiveConfig {
    /// Returns the live config of `tuning`: the reference pitch and voiced-frame confidence of its voicing, and its live values with the clear time converted from milliseconds to seconds.
    fn from(tuning: &crate::settings::Tuning) -> Self {
        Self {
            min_confidence: tuning.voicing.min_confidence as f32,
            a4_hz: tuning.voicing.a4_hz as f32,
            agree_frames: tuning.live.agree_frames,
            hold_cents: tuning.live.hold_cents as f32,
            clear_s: tuning.live.clear_ms / 1000.0,
            in_tune_cents: tuning.live.in_tune_cents as f32,
            level_floor_db: tuning.live.level_floor_db as f32,
        }
    }
}

/// The note the live view shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShownNote {
    /// Nearest MIDI note number of the reading (60 is middle C, 69 is A4).
    pub midi: i32,
    /// Distance of the reading from `midi` in cents, within ±[`LiveConfig::hold_cents`] of the shown note.
    pub cents: f32,
    /// Frequency in Hz of the voiced frame that last updated the note.
    pub freq_hz: f32,
}

/// Builds the shown note of a voiced reading: `midi` with the cents of `midi_float` relative to it and the frame frequency `hz`.
fn reading(midi: i32, midi_float: f32, hz: f32) -> ShownNote {
    ShownNote {
        midi,
        cents: (midi_float - midi as f32) * 100.0,
        freq_hz: hz,
    }
}

/// Turns live frames into a stable shown note.
///
/// A new note appears once [`LiveConfig::agree_frames`] voiced frames in a row
/// agree on the same nearest note, a shown note stays while the readings stay
/// within [`LiveConfig::hold_cents`] of it, and the shown note clears
/// [`LiveConfig::clear_s`] after the last voiced frame.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NoteSmoother {
    /// The note the live view shows.
    shown: Option<ShownNote>,
    /// Nearest MIDI note of the latest voiced frames and how many agreed in a row.
    candidate: Option<(i32, u32)>,
    /// Engine-clock time of the last voiced frame in seconds.
    last_voiced_s: f64,
    /// The config of the smoother: its smoothing and display values.
    config: LiveConfig,
}

impl NoteSmoother {
    /// Returns a smoother that uses `config`.
    pub fn with_config(config: LiveConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Folds `frame` into the smoother.
    ///
    /// A voiced frame updates the shown note while it stays within
    /// [`LiveConfig::hold_cents`] of it and otherwise counts towards
    /// [`LiveConfig::agree_frames`] agreeing frames on a new note; an unvoiced
    /// frame breaks the agreement. Once [`LiveConfig::clear_s`] passes without
    /// a voiced frame, the shown note clears.
    pub fn push(&mut self, frame: &LiveFrame) {
        let config = self.config;
        let voiced = frame
            .freq_hz
            .filter(|hz| hz.is_finite() && *hz > 0.0 && frame.confidence >= config.min_confidence);
        if let Some(hz) = voiced {
            self.last_voiced_s = frame.time_s;
            let midi_float = freq_to_midi_float_at(hz, config.a4_hz);
            let held = self.shown.filter(|shown| {
                ((midi_float - shown.midi as f32) * 100.0).abs() <= config.hold_cents
            });
            if let Some(shown) = held {
                self.shown = Some(reading(shown.midi, midi_float, hz));
                self.candidate = None;
            } else {
                let nearest = midi_float.round() as i32;
                let count = match self.candidate {
                    Some((note, count)) if note == nearest => count + 1,
                    _ => 1,
                };
                if count >= config.agree_frames {
                    self.shown = Some(reading(nearest, midi_float, hz));
                    self.candidate = None;
                } else {
                    self.candidate = Some((nearest, count));
                }
            }
        } else {
            self.candidate = None;
        }
        if self.shown.is_some() && frame.time_s - self.last_voiced_s > config.clear_s {
            self.shown = None;
            self.candidate = None;
        }
    }

    /// Returns the note the live view shows.
    pub fn shown(&self) -> Option<ShownNote> {
        self.shown
    }

    /// Returns the config of the smoother.
    pub fn config(&self) -> &LiveConfig {
        &self.config
    }

    /// Sets the config of the smoother; the shown note and the agreement stay.
    pub fn set_config(&mut self, config: LiveConfig) {
        self.config = config;
    }

    /// Returns the smoother to its default state, keeping the config.
    pub fn reset(&mut self) {
        *self = Self::with_config(self.config);
    }
}

/// Half range of the cents meter in cents; readings beyond it sit at the ends of the scale.
pub const METER_CENTS: f32 = 50.0;

/// Default of [`LiveConfig::in_tune_cents`]: distance in cents from the shown note within which the needle shows in tune.
pub const IN_TUNE_CENTS: f32 = 5.0;

/// Default of [`LiveConfig::level_floor_db`]: level in dBFS at the empty end of the level bar.
pub const LEVEL_FLOOR_DB: f32 = -60.0;

/// Cents meter ticks: the cents value, the label under the tick and the tick height in points; the zero tick is taller and only the outer ticks and the zero tick carry a label.
const METER_TICKS: [(f32, &str, f32); 5] = [
    (-50.0, "-50", 6.0),
    (-25.0, "", 6.0),
    (0.0, "0", 10.0),
    (25.0, "", 6.0),
    (50.0, "+50", 6.0),
];

/// Maps a cents reading to a fraction along the cents meter: `-METER_CENTS` is 0.0, `+METER_CENTS` is 1.0, clamped outside the range.
fn needle_fraction(cents: f32) -> f32 {
    ((cents + METER_CENTS) / (2.0 * METER_CENTS)).clamp(0.0, 1.0)
}

/// Returns whether a cents reading is within `in_tune_cents` of the shown note, which counts as in tune.
fn in_tune(cents: f32, in_tune_cents: f32) -> bool {
    cents.abs() <= in_tune_cents
}

/// Returns the level in dBFS, or `None` when `level` is not finite and positive.
fn level_db(level: f32) -> Option<f32> {
    if level.is_finite() && level > 0.0 {
        Some(20.0 * level.log10())
    } else {
        None
    }
}

/// Maps a level to a fraction of the level bar: `floor_db` is 0.0, 0 dBFS is 1.0, clamped; a missing level gives 0.0.
fn level_fraction(level: f32, floor_db: f32) -> f32 {
    match level_db(level) {
        Some(db) => ((db - floor_db) / -floor_db).clamp(0.0, 1.0),
        None => 0.0,
    }
}

/// Returns the level bar text: the level in dBFS, or "silent" when there is no level.
fn level_text(level: f32) -> String {
    match level_db(level) {
        Some(db) => format!("{db:.0} dBFS"),
        None => "silent".to_string(),
    }
}

/// Returns the reading line under the note name: the cents deviation from the shown note and the frequency in Hz.
fn reading_text(note: &ShownNote) -> String {
    format!("{:+.0} cents   {:.1} Hz", note.cents, note.freq_hz)
}

/// Width of the live view in points, shared by the cents meter and the level bar.
const VIEW_WIDTH: f32 = 320.0;

/// Height of the cents meter in points.
const METER_HEIGHT: f32 = 48.0;

/// Shows the note readout: the name of `note` in `name_size` points, its reading in cents and Hz and the cents meter with the in-tune range of `config`; "-" without a note.
pub(crate) fn note_readout(
    ui: &mut egui::Ui,
    note: Option<ShownNote>,
    name_size: f32,
    config: &LiveConfig,
) {
    let (name, reading) = match note {
        Some(note) => (midi_to_name(note.midi), reading_text(&note)),
        None => ("-".to_string(), " ".to_string()),
    };
    ui.label(egui::RichText::new(name).size(name_size).strong());
    ui.label(egui::RichText::new(reading).monospace());
    ui.add_space(8.0);
    cents_meter(ui, note.map(|note| note.cents), config.in_tune_cents);
}

/// Shows the live view: the name of the shown note, its reading in cents and Hz, the cents meter and the input level bar, centered in `ui`; when the engine does not listen, a hint to turn on Listen instead.
pub fn show(ui: &mut egui::Ui, state: &AppState) {
    ui.vertical_centered(|ui| {
        if !state.listening() {
            ui.add_space(40.0);
            ui.weak("Turn on Listen to see the whistled note.");
            return;
        }
        note_readout(ui, state.live_note(), 96.0, state.live_config());
        ui.add_space(8.0);
        ui.label("Input level");
        let level = state.latest_live().map_or(0.0, |frame| frame.level);
        ui.add(
            egui::ProgressBar::new(level_fraction(level, state.live_config().level_floor_db))
                .desired_width(VIEW_WIDTH)
                .text(level_text(level)),
        );
    });
}

/// Draws the cents meter: a base line with ticks and labels, and a needle at `cents` when there is a reading; the needle shows in tune within `in_tune_cents`.
fn cents_meter(ui: &mut egui::Ui, cents: Option<f32>, in_tune_cents: f32) {
    let (rect, _response) =
        ui.allocate_exact_size(egui::vec2(VIEW_WIDTH, METER_HEIGHT), egui::Sense::hover());
    let painter = ui.painter();
    let base_y = egui::lerp(rect.top()..=rect.bottom(), 0.6);
    let line_color = ui.visuals().text_color();
    let label_color = ui.visuals().weak_text_color();
    painter.line_segment(
        [
            egui::pos2(rect.left(), base_y),
            egui::pos2(rect.right(), base_y),
        ],
        egui::Stroke::new(1.0, line_color),
    );
    for (tick_cents, label, height) in METER_TICKS {
        let x = egui::lerp(rect.left()..=rect.right(), needle_fraction(tick_cents));
        painter.line_segment(
            [egui::pos2(x, base_y - height), egui::pos2(x, base_y)],
            egui::Stroke::new(1.0, line_color),
        );
        if !label.is_empty() {
            painter.text(
                egui::pos2(x, base_y + 4.0),
                egui::Align2::CENTER_TOP,
                label,
                egui::FontId::proportional(11.0),
                label_color,
            );
        }
    }
    if let Some(cents) = cents {
        let x = egui::lerp(rect.left()..=rect.right(), needle_fraction(cents));
        let color = if in_tune(cents, in_tune_cents) {
            GOOD_COLOR
        } else {
            ui.visuals().warn_fg_color
        };
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, base_y)],
            egui::Stroke::new(3.0, color),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::assert_close;
    use crate::music::note::midi_to_freq;
    use crate::music::segment::SegmentConfig;

    /// Returns a voiced frame at `time_s` with the pitch of `midi` at the 440 Hz reference.
    fn voiced(time_s: f64, midi: f32) -> LiveFrame {
        LiveFrame {
            time_s,
            freq_hz: Some(midi_to_freq(midi)),
            confidence: 0.95,
            level: 0.01,
        }
    }

    /// Returns an unvoiced frame at `time_s`.
    fn unvoiced(time_s: f64) -> LiveFrame {
        LiveFrame {
            time_s,
            freq_hz: None,
            confidence: 0.0,
            level: 0.01,
        }
    }

    #[test]
    fn three_agreeing_frames_show_a_note() {
        let mut smoother = NoteSmoother::default();
        smoother.push(&voiced(0.00, 69.0));
        smoother.push(&voiced(0.01, 69.0));
        assert_eq!(smoother.shown(), None);
        smoother.push(&voiced(0.02, 69.0));
        let shown = smoother.shown().expect("the third frame shows a note");
        assert_eq!(shown.midi, 69);
        assert_close(shown.cents as f64, 0.0, 0.5);
        assert_close(shown.freq_hz as f64, 440.0, 0.5);
    }

    #[test]
    fn an_unvoiced_frame_breaks_the_agreement() {
        let mut smoother = NoteSmoother::default();
        for frame in [
            voiced(0.00, 69.0),
            voiced(0.01, 69.0),
            unvoiced(0.02),
            voiced(0.03, 69.0),
            voiced(0.04, 69.0),
        ] {
            smoother.push(&frame);
        }
        assert_eq!(smoother.shown(), None);
        smoother.push(&voiced(0.05, 69.0));
        assert_eq!(smoother.shown().map(|note| note.midi), Some(69));
    }

    #[test]
    fn a_low_confidence_frame_counts_as_unvoiced() {
        let mut smoother = NoteSmoother::default();
        let mut low_confidence = voiced(0.02, 69.0);
        low_confidence.confidence = 0.5;
        for frame in [
            voiced(0.00, 69.0),
            voiced(0.01, 69.0),
            low_confidence,
            voiced(0.03, 69.0),
            voiced(0.04, 69.0),
        ] {
            smoother.push(&frame);
        }
        assert_eq!(smoother.shown(), None);
    }

    #[test]
    fn a_shown_note_stays_within_hold_cents() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        for time_s in [0.03, 0.04, 0.05, 0.06, 0.07] {
            smoother.push(&voiced(time_s, 69.6));
            let shown = smoother.shown().expect("the note stays shown");
            assert_eq!(shown.midi, 69);
            assert_close(shown.cents as f64, 60.0, 0.5);
        }
        for time_s in [0.08, 0.09] {
            smoother.push(&voiced(time_s, 69.7));
            assert_eq!(smoother.shown().map(|note| note.midi), Some(69));
        }
        smoother.push(&voiced(0.10, 69.7));
        let shown = smoother
            .shown()
            .expect("the third frame shows the new note");
        assert_eq!(shown.midi, 70);
        assert_close(shown.cents as f64, -30.0, 0.5);
    }

    #[test]
    fn a_shown_note_stays_within_hold_cents_downwards() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        smoother.push(&voiced(0.03, 68.4));
        let shown = smoother.shown().expect("the note stays shown");
        assert_eq!(shown.midi, 69);
        assert_close(shown.cents as f64, -60.0, 0.5);
    }

    #[test]
    fn the_shown_note_clears_after_clear_s_without_voiced_frames() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        assert!(smoother.shown().is_some());
        for time_s in [0.12, 0.22, 0.32] {
            smoother.push(&unvoiced(time_s));
            assert!(smoother.shown().is_some(), "still shown at {time_s}");
        }
        smoother.push(&unvoiced(0.33));
        assert_eq!(smoother.shown(), None);
    }

    #[test]
    fn reset_returns_to_the_default_state() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        assert!(smoother.shown().is_some());
        smoother.reset();
        assert_eq!(smoother.shown(), None);
        assert_eq!(smoother, NoteSmoother::default());
    }

    #[test]
    fn the_default_config_matches_the_segment_config_default() {
        assert_eq!(
            LiveConfig::default().min_confidence,
            SegmentConfig::default().min_confidence
        );
        assert_eq!(LiveConfig::default().a4_hz, SegmentConfig::default().a4_hz);
    }

    #[test]
    fn the_config_from_default_tuning_is_the_default_config() {
        assert_eq!(
            LiveConfig::from(&crate::settings::Tuning::default()),
            LiveConfig::default()
        );
    }

    #[test]
    fn one_agreeing_frame_is_enough_with_agree_frames_1() {
        let mut smoother = NoteSmoother::with_config(LiveConfig {
            agree_frames: 1,
            ..Default::default()
        });
        smoother.push(&voiced(0.00, 69.0));
        let shown = smoother.shown().expect("one frame shows a note");
        assert_eq!(shown.midi, 69);
    }

    #[test]
    fn a_432_hz_reference_reads_432_hz_as_a4_in_tune() {
        let mut smoother = NoteSmoother::with_config(LiveConfig {
            a4_hz: 432.0,
            ..Default::default()
        });
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&LiveFrame {
                time_s,
                freq_hz: Some(432.0),
                confidence: 0.95,
                level: 0.01,
            });
        }
        let shown = smoother.shown().expect("three frames show a note");
        assert_eq!(shown.midi, 69);
        assert!(shown.cents.abs() < 0.5, "cents: {}", shown.cents);
    }

    #[test]
    fn a_longer_clear_time_keeps_the_note() {
        let mut smoother = NoteSmoother::with_config(LiveConfig {
            clear_s: 1.0,
            ..Default::default()
        });
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        assert!(smoother.shown().is_some());
        smoother.push(&unvoiced(0.6));
        assert!(smoother.shown().is_some(), "still shown at 0.6");
        smoother.push(&unvoiced(1.1));
        assert_eq!(smoother.shown(), None);
    }

    #[test]
    fn reset_keeps_the_config() {
        let mut smoother = NoteSmoother::default();
        for time_s in [0.00, 0.01, 0.02] {
            smoother.push(&voiced(time_s, 69.0));
        }
        let config = LiveConfig {
            clear_s: 1.0,
            ..Default::default()
        };
        smoother.set_config(config);
        smoother.reset();
        assert_eq!(smoother.config(), &config);
        assert_eq!(smoother.shown(), None);
    }

    #[test]
    fn a_higher_min_confidence_ignores_frames() {
        let mut smoother = NoteSmoother::with_config(LiveConfig {
            min_confidence: 0.99,
            ..Default::default()
        });
        for time_s in [0.00, 0.01, 0.02, 0.03] {
            smoother.push(&voiced(time_s, 69.0));
        }
        assert_eq!(smoother.shown(), None);
    }

    #[test]
    fn needle_fraction_maps_the_meter_range() {
        assert_close(needle_fraction(-METER_CENTS) as f64, 0.0, 1e-6);
        assert_close(needle_fraction(0.0) as f64, 0.5, 1e-6);
        assert_close(needle_fraction(METER_CENTS) as f64, 1.0, 1e-6);
        assert_close(needle_fraction(HOLD_CENTS) as f64, 1.0, 1e-6);
        assert_close(needle_fraction(-HOLD_CENTS) as f64, 0.0, 1e-6);
        assert_close(needle_fraction(25.0) as f64, 0.75, 1e-6);
    }

    #[test]
    fn in_tune_checks_the_distance_to_the_shown_note() {
        assert!(in_tune(IN_TUNE_CENTS, IN_TUNE_CENTS));
        assert!(in_tune(-IN_TUNE_CENTS, IN_TUNE_CENTS));
        assert!(!in_tune(IN_TUNE_CENTS + 0.1, IN_TUNE_CENTS));
        assert!(!in_tune(-IN_TUNE_CENTS - 0.1, IN_TUNE_CENTS));
        assert!(in_tune(0.0, IN_TUNE_CENTS));
    }

    #[test]
    fn level_db_converts_a_level_to_dbfs() {
        assert_close(level_db(1.0).expect("1.0 has a level") as f64, 0.0, 1e-3);
        assert_close(
            level_db(0.01).expect("0.01 has a level") as f64,
            -40.0,
            1e-3,
        );
        assert_eq!(level_db(0.0), None);
        assert_eq!(level_db(-1.0), None);
        assert_eq!(level_db(f32::NAN), None);
    }

    #[test]
    fn level_fraction_maps_dbfs_to_the_bar() {
        assert_close(level_fraction(1.0, LEVEL_FLOOR_DB) as f64, 1.0, 1e-6);
        assert_close(level_fraction(0.001, LEVEL_FLOOR_DB) as f64, 0.0, 1e-6);
        assert_close(level_fraction(0.01, LEVEL_FLOOR_DB) as f64, 1.0 / 3.0, 1e-3);
        assert_close(level_fraction(0.0001, LEVEL_FLOOR_DB) as f64, 0.0, 1e-6);
        assert_close(level_fraction(2.0, LEVEL_FLOOR_DB) as f64, 1.0, 1e-6);
        assert_close(level_fraction(0.0, LEVEL_FLOOR_DB) as f64, 0.0, 1e-6);
    }

    #[test]
    fn level_text_shows_dbfs_or_silent() {
        assert_eq!(level_text(0.01), "-40 dBFS");
        assert_eq!(level_text(0.0), "silent");
    }

    #[test]
    fn reading_text_shows_cents_and_hz() {
        let note = ShownNote {
            midi: 69,
            cents: 12.4,
            freq_hz: 440.0,
        };
        assert_eq!(reading_text(&note), "+12 cents   440.0 Hz");
        let flat = ShownNote {
            midi: 69,
            cents: -3.0,
            freq_hz: 440.0,
        };
        assert!(reading_text(&flat).starts_with("-3 cents"));
    }
}
