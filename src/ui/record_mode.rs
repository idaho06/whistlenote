//! The recording state of the Record tab: the phase of a recording, the grid and metronome settings, the latest beat and the finished take.

use crate::engine::{BeatPhase, EngineCommand, EngineEvent};
use crate::export::csv::to_csv;
use crate::export::midi::to_midi_bytes;
use crate::metronome::BEATS_PER_BAR;
use crate::music::quantize::{Grid, QuantNote, quantize};
use crate::music::segment::{PitchFrame, RawNote, SegmentConfig, segment};

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
    /// A stop is sent; the tab waits for the finished take.
    Stopping,
}

/// A finished recording: its frames and the notes quantized from them.
#[derive(Debug, Clone, PartialEq)]
pub struct Take {
    /// Pitch frames of the recording.
    frames: Vec<PitchFrame>,
    /// Engine-clock time of the recording origin in seconds.
    origin_s: f64,
    /// Tempo the take was recorded at.
    bpm: f64,
    /// Notes segmented from `frames` with the default config, computed once.
    raw: Vec<RawNote>,
    /// Notes quantized from `raw` onto the current grid.
    notes: Vec<QuantNote>,
}

impl Take {
    /// Segments `frames` with the default config and quantizes the notes onto `grid`, building the take recorded at `bpm` with its origin at `origin_s`.
    pub fn new(frames: Vec<PitchFrame>, origin_s: f64, bpm: f64, grid: Grid) -> Self {
        let raw = segment(&frames, &SegmentConfig::default());
        let notes = quantize(&raw, bpm, grid, origin_s);
        Self {
            frames,
            origin_s,
            bpm,
            raw,
            notes,
        }
    }

    /// Re-quantizes the segmented notes onto `grid`.
    pub fn set_grid(&mut self, grid: Grid) {
        self.notes = quantize(&self.raw, self.bpm, grid, self.origin_s);
    }

    /// Returns the notes quantized from the take.
    pub fn notes(&self) -> &[QuantNote] {
        &self.notes
    }

    /// Returns the tempo the take was recorded at.
    pub fn bpm(&self) -> f64 {
        self.bpm
    }

    /// Returns the engine-clock time of the recording origin in seconds.
    pub fn origin_s(&self) -> f64 {
        self.origin_s
    }

    /// Returns how many pitch frames the take holds.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Returns the notes as a Standard MIDI File at the take's tempo.
    pub fn midi_bytes(&self) -> Vec<u8> {
        to_midi_bytes(&self.notes, self.bpm)
    }

    /// Returns the notes as CSV text.
    pub fn csv(&self) -> String {
        to_csv(&self.notes)
    }
}

/// The recording state of the Record tab.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordState {
    /// Phase of the running recording.
    phase: RecordPhase,
    /// Grid the finished take is quantized onto.
    grid: Grid,
    /// Whether recording beats are audible.
    metronome: bool,
    /// Tempo of the running recording, locked at start.
    bpm: f64,
    /// Index and phase of the latest beat of the running recording.
    last_beat: Option<(u32, BeatPhase)>,
    /// Whether a count-in beat arrived for the running recording.
    count_in_seen: bool,
    /// The latest finished take.
    take: Option<Take>,
}

