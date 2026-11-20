//! The preferences window's state: the draft tuning edited in the window, the tuning it opened with, and the take kept for Cancel.

use std::path::Path;

use crate::settings::{Settings, Tuning, VERSION};
use crate::ui::record_mode::Take;
use crate::ui::state::AppState;

/// A section of the preferences window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Section {
    /// Reference pitch and voiced-frame confidence.
    #[default]
    Tuning,
    /// Pitch tracking.
    Pitch,
    /// Clap and tap detection.
    Onsets,
    /// Segmentation of pitch frames into notes.
    Notes,
    /// Recording timing.
    Timing,
    /// The click sound.
    Metronome,
    /// The tempo estimate from claps.
    Tempo,
    /// The live note readout.
    Live,
    /// MIDI export.
    Export,
}

impl Section {
    /// Every section, in the order the window lists them.
    pub const ALL: [Section; 9] = [
        Section::Tuning,
        Section::Pitch,
        Section::Onsets,
        Section::Notes,
        Section::Timing,
        Section::Metronome,
        Section::Tempo,
        Section::Live,
        Section::Export,
    ];

    /// Returns the section's label.
    pub fn label(self) -> &'static str {
        match self {
            Section::Tuning => "Tuning",
            Section::Pitch => "Pitch",
            Section::Onsets => "Onsets",
            Section::Notes => "Notes",
            Section::Timing => "Timing",
            Section::Metronome => "Metronome",
            Section::Tempo => "Tempo",
            Section::Live => "Live",
            Section::Export => "Export",
        }
    }
}

/// State of the preferences window.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrefsWindow {
    /// Whether the window is open.
    open: bool,
    /// The tuning edited in the window, applied live while the window is open.
    draft: Tuning,
    /// The tuning the window opened with, restored by Cancel.
    original: Tuning,
    /// The shown take as it was just before the first re-segment of this window session, with its generation; restored by Cancel.
    snapshot: Option<(u64, Take)>,
    /// The section shown in the window.
    section: Section,
    /// The error of the latest failed save, if any.
    save_error: Option<String>,
}

impl PrefsWindow {
    /// Returns whether the window is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the window with the applied tuning of `state` as the draft and the original and clears the take snapshot and the save error; an already open window keeps its draft, and the section keeps its last value either way.
    pub fn open(&mut self, state: &AppState) {
        if self.open {
            return;
        }
        self.draft = state.tuning().clone();
        self.original = state.tuning().clone();
        self.snapshot = None;
        self.save_error = None;
        self.open = true;
    }

    /// Returns the draft tuning edited in the window.
    pub fn draft(&self) -> &Tuning {
        &self.draft
    }

    /// Returns the draft tuning mutably.
    pub fn draft_mut(&mut self) -> &mut Tuning {
        &mut self.draft
    }

    /// Returns the section shown in the window.
    pub fn section(&self) -> Section {
        self.section
    }

    /// Shows `section` in the window.
    pub fn set_section(&mut self, section: Section) {
        self.section = section;
    }

    /// Returns the error of the latest failed save, if any.
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Applies the draft to `state`, called every frame: does nothing when the window is closed or the applied tuning already equals the draft; otherwise keeps the shown take with its edits in the snapshot when it is about to be re-segmented for the first time in this window session, then applies the draft.
    pub fn apply(&mut self, state: &mut AppState) {
        if !self.open || state.tuning() == &self.draft {
            return;
        }
        if self.snapshot.is_none()
            && state.tuning().segment() != self.draft.segment()
            && let Some(take) = state.record.take()
        {
            self.snapshot = Some((state.record.take_generation(), take.clone()));
        }
        state.apply_tuning(&self.draft);
    }

    /// Cancels the window: restores the original tuning and the snapshotted take, then closes; a take recorded since the snapshot is kept, because the restore checks the generation. Does nothing when the window is closed.
    pub fn cancel(&mut self, state: &mut AppState) {
        if !self.open {
            return;
        }
        if state.tuning() != &self.original {
            state.apply_tuning(&self.original);
        }
        if let Some((generation, take)) = self.snapshot.take() {
            state.record.restore_take(generation, take);
        }
        self.open = false;
        self.snapshot = None;
        self.save_error = None;
    }

