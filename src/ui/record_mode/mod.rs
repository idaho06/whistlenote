//! The Record tab, in three parts: the recording state — the phase of a recording, the grid and metronome settings, the segmentation and export settings, the latest beat, the live frames and preview of the running recording and the finished take — the finished or preview take of a recording with the rows of its notes table, and the view that draws the tab.

mod take;
mod view;

pub use take::{Row, Take, table_rows};
pub use view::show;

use crate::engine::{BeatPhase, EngineCommand, EngineEvent, LiveFrame};
use crate::export::midi::MidiOptions;
use crate::metronome::BEATS_PER_BAR;
use crate::music::quantize::Grid;
use crate::music::segment::{PitchFrame, SegmentConfig};

/// Phase of a recording as the Record tab sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordPhase {
    /// No recording runs; the tab waits for a start.
    #[default]
    Idle,
    /// The count-in bar plays; the recording has not begun.
    CountIn,
    /// The recording runs and collects pitch frames.
    Recording,
    /// A stop is sent; the tab waits for the engine's answer, the finished take or the abort.
    Stopping,
}

/// Live frames between two rebuilds of the preview of the running recording; about 0.1 s.
pub const PREVIEW_EVERY_FRAMES: usize = 10;

/// The recording state of the Record tab.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordState {
    /// Phase of the running recording.
    phase: RecordPhase,
    /// Grid the finished take and the preview are quantized onto.
    grid: Grid,
    /// Segmentation config takes and the preview are built with.
    segment: SegmentConfig,
    /// MIDI export options takes and the preview are exported with.
    midi: MidiOptions,
    /// Whether recording beats are audible.
    metronome: bool,
    /// Tempo of the running recording, locked at start.
    bpm: f64,
    /// Index and phase of the latest beat of the running recording.
    last_beat: Option<(u32, BeatPhase)>,
    /// The latest finished take.
    take: Option<Take>,
    /// Count of the finished takes.
    take_generation: u64,
    /// Beat 0 of the running recording, once the engine reports the start.
    origin_s: Option<f64>,
    /// Live frames of the running recording, from its start.
    live_frames: Vec<PitchFrame>,
    /// Live frames pushed since the preview was last rebuilt.
    unprocessed: usize,
    /// Notes of the running recording so far.
    preview: Option<Take>,
}

impl Default for RecordState {
    fn default() -> Self {
        Self {
            phase: RecordPhase::Idle,
            grid: Grid::default(),
            segment: SegmentConfig::default(),
            midi: MidiOptions::default(),
            metronome: true,
            bpm: 0.0,
            last_beat: None,
            take: None,
            take_generation: 0,
            origin_s: None,
            live_frames: Vec::new(),
            unprocessed: 0,
            preview: None,
        }
    }
}

impl RecordState {
    /// Returns the phase of the running recording.
    pub fn phase(&self) -> RecordPhase {
        self.phase
    }

    /// Returns the grid the finished take is quantized onto.
    pub fn grid(&self) -> Grid {
        self.grid
    }

    /// Returns whether recording beats are audible.
    pub fn metronome(&self) -> bool {
        self.metronome
    }

    /// Returns the latest finished take, if there is one.
    pub fn take(&self) -> Option<&Take> {
        self.take.as_ref()
    }

    /// Returns the latest finished take mutably, if there is one.
    pub fn take_mut(&mut self) -> Option<&mut Take> {
        self.take.as_mut()
    }

    /// Returns how many takes have been finished.
    pub fn take_generation(&self) -> u64 {
        self.take_generation
    }

    /// Puts back a copy of the finished take taken at `generation`; a take recorded since then is kept.
    pub fn restore_take(&mut self, generation: u64, take: Take) -> bool {
        if generation == self.take_generation {
            self.take = Some(take);
            true
        } else {
            false
        }
    }

    /// Returns the preview of the running recording — its notes so far — once one has been built.
    pub fn preview(&self) -> Option<&Take> {
        self.preview.as_ref()
    }

    /// Returns the index and phase of the latest beat of the running recording.
    pub fn last_beat(&self) -> Option<(u32, BeatPhase)> {
        self.last_beat
    }