impl Default for RecordState {
    fn default() -> Self {
        Self {
            phase: RecordPhase::Idle,
            grid: Grid::default(),
            metronome: true,
            bpm: 0.0,
            last_beat: None,
            count_in_seen: false,
            take: None,
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

    /// Stores `grid` and re-quantizes the finished take onto it, if there is one.
    pub fn set_grid(&mut self, grid: Grid) {
        self.grid = grid;
        if let Some(take) = &mut self.take {
            take.set_grid(grid);
        }
    }

    /// Stores whether recording beats are audible; the setting changes only while idle and is locked during a recording.
    pub fn set_metronome(&mut self, on: bool) {
        if self.is_idle() {
            self.metronome = on;
        }
    }

    /// Locks `bpm`, clears the latest beat, enters the count-in and returns the command that starts the recording; returns `None` and changes nothing unless the tab is idle and `bpm` is finite and positive. The finished take is kept.
    pub fn start(&mut self, bpm: f64) -> Option<EngineCommand> {
        if !self.is_idle() || !bpm.is_finite() || bpm <= 0.0 {
            return None;
        }
        self.bpm = bpm;
        self.last_beat = None;
        self.count_in_seen = false;
        self.phase = RecordPhase::CountIn;
        Some(EngineCommand::StartRecording {
            bpm,
            metronome: self.metronome,
        })
    }

    /// Enters the stopping phase and returns the command that ends the recording; returns `None` while idle or already stopping.
    pub fn stop(&mut self) -> Option<EngineCommand> {
        match self.phase {
            RecordPhase::CountIn | RecordPhase::Recording => {
                self.phase = RecordPhase::Stopping;
                Some(EngineCommand::StopRecording)
            }
            RecordPhase::Idle | RecordPhase::Stopping => None,
        }
    }

    /// Returns to idle without a take when a recording runs, because the engine discards a recording without an event when listening stops or a device changes; the finished take is kept.
    pub fn cancel(&mut self) {
        if !self.is_idle() {
            self.phase = RecordPhase::Idle;
            self.last_beat = None;
            self.count_in_seen = false;
        }
    }

    /// Folds `event` into the state: a beat while a recording runs updates the latest beat and a count-in beat marks the count-in as seen, a start moves the count-in into the recording, a finish stores the take and returns to idle, an error before the first count-in beat aborts the start. Other events change nothing.
    pub fn apply_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::Beat { index, phase, .. } if !self.is_idle() => {
                self.last_beat = Some((*index, *phase));
                if *phase == BeatPhase::CountIn {
                    self.count_in_seen = true;
                }
            }
            EngineEvent::RecordingStarted { .. } if self.phase == RecordPhase::CountIn => {
                self.phase = RecordPhase::Recording;
            }
            EngineEvent::RecordingFinished { frames, origin_s } if !self.is_idle() => {
                self.take = Some(Take::new(frames.clone(), *origin_s, self.bpm, self.grid));
                self.phase = RecordPhase::Idle;
                self.last_beat = None;
                self.count_in_seen = false;
            }
            EngineEvent::Error(_) if self.phase == RecordPhase::CountIn && !self.count_in_seen => {
                self.phase = RecordPhase::Idle;
            }
            _ => {}
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a beat event with `index` and `phase`, accented every fourth beat, at time 0.
    fn beat(index: u32, phase: BeatPhase) -> EngineEvent {
        EngineEvent::Beat {
            index,
            accent: index.is_multiple_of(4),
            phase,
            time_s: 0.0,
        }
    }

    /// Returns voiced frames every 0.01 s from `start_s` for `len_s` seconds at `hz` Hz with confidence 0.95.
    fn tone_frames(start_s: f64, len_s: f64, hz: f32) -> Vec<PitchFrame> {
        let count = (len_s / 0.01).round() as usize;
        (0..count)
            .map(|i| PitchFrame {
                time_s: start_s + i as f64 * 0.01,
                freq_hz: Some(hz),
                confidence: 0.95,
            })
            .collect()
    }

    /// Runs a full recording at 120 BPM with `frames` and origin 1.0 and returns the state and the frames.
    fn recorded_state(frames: Vec<PitchFrame>) -> (RecordState, Vec<PitchFrame>) {
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
    fn stopping_during_the_count_in_gives_an_empty_take() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&beat(0, BeatPhase::CountIn));
        assert_eq!(state.stop(), Some(EngineCommand::StopRecording));
        state.apply_event(&EngineEvent::RecordingFinished {
            frames: Vec::new(),
            origin_s: 0.0,
        });
        assert!(state.is_idle());
        let take = state.take().expect("a take");
        assert!(take.notes().is_empty());
        assert_eq!(take.frame_count(), 0);
        assert_eq!(take.origin_s(), 0.0);
    }

    #[test]
    fn an_error_before_the_first_count_in_beat_aborts_the_start() {
        let mut state = RecordState::default();
        state.start(120.0);
        state.apply_event(&EngineEvent::Error("device busy".to_string()));
        assert!(state.is_idle());

        state.start(120.0);
        state.apply_event(&beat(0, BeatPhase::CountIn));
        state.apply_event(&EngineEvent::Error("device busy".to_string()));
        assert_eq!(
            state.phase(),
            RecordPhase::CountIn,
            "an error after a count-in beat changes nothing"
        );
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
        assert_eq!(state.last_beat(), None);
        assert!(state.is_idle());
        assert!(state.take().is_none());
        assert_eq!(state.beat_text(), "");
    }
}