    /// Resets the sub-struct of the current section in the draft to its default; changes only the draft, never the session.
    pub fn reset_section(&mut self) {
        match self.section {
            Section::Tuning => self.draft.voicing = Default::default(),
            Section::Pitch => self.draft.pitch = Default::default(),
            Section::Onsets => self.draft.onsets = Default::default(),
            Section::Notes => self.draft.notes = Default::default(),
            Section::Timing => self.draft.timing = Default::default(),
            Section::Metronome => self.draft.metronome = Default::default(),
            Section::Tempo => self.draft.tempo = Default::default(),
            Section::Live => self.draft.live = Default::default(),
            Section::Export => self.draft.export = Default::default(),
        }
    }

    /// Resets the whole draft to the default tuning.
    pub fn restore_all(&mut self) {
        self.draft = Tuning::default();
    }

    /// Saves the session of `state` and the draft to the settings file at `path` and returns the saved tuning: on success applies the saved tuning and closes the window; on failure keeps the window open and stores the error. With `path == None` stores an error and returns `None`.
    pub fn save(&mut self, state: &mut AppState, path: Option<&Path>) -> Option<Tuning> {
        let Some(path) = path else {
            self.save_error = Some(
                "No configuration directory is available, so the settings cannot be saved."
                    .to_string(),
            );
            return None;
        };
        let settings = Settings {
            version: VERSION,
            session: state.session(),
            tuning: self.draft.clone(),
        }
        .sanitized();
        match settings.save_to(path) {
            Ok(()) => {
                if state.tuning() != &settings.tuning {
                    state.apply_tuning(&settings.tuning);
                }
                self.open = false;
                self.snapshot = None;
                self.save_error = None;
                Some(settings.tuning)
            }
            Err(error) => {
                self.save_error = Some(format!("Could not save {}: {error}", path.display()));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::engine::{BeatPhase, EngineEvent};
    use crate::settings::Pitch;
    use crate::ui::record_mode::test_support::{beat, recorded_state, tone_frames};

    /// Returns a fresh scratch directory for the test `name` in the temporary directory.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("whistlenote-prefs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Returns the state with a finished take of three tones at 1.0, 2.0 and 3.0 s recorded at 120 BPM; the take has 3 notes.
    fn state_with_take() -> AppState {
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut frames = tone_frames(1.0, 0.5, 440.0);
        frames.extend(tone_frames(2.0, 0.5, 523.25));
        frames.extend(tone_frames(3.0, 0.5, 659.26));
        state.record = recorded_state(frames).0;
        let take = state.record.take().expect("a take");
        assert_eq!(take.notes().len(), 3);
        state
    }

    #[test]
    fn opening_copies_the_applied_tuning() {
        let mut state = AppState::default();
        let mut tuning = Tuning::default();
        tuning.voicing.a4_hz = 432.0;
        state.apply_tuning(&tuning);
        let mut prefs = PrefsWindow::default();
        assert!(!prefs.is_open());
        prefs.open(&state);
        assert!(prefs.is_open());
        assert_eq!(prefs.draft(), state.tuning());
    }

    #[test]
    fn opening_an_open_window_keeps_the_draft() {
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.open(&state);
        assert_eq!(prefs.draft().voicing.a4_hz, 432.0);
    }

    #[test]
    fn apply_applies_a_changed_draft_and_ignores_a_closed_window() {
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.apply(&mut state);
        assert_eq!(
            state.tuning().voicing.a4_hz,
            440.0,
            "closed: nothing is applied"
        );
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.apply(&mut state);
        assert_eq!(state.tuning().voicing.a4_hz, 432.0);
    }

    #[test]
    fn cancel_restores_the_original_tuning_and_closes() {
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.cancel(&mut state);
        assert_eq!(
            state.tuning(),
            &Tuning::default(),
            "closed: nothing happens"
        );
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.apply(&mut state);
        assert_eq!(state.tuning().voicing.a4_hz, 432.0);
        prefs.cancel(&mut state);
        assert!(!prefs.is_open());
        assert_eq!(state.tuning(), &Tuning::default());
    }

    #[test]
    fn cancel_restores_an_edited_take_after_a_resegment() {
        let mut state = state_with_take();
        state.record.take_mut().expect("a take").delete(0);
        let edited = state.record.take().expect("a take").clone();
        assert_eq!(edited.notes().len(), 2);
        assert!(edited.is_edited());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().notes.min_note_ms = 200.0;
        prefs.apply(&mut state);
        let take = state.record.take().expect("a take");
        assert!(!take.is_edited());
        assert_eq!(take.notes().len(), 3);
        prefs.cancel(&mut state);
        assert_eq!(state.record.take(), Some(&edited));
    }

    #[test]
    fn edits_survive_cancel_when_nothing_resegments() {
        let mut state = state_with_take();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        state.record.take_mut().expect("a take").delete(0);
        prefs.draft_mut().export.velocity = 64;
        prefs.apply(&mut state);
        prefs.cancel(&mut state);
        let take = state.record.take().expect("a take");
        assert_eq!(take.notes().len(), 2);
        assert!(take.is_edited());
    }

    #[test]
    fn cancel_keeps_a_take_recorded_after_the_resegment() {
        let mut state = state_with_take();
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().notes.min_note_ms = 200.0;
        prefs.apply(&mut state);
        assert!(state.record.start(120.0).is_some());
        for i in 0..4 {
            state.record.apply_event(&beat(i, BeatPhase::CountIn));
        }
        state
            .record
            .apply_event(&EngineEvent::RecordingStarted { origin_s: 1.0 });
        state.record.apply_event(&beat(0, BeatPhase::Recording));
        assert!(state.record.stop().is_some());
        state.record.apply_event(&EngineEvent::RecordingFinished {
            frames: tone_frames(1.0, 0.5, 440.0),
            origin_s: 1.0,
        });
        prefs.cancel(&mut state);
        let take = state.record.take().expect("a take");
        assert_eq!(take.notes().len(), 1);
    }

    #[test]
    fn reset_section_resets_only_the_current_section() {
        let mut prefs = PrefsWindow::default();
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.draft_mut().pitch.clarity = 0.7;
        prefs.set_section(Section::Pitch);
        assert_eq!(prefs.section(), Section::Pitch);
        prefs.reset_section();
        assert_eq!(prefs.draft().pitch, Pitch::default());
        assert_eq!(prefs.draft().voicing.a4_hz, 432.0);
    }

    #[test]
    fn restore_all_resets_the_draft() {
        let mut prefs = PrefsWindow::default();
        prefs.draft_mut().voicing.a4_hz = 432.0;
        prefs.restore_all();
        assert_eq!(prefs.draft(), &Tuning::default());
    }

    #[test]
    fn save_writes_the_settings_and_closes() {
        let dir = scratch_dir("save");
        let path = dir.join("settings.toml");
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        assert!(state.set_latency_ms(25.0).is_some());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        prefs.draft_mut().voicing.a4_hz = 432.0;
        let saved = prefs
            .save(&mut state, Some(&path))
            .expect("the settings are saved");
        assert_eq!(saved.voicing.a4_hz, 432.0);
        assert!(!prefs.is_open());
        assert_eq!(prefs.save_error(), None);
        assert_eq!(state.tuning().voicing.a4_hz, 432.0);
        let (loaded, warning) = Settings::load_from(&path);
        assert_eq!(warning, None);
        assert_eq!(loaded.tuning, saved);
        assert_eq!(loaded.session.latency_ms, 25.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_without_a_path_keeps_the_window_open_with_an_error() {
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        assert_eq!(prefs.save(&mut state, None), None);
        assert!(prefs.is_open());
        let error = prefs.save_error().expect("an error");
        assert!(
            error.contains("No configuration directory"),
            "unexpected: {error}"
        );
    }

    #[test]
    fn save_to_an_unwritable_path_keeps_the_window_open_with_an_error() {
        let dir = scratch_dir("unwritable");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("blocker"), "a file").unwrap();
        let mut state = AppState::default();
        state.apply_tuning(&Tuning::default());
        let mut prefs = PrefsWindow::default();
        prefs.open(&state);
        let path = dir.join("blocker").join("settings.toml");
        assert_eq!(prefs.save(&mut state, Some(&path)), None);
        assert!(prefs.is_open());
        let error = prefs.save_error().expect("an error");
        assert!(error.contains("Could not save"), "unexpected: {error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sections_have_distinct_labels() {
        assert_eq!(Section::ALL.len(), 9);
        let mut labels: Vec<&str> = Section::ALL.iter().map(|section| section.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 9);
        assert_eq!(Section::default(), Section::Tuning);
    }
}