    /// Returns whether no recording runs.
    pub fn is_idle(&self) -> bool {
        self.phase == RecordPhase::Idle
    }

    /// Returns the tempo of the running recording, or `None` while idle.
    pub fn recording_bpm(&self) -> Option<f64> {
        if self.is_idle() { None } else { Some(self.bpm) }
    }

    /// Stores `grid` and re-quantizes the finished take and the preview onto it, if there are any.
    pub fn set_grid(&mut self, grid: Grid) {
        self.grid = grid;
        for take in [&mut self.take, &mut self.preview].into_iter().flatten() {
            take.set_grid(grid);
        }
    }

    /// Returns the segmentation config takes and the preview are built with.
    pub fn segment_config(&self) -> SegmentConfig {
        self.segment
    }

    /// Stores `segment` as the segmentation config and applies it to the finished take and the preview, if there are any, which re-segments them and resets the take's edits; a `segment` equal to the stored config changes nothing, so the take keeps its edits and its own merge threshold.
    pub fn set_segment_config(&mut self, segment: SegmentConfig) {
        if segment == self.segment {
            return;
        }
        self.segment = segment;
        for take in [&mut self.take, &mut self.preview].into_iter().flatten() {
            take.set_segment_config(segment);
        }
    }

    /// Returns the MIDI export options takes and the preview are exported with.
    pub fn midi_options(&self) -> MidiOptions {
        self.midi
    }

    /// Stores `midi` as the MIDI export options and applies them to the finished take and the preview, if there are any; the notes stay unchanged.
    pub fn set_midi_options(&mut self, midi: MidiOptions) {
        self.midi = midi;
        for take in [&mut self.take, &mut self.preview].into_iter().flatten() {
            take.set_midi_options(midi);
        }
    }

    /// Stores whether recording beats are audible; the setting changes only while idle and is locked during a recording.
    pub fn set_metronome(&mut self, on: bool) {
        if self.is_idle() {
            self.metronome = on;
        }
    }

    /// Locks `bpm`, clears the latest beat and the live frames and preview of the running recording, enters the count-in and returns the command that starts the recording; returns `None` and changes nothing unless the tab is idle and `bpm` is finite and positive. The finished take is kept.
    pub fn start(&mut self, bpm: f64) -> Option<EngineCommand> {
        if !self.is_idle() || !bpm.is_finite() || bpm <= 0.0 {
            return None;
        }
        self.bpm = bpm;
        self.clear_run();
        self.phase = RecordPhase::CountIn;
        Some(EngineCommand::StartRecording {
            bpm,
            metronome: self.metronome,
        })
    }

    /// Enters the stopping phase and returns the command that ends the recording; returns `None` while idle or already stopping.
    pub fn stop(&mut self) -> Option<EngineCommand> {
        if !self.is_running() {
            return None;
        }
        self.phase = RecordPhase::Stopping;
        Some(EngineCommand::StopRecording)
    }

    /// Returns to idle without a take when a count-in or recording runs, because the engine discards a recording without an event when listening stops or a device changes; the live frames and the preview are cleared and the finished take is kept. While stopping it does nothing, because the engine still answers the sent stop. While idle it does nothing.
    pub fn cancel(&mut self) {
        if self.is_running() {
            self.end_run();
        }
    }

    /// Folds `event` into the state: a beat while a recording runs updates the latest beat, a start moves the count-in into the recording and stores its origin, a finish stores the take, clears the live frames and the preview and returns to idle, an abort returns to idle and keeps the earlier take, an error changes nothing. Other events change nothing.
    pub fn apply_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::Beat { index, phase, .. } if !self.is_idle() => {
                self.last_beat = Some((*index, *phase));
            }
            EngineEvent::RecordingStarted { origin_s } if self.phase == RecordPhase::CountIn => {
                self.phase = RecordPhase::Recording;
                self.origin_s = Some(*origin_s);
            }
            EngineEvent::RecordingFinished { frames, origin_s } if !self.is_idle() => {
                self.take = Some(Take::with_config(
                    frames.clone(),
                    *origin_s,
                    self.bpm,
                    self.grid,
                    self.segment,
                    self.midi,
                ));
                self.take_generation = self.take_generation.wrapping_add(1);
                self.end_run();
            }
            EngineEvent::RecordingAborted if !self.is_idle() => self.end_run(),
            _ => {}
        }
    }

    /// Folds a live frame of the engine into the running recording: while a recording runs, the frame is collected as a pitch frame and every [`PREVIEW_EVERY_FRAMES`] collected frames the preview is rebuilt from all collected frames; while idle or stopping the frame is ignored, and no preview is built before the recording start arrived.
    pub fn push_live(&mut self, frame: &LiveFrame) {
        if !self.is_running() {
            return;
        }
        self.live_frames.push(PitchFrame {
            time_s: frame.time_s,
            freq_hz: frame.freq_hz,
            confidence: frame.confidence,
        });
        self.unprocessed += 1;
        if let Some(origin_s) = self.origin_s
            && self.unprocessed >= PREVIEW_EVERY_FRAMES
        {
            self.preview = Some(Take::with_config(
                self.live_frames.clone(),
                origin_s,
                self.bpm,
                self.grid,
                self.segment,
                self.midi,
            ));
            self.unprocessed = 0;
        }
    }

    /// Returns the beat line of the tab: empty while idle, "Stopping" while stopping, and the count-in or bar position of the latest beat otherwise.
    pub fn beat_text(&self) -> String {
        match self.phase {
            RecordPhase::Idle => String::new(),
            RecordPhase::Stopping => "Stopping".to_string(),
            RecordPhase::CountIn | RecordPhase::Recording => match self.last_beat {
                Some((index, BeatPhase::CountIn)) => {
                    format!("Count-in {}/{}", index % BEATS_PER_BAR + 1, BEATS_PER_BAR)
                }
                Some((index, BeatPhase::Recording)) => format!(
                    "Bar {} beat {}",
                    index / BEATS_PER_BAR + 1,
                    index % BEATS_PER_BAR + 1
                ),
                Some((_, BeatPhase::Free)) | None => "Count-in".to_string(),
            },
        }
    }

    /// Returns whether the count-in or the recording runs.
    fn is_running(&self) -> bool {
        matches!(self.phase, RecordPhase::CountIn | RecordPhase::Recording)
    }

    /// Returns to idle and clears the per-recording fields; the finished take is kept.
    fn end_run(&mut self) {
        self.phase = RecordPhase::Idle;
        self.clear_run();
    }

    /// Clears the per-recording fields: the latest beat, the origin, the live frames, the unprocessed count and the preview.
    fn clear_run(&mut self) {
        self.last_beat = None;
        self.origin_s = None;
        self.live_frames.clear();
        self.unprocessed = 0;
        self.preview = None;
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{RecordState, Take};
    use crate::engine::{BeatPhase, EngineEvent, LiveFrame};
    use crate::music::quantize::Grid;
    use crate::music::segment::PitchFrame;

    /// Returns a beat event with `index` and `phase`, accented every fourth beat, at time 0.
    pub(crate) fn beat(index: u32, phase: BeatPhase) -> EngineEvent {
        EngineEvent::Beat {
            index,
            accent: index.is_multiple_of(4),
            phase,
            time_s: 0.0,
        }
    }

    /// Returns voiced frames every 0.01 s from `start_s` for `len_s` seconds at `hz` Hz with confidence 0.95.
    pub(crate) fn tone_frames(start_s: f64, len_s: f64, hz: f32) -> Vec<PitchFrame> {
        let count = (len_s / 0.01).round() as usize;
        (0..count)
            .map(|i| PitchFrame {
                time_s: start_s + i as f64 * 0.01,
                freq_hz: Some(hz),
                confidence: 0.95,
            })
            .collect()
    }

    /// Returns `frames` as live frames with level 0.01.
    pub(crate) fn live_frames(frames: &[PitchFrame]) -> Vec<LiveFrame> {
        frames
            .iter()
            .map(|frame| LiveFrame {
                time_s: frame.time_s,
                freq_hz: frame.freq_hz,
                confidence: frame.confidence,
                level: 0.01,
            })
            .collect()
    }

    /// Runs a full recording at 120 BPM with `frames` and origin 1.0 and returns the state and the frames.
    pub(crate) fn recorded_state(frames: Vec<PitchFrame>) -> (RecordState, Vec<PitchFrame>) {
        let mut state = RecordState::default();
        state.start(120.0);
        for i in 0..4 {
            state.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        state.apply_event(&beat(0, BeatPhase::Recording));
        state.stop();
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: frames.clone(),
            origin_s: 1.0,
        });
        (state, frames)
    }

    /// Builds a take of three tones at 1.0, 2.0 and 3.0 s (A4, C5, E5) recorded at 120 BPM with beat 0 at 1.0 s on the sixteenth grid; the notes sit at beats 0, 2 and 4.
    pub(crate) fn three_note_take() -> Take {
        let mut frames = tone_frames(1.0, 0.5, 440.0);
        frames.extend(tone_frames(2.0, 0.5, 523.25));
        frames.extend(tone_frames(3.0, 0.5, 659.26));
        Take::new(frames, 1.0, 120.0, Grid::Sixteenth)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{beat, live_frames, recorded_state, tone_frames};
    use super::*;
    use crate::export::csv::to_csv;
    use crate::export::midi::{MidiOptions, to_midi_bytes};
    use crate::music::assert_close;
    use crate::music::quantize::quantize;
    use crate::music::segment::{SegmentConfig, segment};

    /// Pushes `frames` as live frames into `state`.
    fn push_live_frames(state: &mut RecordState, frames: &[PitchFrame]) {
        for frame in live_frames(frames) {
            state.push_live(&frame);
        }
    }

    #[test]
    fn the_default_state_is_idle() {
        let state = RecordState::default();
        assert_eq!(state.phase(), RecordPhase::Idle);
        assert!(state.is_idle());
        assert_eq!(state.grid(), Grid::Sixteenth);
        assert!(state.metronome());
        assert!(state.take().is_none());
        assert_eq!(state.last_beat(), None);
        assert_eq!(state.beat_text(), "");
        assert_eq!(state.recording_bpm(), None);
    }

    #[test]
    fn start_locks_the_tempo_and_sends_the_command() {
        let mut state = RecordState::default();
        assert_eq!(
            state.start(100.0),
            Some(EngineCommand::StartRecording {
                bpm: 100.0,
                metronome: true,
            })
        );
        assert_eq!(state.phase(), RecordPhase::CountIn);
        assert_eq!(state.recording_bpm(), Some(100.0));
        assert_eq!(state.beat_text(), "Count-in");
        assert_eq!(state.start(120.0), None, "a second start is refused");
        assert_eq!(state.recording_bpm(), Some(100.0));
    }

    #[test]
    fn an_invalid_tempo_is_refused() {
        let mut state = RecordState::default();
        for bpm in [f64::NAN, 0.0, -120.0, f64::INFINITY] {
            assert_eq!(state.start(bpm), None);
        }
        assert!(state.is_idle());
        assert_eq!(state.recording_bpm(), None);
    }

    #[test]
    fn count_in_beats_show_the_count_in_position() {
        let mut state = RecordState::default();
        state.start(120.0);
        for (i, text) in [
            "Count-in 1/4",
            "Count-in 2/4",
            "Count-in 3/4",
            "Count-in 4/4",
        ]
        .iter()
        .enumerate()
        {
            state.apply_event(&beat(i as u32, BeatPhase::CountIn));
            assert_eq!(state.beat_text(), *text);
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 2.0 });
        assert_eq!(state.phase(), RecordPhase::Recording);
    }

    #[test]
    fn recording_beats_show_bar_and_beat() {
        let mut state = RecordState::default();
        state.start(120.0);
        for i in 0..4 {
            state.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 2.0 });
        for (index, text) in [
            (0, "Bar 1 beat 1"),
            (3, "Bar 1 beat 4"),
            (4, "Bar 2 beat 1"),
            (9, "Bar 3 beat 2"),
        ] {
            state.apply_event(&beat(index, BeatPhase::Recording));
            assert_eq!(state.beat_text(), text);
        }
        state.apply_event(&beat(12, BeatPhase::Free));
        assert_eq!(state.beat_text(), "Count-in");
    }

    #[test]
    fn stop_sends_the_stop_command() {
        let mut state = RecordState::default();
        assert_eq!(state.stop(), None, "stop while idle does nothing");
        state.start(120.0);
        for i in 0..4 {
            state.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 2.0 });
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
        assert_eq!(state.phase(), RecordPhase::Stopping);
        assert_eq!(state.beat_text(), "Stopping");
        assert_eq!(state.stop(), None, "a second stop does nothing");
    }

    #[test]
    fn a_full_recording_produces_a_take() {
        let frames = tone_frames(1.0, 0.6, 440.0);
        let (state, frames) = recorded_state(frames);
        assert!(state.is_idle());
        let take = state.take().expect("a take");
        assert_eq!(take.bpm(), 120.0);
        assert_eq!(take.origin_s(), 1.0);
        assert_eq!(take.frame_count(), 60);
        let expected = quantize(
            &segment(&frames, &SegmentConfig::default()),
            120.0,
            Grid::Sixteenth,
            1.0,
        );
        assert_eq!(take.notes(), expected.as_slice());
        assert!(!take.notes().is_empty());
        assert!(take.notes().iter().all(|note| note.midi == 69));
    }

    #[test]
    fn set_grid_requantizes_the_take() {
        let frames = tone_frames(1.0, 0.6, 440.0);
        let (mut state, frames) = recorded_state(frames);
        state.set_grid(Grid::Quarter);
        assert_eq!(state.grid(), Grid::Quarter);
        let take = state.take().expect("a take");
        let expected = quantize(
            &segment(&frames, &SegmentConfig::default()),
            120.0,
            Grid::Quarter,
            1.0,
        );
        assert_eq!(take.notes(), expected.as_slice());
    }

    #[test]
    fn the_take_exports_midi_and_csv() {
        let (state, _) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        let take = state.take().expect("a take");
        assert_eq!(take.midi_bytes(), to_midi_bytes(take.notes(), 120.0));
        assert_eq!(take.csv(), to_csv(take.notes()));
    }

    #[test]
    fn stopping_during_the_count_in_keeps_the_earlier_take() {
        let (mut state, _) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        let take_before = state.take().cloned();
        assert!(take_before.is_some());
        state.start(120.0);
        state.apply_event(&beat(0, BeatPhase::CountIn));
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
        state.apply_event(&EngineEvent::RecordingAborted);
        assert!(state.is_idle());
        assert_eq!(state.last_beat(), None);
        assert_eq!(state.take(), take_before.as_ref());
    }

    #[test]
    fn an_error_during_the_count_in_keeps_the_count_in() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&EngineEvent::Error("device busy".to_string()));
        assert_eq!(
            state.phase(),
            RecordPhase::CountIn,
            "an error before the first count-in beat changes nothing"
        );
        state.apply_event(&beat(0, BeatPhase::CountIn));
        state.apply_event(&EngineEvent::Error("device busy".to_string()));
        assert_eq!(
            state.phase(),
            RecordPhase::CountIn,
            "an error after a count-in beat changes nothing"
        );
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
    }

    #[test]
    fn a_failed_start_returns_to_idle_and_keeps_the_take() {
        let (mut state, _) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        let take_before = state.take().cloned();
        assert!(take_before.is_some());
        state.start(120.0);
        state.apply_event(&EngineEvent::Error("no device".to_string()));
        state.apply_event(&EngineEvent::RecordingAborted);
        assert!(state.is_idle());
        assert_eq!(state.take(), take_before.as_ref());
    }

    #[test]
    fn a_stop_before_a_start_error_returns_to_idle() {
        let mut state = RecordState::default();
        state.start(120.0);
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
        state.apply_event(&EngineEvent::Error("no device".to_string()));
        state.apply_event(&EngineEvent::RecordingAborted);
        assert!(state.is_idle());
    }

    #[test]
    fn cancel_while_stopping_waits_for_the_engine() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&beat(0, BeatPhase::CountIn));
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
        state.cancel();
        assert_eq!(
            state.phase(),
            RecordPhase::Stopping,
            "a cancel while stopping waits for the answer to the stop"
        );
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: tone_frames(1.0, 0.6, 440.0),
            origin_s: 1.0,
        });
        assert!(state.is_idle());
        let take = state.take().expect("a take");
        assert_eq!(take.notes().len(), 1);
    }

    #[test]
    fn cancel_returns_to_idle_and_keeps_the_take() {
        let (mut state, frames) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        let take_before = state.take().cloned();
        state.start(90.0);
        state.apply_event(&beat(0, BeatPhase::CountIn));
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 5.0 });
        state.cancel();
        assert!(state.is_idle());
        assert_eq!(state.last_beat(), None);
        assert_eq!(state.take(), take_before.as_ref());
        state.apply_event(&EngineEvent::RecordingFinished {
            frames,
            origin_s: 5.0,
        });
        assert_eq!(
            state.take(),
            take_before.as_ref(),
            "a finish while idle is ignored"
        );
    }

    #[test]
    fn the_metronome_setting_is_locked_during_a_recording() {
        let mut state = RecordState::default();
        state.set_metronome(false);
        assert!(!state.metronome());
        assert_eq!(
            state.start(100.0),
            Some(EngineCommand::StartRecording {
                bpm: 100.0,
                metronome: false,
            })
        );
        state.set_metronome(true);
        assert!(
            !state.metronome(),
            "the setting is locked during the count-in"
        );
    }

    #[test]
    fn events_while_idle_are_ignored() {
        let mut state = RecordState::default();
        state.apply_event(&beat(2, BeatPhase::Recording));
        state.apply_event(&beat(0, BeatPhase::CountIn));
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: Vec::new(),
            origin_s: 0.0,
        });
        state.apply_event(&EngineEvent::Error("x".to_string()));
        state.apply_event(&EngineEvent::RecordingAborted);
        assert_eq!(state.last_beat(), None);
        assert!(state.is_idle());
        assert!(state.take().is_none());
        assert_eq!(state.beat_text(), "");
    }

    #[test]
    fn take_mut_edits_the_stored_take() {
        let (mut state, _) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        state.take_mut().expect("a take").delete(0);
        assert!(state.take().expect("a take").notes().is_empty());
    }

    #[test]
    fn live_frames_are_ignored_while_idle() {
        let mut state = RecordState::default();
        push_live_frames(&mut state, &tone_frames(0.0, 0.5, 440.0));
        assert!(state.preview().is_none());
    }

    #[test]
    fn the_preview_is_built_once_the_recording_started() {
        let mut state = RecordState::default();
        state.start(120.0);
        push_live_frames(&mut state, &tone_frames(0.0, 0.05, 440.0));
        assert!(
            state.preview().is_none(),
            "frames of the count-in are collected but no preview is built without an origin"
        );
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        assert!(state.preview().is_none());
        push_live_frames(&mut state, &tone_frames(0.05, 0.05, 440.0));
        assert!(
            state.preview().is_some(),
            "the tenth collected frame builds the preview"
        );
    }

    #[test]
    fn a_tone_gives_a_one_note_preview_starting_at_beat_zero() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        push_live_frames(&mut state, &tone_frames(1.0, 0.6, 440.0));
        let preview = state.preview().expect("a preview after 60 frames");
        assert_eq!(preview.notes().len(), 1);
        assert_eq!(preview.notes()[0].midi, 69);
        assert_close(preview.notes()[0].start_beats, 0.0, 1e-9);
    }

    #[test]
    fn stopping_ignores_live_frames_and_the_finish_clears_the_preview() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        push_live_frames(&mut state, &tone_frames(1.0, 0.2, 440.0));
        assert!(state.preview().is_some());
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
        push_live_frames(&mut state, &tone_frames(1.2, 0.2, 440.0));
        let preview = state.preview().expect("the preview survives the stop");
        assert_eq!(
            preview.frame_count(),
            20,
            "frames pushed after the stop are ignored"
        );
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: tone_frames(1.0, 0.6, 440.0),
            origin_s: 1.0,
        });
        assert!(state.is_idle());
        assert!(state.preview().is_none(), "the finish clears the preview");
        let take = state.take().expect("a take");
        assert_eq!(take.frame_count(), 60);
    }

    #[test]
    fn cancel_clears_the_preview() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        push_live_frames(&mut state, &tone_frames(1.0, 0.2, 440.0));
        assert!(state.preview().is_some());
        state.cancel();
        assert!(state.is_idle());
        assert!(state.preview().is_none());
    }

    #[test]
    fn a_new_recording_starts_without_a_preview() {
        let (mut state, _) = recorded_state(tone_frames(1.0, 0.6, 440.0));
        state.start(120.0);
        assert!(state.preview().is_none());
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 5.0 });
        push_live_frames(&mut state, &tone_frames(5.0, 0.2, 440.0));
        let preview = state.preview().expect("a preview");
        assert_eq!(
            preview.frame_count(),
            20,
            "only the frames of the new recording are collected"
        );
    }

    #[test]
    fn set_grid_requantizes_the_preview() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        let frames = tone_frames(1.0, 0.6, 440.0);
        push_live_frames(&mut state, &frames);
        state.set_grid(Grid::Quarter);
        let preview = state.preview().expect("a preview");
        let expected = quantize(
            &segment(&frames, &SegmentConfig::default()),
            120.0,
            Grid::Quarter,
            1.0,
        );
        assert_eq!(preview.notes(), expected.as_slice());
    }

    #[test]
    fn a_finished_recording_uses_the_stored_configs() {
        let mut state = RecordState::default();
        state.set_segment_config(SegmentConfig {
            min_note_s: 1.0,
            ..SegmentConfig::default()
        });
        state.set_midi_options(MidiOptions {
            velocity: 64,
            channel: 9,
        });
        state.start(120.0);
        for i in 0..4 {
            state.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state.apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        state.apply_event(&beat(0, BeatPhase::Recording));
        state.stop();
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: tone_frames(1.0, 0.5, 440.0),
            origin_s: 1.0,
        });
        let take = state.take().expect("a take");
        assert!(take.notes().is_empty());
        assert_eq!(
            take.midi_options(),
            MidiOptions {
                velocity: 64,
                channel: 9,
            }
        );
    }

    #[test]
    fn set_segment_config_resegments_the_take() {
        let (mut state, _) = recorded_state(tone_frames(1.0, 0.5, 440.0));
        assert_eq!(state.take().expect("a take").notes().len(), 1);
        state.set_segment_config(SegmentConfig {
            min_note_s: 1.0,
            ..SegmentConfig::default()
        });
        assert!(state.take().expect("a take").notes().is_empty());
    }

    #[test]
    fn an_unchanged_segment_config_keeps_the_edits() {
        let (mut state, _) = recorded_state(tone_frames(1.0, 0.5, 440.0));
        state.take_mut().expect("a take").delete(0);
        assert!(state.take().expect("a take").is_edited());
        state.set_segment_config(SegmentConfig::default());
        assert!(state.take().expect("a take").is_edited());
    }

    #[test]
    fn each_finished_take_raises_the_generation() {
        let state = RecordState::default();
        assert_eq!(state.take_generation(), 0);
        let (state, _) = recorded_state(tone_frames(1.0, 0.5, 440.0));
        assert_eq!(state.take_generation(), 1);
    }

    #[test]
    fn restore_take_needs_the_same_generation() {
        let (mut state, _) = recorded_state(tone_frames(1.0, 0.5, 440.0));
        assert_eq!(state.take_generation(), 1);
        let snapshot = state.take().expect("a take").clone();
        state.set_segment_config(SegmentConfig {
            min_note_s: 1.0,
            ..SegmentConfig::default()
        });
        assert!(state.take().expect("a take").notes().is_empty());
        assert!(state.restore_take(1, snapshot.clone()));
        assert_eq!(state.take().expect("a take").notes().len(), 1);
        assert!(!state.restore_take(0, snapshot));
    }
}
