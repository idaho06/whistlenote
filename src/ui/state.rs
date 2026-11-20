//! The application state shared by the views of the desktop application.
//!
//! The state is pure: [`AppState::apply_event`] folds the events the engine
//! reports into the state, and the setter methods turn user actions into
//! the commands the engine expects.

use crate::engine::{EngineCommand, EngineEvent, LiveFrame};
use crate::ui::bpm_mode::BpmState;
use crate::ui::live::{CLEAR_S, NoteSmoother, ShownNote};
use crate::ui::record_mode::RecordState;

pub(crate) use crate::engine::MAX_LATENCY_MS;

/// Returns the label of an input rate in Hz: whole kHz without decimals, otherwise one decimal rounded, for example "48 kHz" for 48_000 and "44.1 kHz" for 44_100.
pub fn rate_label(rate_hz: u32) -> String {
    if rate_hz.is_multiple_of(1_000) {
        format!("{} kHz", rate_hz / 1_000)
    } else {
        format!("{:.1} kHz", f64::from(rate_hz) / 1_000.0)
    }
}

/// Tab shown in the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// The live pitch view.
    #[default]
    Live,
    /// The tempo view.
    Bpm,
    /// The recording view.
    Record,
}

impl Tab {
    /// Returns the label of the tab in the tab buttons and the heading.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Live => "Live",
            Tab::Bpm => "BPM",
            Tab::Record => "Record",
        }
    }
}

/// State shared by the views of the application.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppState {
    /// Tab shown in the main window.
    pub tab: Tab,
    /// Input device names reported by the engine.
    inputs: Vec<String>,
    /// Output device names reported by the engine.
    outputs: Vec<String>,
    /// Selected input device; `None` is the host default device.
    input: Option<String>,
    /// Selected output device; `None` is the host default device.
    output: Option<String>,
    /// Latency offset in milliseconds; 0.0 by default.
    latency_ms: f64,
    /// Whether the engine listens.
    listening: bool,
    /// Sample rate of the open input in Hz, or `None` when no input is open.
    input_rate_hz: Option<u32>,
    /// Latest error text and how many times in a row it arrived.
    error: Option<(String, u32)>,
    /// Latest live pitch frame.
    latest_live: Option<LiveFrame>,
    /// The smoother of the live note.
    live: NoteSmoother,
    /// UI time in seconds of the latest tick that received live frames.
    last_live_ui_s: Option<f64>,
    /// The tempo state of the BPM tab, including the tempo used for recordings.
    pub bpm: BpmState,
    /// The recording state of the Record tab.
    pub record: RecordState,
}

impl AppState {
    /// Applies `event`: the recording state of the Record tab folds it first,
    /// a device list replaces the stored list and keeps the selections, an
    /// input rate replaces the stored rate, an onset feeds the tempo state of
    /// the BPM tab, a repeated error raises its count, other events change
    /// nothing.
    pub fn apply_event(&mut self, event: EngineEvent) {
        self.record.apply_event(&event);
        match event {
            EngineEvent::Onset { time_s } => self.bpm.push_onset(time_s),
            EngineEvent::Devices { inputs, outputs } => {
                self.inputs = inputs;
                self.outputs = outputs;
            }
            EngineEvent::InputRate(rate) => self.input_rate_hz = rate,
            EngineEvent::Error(text) => count_error(&mut self.error, text),
            _ => {}
        }
    }

    /// Folds `frame` into the live note smoother, stores it as the latest live frame and feeds it to the running recording of the Record tab, which collects the live frames and rebuilds its preview from them.
    pub fn apply_live(&mut self, frame: LiveFrame) {
        self.live.push(&frame);
        self.latest_live = Some(frame);
        self.record.push_live(&frame);
    }

    /// Advances the live readout to the UI time `now_ui_s`: `received` says whether live frames arrived since the previous tick. When none arrived for more than [`CLEAR_S`] since the last tick that received frames, the shown note and the latest live frame clear.
    pub fn live_tick(&mut self, now_ui_s: f64, received: bool) {
        if received {
            self.last_live_ui_s = Some(now_ui_s);
        } else if self
            .last_live_ui_s
            .is_some_and(|last_s| now_ui_s - last_s > CLEAR_S)
        {
            self.live.reset();
            self.latest_live = None;
            self.last_live_ui_s = None;
        }
    }

    /// Selects the input device `name` and returns the command for the engine, or `None` when unchanged; a change cancels a running count-in or recording, since the engine discards it without an event, while stopping the tab waits for the engine's answer to the sent stop.
    pub fn select_input(&mut self, name: Option<String>) -> Option<EngineCommand> {
        let command = set_selection(&mut self.input, name).map(EngineCommand::SelectInput);
        if command.is_some() {
            self.record.cancel();
        }
        command
    }

    /// Selects the output device `name` and returns the command for the engine, or `None` when unchanged; a change cancels a running count-in or recording, since the engine discards it without an event, while stopping the tab waits for the engine's answer to the sent stop.
    pub fn select_output(&mut self, name: Option<String>) -> Option<EngineCommand> {
        let command = set_selection(&mut self.output, name).map(EngineCommand::SelectOutput);
        if command.is_some() {
            self.record.cancel();
        }
        command
    }

    /// Clamps `ms` to the allowed range, stores it and returns the command for the engine; returns `None` when `ms` is not finite or unchanged.
    pub fn set_latency_ms(&mut self, ms: f64) -> Option<EngineCommand> {
        if !ms.is_finite() {
            return None;
        }
        let clamped = ms.clamp(-MAX_LATENCY_MS, MAX_LATENCY_MS);
        if clamped == self.latency_ms {
            return None;
        }
        self.latency_ms = clamped;
        Some(EngineCommand::SetLatencyMs(clamped))
    }

    /// Sets whether the engine listens and returns the command for it, or `None` when unchanged; stopping also resets the live note smoother and drops the UI time of the last tick that received frames, since frames stop arriving and the tick-based clearing does not run, and cancels a running count-in or recording, since the engine discards it without an event, while stopping the tab waits for the engine's answer to the sent stop.
    pub fn set_listening(&mut self, on: bool) -> Option<EngineCommand> {
        if self.listening == on {
            return None;
        }
        self.listening = on;
        if !on {
            self.live.reset();
            self.last_live_ui_s = None;
            self.record.cancel();
        }
        Some(if on {
            EngineCommand::StartListening
        } else {
            EngineCommand::StopListening
        })
    }

    /// Records an error of the application itself, for example a failed file export, the same way as an engine error.
    pub fn report_error(&mut self, text: String) {
        count_error(&mut self.error, text);
    }

    /// Clears the stored error.
    pub fn dismiss_error(&mut self) {
        self.error = None;
    }

    /// Returns the input device names reported by the engine.
    pub fn inputs(&self) -> &[String] {
        &self.inputs
    }

    /// Returns the output device names reported by the engine.
    pub fn outputs(&self) -> &[String] {
        &self.outputs
    }

    /// Returns the selected input device, or `None` for the host default.
    pub fn input(&self) -> Option<&str> {
        self.input.as_deref()
    }

    /// Returns the selected output device, or `None` for the host default.
    pub fn output(&self) -> Option<&str> {
        self.output.as_deref()
    }

    /// Returns the latency offset in milliseconds.
    pub fn latency_ms(&self) -> f64 {
        self.latency_ms
    }

    /// Returns whether the engine listens.
    pub fn listening(&self) -> bool {
        self.listening
    }

    /// Returns the sample rate of the open input in Hz, or `None` when no input is open.
    pub fn input_rate_hz(&self) -> Option<u32> {
        self.input_rate_hz
    }

    /// Returns the latest error text and how many times in a row it arrived.
    pub fn error(&self) -> Option<(&str, u32)> {
        self.error
            .as_ref()
            .map(|(text, count)| (text.as_str(), *count))
    }

    /// Returns the latest live pitch frame, if one arrived.
    pub fn latest_live(&self) -> Option<LiveFrame> {
        self.latest_live
    }

    /// Returns the smoothed live note the live view shows, if one is shown.
    pub fn live_note(&self) -> Option<ShownNote> {
        self.live.shown()
    }

    /// Returns whether the selected input is not in the reported input list; `None` is never missing.
    pub fn input_missing(&self) -> bool {
        missing(&self.input, &self.inputs)
    }

    /// Returns whether the selected output is not in the reported output list; `None` is never missing.
    pub fn output_missing(&self) -> bool {
        missing(&self.output, &self.outputs)
    }
}

/// Counts `text` in `slot`: the same text as the stored one raises its count, any other text starts at one.
fn count_error(slot: &mut Option<(String, u32)>, text: String) {
    match slot {
        Some((current, count)) if *current == text => *count += 1,
        slot => *slot = Some((text, 1)),
    }
}

/// Stores `name` in `slot` and returns it when the selection changed.
fn set_selection(slot: &mut Option<String>, name: Option<String>) -> Option<Option<String>> {
    if *slot == name {
        return None;
    }
    *slot = name.clone();
    Some(name)
}

/// Returns whether `selection` names a device that is missing from `names`.
fn missing(selection: &Option<String>, names: &[String]) -> bool {
    selection.as_ref().is_some_and(|name| !names.contains(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BeatPhase;
    use crate::music::assert_close;
    use crate::music::segment::PitchFrame;
    use crate::ui::record_mode::RecordPhase;
    use crate::ui::record_mode::test_support::tone_frames;

    /// Returns a live frame at `time_s`.
    fn frame(time_s: f64) -> LiveFrame {
        LiveFrame {
            time_s,
            freq_hz: Some(440.0),
            confidence: 0.9,
            level: 0.2,
        }
    }

    /// Feeds `state` three voiced 440 Hz frames at 10 ms intervals, which shows A4 as the live note.
    fn feed_three_voiced_frames(state: &mut AppState) {
        for time_s in [0.0, 0.01, 0.02] {
            state.apply_live(LiveFrame {
                time_s,
                freq_hz: Some(440.0),
                confidence: 0.95,
                level: 0.01,
            });
        }
    }

    /// Returns a device event with the given lists.
    fn devices(inputs: &[&str], outputs: &[&str]) -> EngineEvent {
        EngineEvent::Devices {
            inputs: inputs.iter().map(|name| name.to_string()).collect(),
            outputs: outputs.iter().map(|name| name.to_string()).collect(),
        }
    }

    #[test]
    fn default_state_is_empty() {
        let state = AppState::default();
        assert_eq!(state.tab, Tab::Live);
        assert!(state.inputs().is_empty());
        assert!(state.outputs().is_empty());
        assert_eq!(state.input(), None);
        assert_eq!(state.output(), None);
        assert_eq!(state.latency_ms(), 0.0);
        assert!(!state.listening());
        assert_eq!(state.error(), None);
        assert_eq!(state.latest_live(), None);
    }

    #[test]
    fn devices_fill_the_lists_and_keep_the_selection() {
        let mut state = AppState::default();
        state.select_input(Some("a".to_string()));
        state.apply_event(devices(&["a", "b"], &["c"]));
        assert_eq!(
            state.inputs(),
            ["a".to_string(), "b".to_string()].as_slice()
        );
        assert_eq!(state.outputs(), ["c".to_string()].as_slice());
        assert_eq!(state.input(), Some("a"));
    }

    #[test]
    fn selecting_a_device_returns_a_command_once() {
        let mut state = AppState::default();
        assert_eq!(
            state.select_input(Some("a".to_string())),
            Some(EngineCommand::SelectInput(Some("a".to_string())))
        );
        assert_eq!(state.select_input(Some("a".to_string())), None);
        assert_eq!(
            state.select_input(None),
            Some(EngineCommand::SelectInput(None))
        );
        assert_eq!(state.input(), None);

        assert_eq!(
            state.select_output(Some("c".to_string())),
            Some(EngineCommand::SelectOutput(Some("c".to_string())))
        );
        assert_eq!(state.select_output(Some("c".to_string())), None);
        assert_eq!(
            state.select_output(None),
            Some(EngineCommand::SelectOutput(None))
        );
        assert_eq!(state.output(), None);
    }

    #[test]
    fn latency_is_clamped_and_sent_once() {
        let mut state = AppState::default();
        assert_eq!(
            state.set_latency_ms(50.0),
            Some(EngineCommand::SetLatencyMs(50.0))
        );
        assert_eq!(state.set_latency_ms(50.0), None);
        assert_eq!(
            state.set_latency_ms(500.0),
            Some(EngineCommand::SetLatencyMs(200.0))
        );
        assert_eq!(
            state.set_latency_ms(-500.0),
            Some(EngineCommand::SetLatencyMs(-200.0))
        );
        assert_eq!(state.set_latency_ms(f64::NAN), None);
        assert_eq!(state.latency_ms(), -200.0);
    }

    #[test]
    fn listening_toggles_with_a_command() {
        let mut state = AppState::default();
        assert_eq!(
            state.set_listening(true),
            Some(EngineCommand::StartListening)
        );
        assert_eq!(state.set_listening(true), None);
        assert_eq!(
            state.set_listening(false),
            Some(EngineCommand::StopListening)
        );
        assert!(!state.listening());
    }

    #[test]
    fn a_selection_missing_from_the_list_is_missing() {
        let mut state = AppState::default();
        state.apply_event(devices(&["a"], &["c"]));
        state.select_input(Some("gone".to_string()));
        state.select_output(Some("gone".to_string()));
        assert!(state.input_missing());
        assert!(state.output_missing());

        state.apply_event(devices(&["a", "gone"], &["c", "gone"]));
        assert!(!state.input_missing());
        assert!(!state.output_missing());

        state.select_input(None);
        state.select_output(None);
        assert!(!state.input_missing());
        assert!(!state.output_missing());
    }

    #[test]
    fn repeated_errors_are_counted() {
        let mut state = AppState::default();
        state.apply_event(EngineEvent::Error("x".to_string()));
        state.apply_event(EngineEvent::Error("x".to_string()));
        assert_eq!(state.error(), Some(("x", 2)));
        state.apply_event(EngineEvent::Error("y".to_string()));
        assert_eq!(state.error(), Some(("y", 1)));
        state.dismiss_error();
        assert_eq!(state.error(), None);
    }

    #[test]
    fn recording_events_reach_the_record_state() {
        let mut state = AppState::default();
        state.record.start(120.0);
        state.apply_event(EngineEvent::Beat {
            index: 0,
            accent: true,
            phase: BeatPhase::CountIn,
            time_s: 0.0,
        });
        assert_eq!(state.record.last_beat(), Some((0, BeatPhase::CountIn)));
        state.apply_event(EngineEvent::RecordingStarted { origin_s: 1.0 });
        state.record.stop();
        state.apply_event(EngineEvent::RecordingFinished {
            frames: vec![],
            origin_s: 1.0,
        });
        assert!(state.record.is_idle());
        assert!(state.record.take().is_some());
    }

    #[test]
    fn stopping_listening_cancels_a_recording() {
        let mut state = AppState::default();
        state.set_listening(true);
        state.record.start(120.0);
        assert!(!state.record.is_idle());
        state.set_listening(false);
        assert!(state.record.is_idle());
        assert!(state.record.take().is_none());
    }

    #[test]
    fn changing_the_input_cancels_a_recording() {
        let mut state = AppState::default();
        state.select_input(Some("a".to_string()));
        state.record.start(120.0);
        assert_eq!(state.select_input(Some("a".to_string())), None);
        assert!(!state.record.is_idle());
        assert_eq!(
            state.select_input(Some("b".to_string())),
            Some(EngineCommand::SelectInput(Some("b".to_string())))
        );
        assert!(state.record.is_idle());
        assert!(state.record.take().is_none());
    }

    #[test]
    fn changing_the_output_cancels_a_recording() {
        let mut state = AppState::default();
        state.select_output(Some("c".to_string()));
        state.record.start(120.0);
        assert_eq!(state.select_output(Some("c".to_string())), None);
        assert!(!state.record.is_idle());
        assert_eq!(
            state.select_output(Some("d".to_string())),
            Some(EngineCommand::SelectOutput(Some("d".to_string())))
        );
        assert!(state.record.is_idle());
        assert!(state.record.take().is_none());
    }

    /// Runs a recording into the stopping phase: starts it at 120 BPM, reports the start with origin 1.0 and sends the stop.
    fn stopping_state() -> AppState {
        let mut state = AppState::default();
        state.record.start(120.0);
        state.apply_event(EngineEvent::RecordingStarted { origin_s: 1.0 });
        assert_eq!(state.record.stop(), Some(EngineCommand::StopRecording));
        assert_eq!(state.record.phase(), RecordPhase::Stopping);
        state
    }

    /// Feeds the finished take of 0.6 s of 440 Hz frames from 1.0 s and asserts that the record tab is idle with a one-note take.
    fn assert_reported_take(state: &mut AppState) {
        state.apply_event(EngineEvent::RecordingFinished {
            frames: tone_frames(1.0, 0.6, 440.0),
            origin_s: 1.0,
        });
        assert!(state.record.is_idle());
        let take = state.record.take().expect("a take");
        assert_eq!(take.notes().len(), 1);
    }

    #[test]
    fn changing_the_output_while_stopping_keeps_the_reported_take() {
        let mut state = stopping_state();
        state.select_output(Some("c".to_string()));
        assert_eq!(
            state.select_output(Some("d".to_string())),
            Some(EngineCommand::SelectOutput(Some("d".to_string())))
        );
        assert_eq!(state.record.phase(), RecordPhase::Stopping);
        assert_reported_take(&mut state);
    }

    #[test]
    fn changing_the_input_while_stopping_keeps_the_reported_take() {
        let mut state = stopping_state();
        state.select_input(Some("a".to_string()));
        assert_eq!(
            state.select_input(Some("b".to_string())),
            Some(EngineCommand::SelectInput(Some("b".to_string())))
        );
        assert_eq!(state.record.phase(), RecordPhase::Stopping);
        assert_reported_take(&mut state);
    }

    #[test]
    fn stopping_listening_while_stopping_keeps_the_reported_take() {
        let mut state = stopping_state();
        state.set_listening(true);
        assert_eq!(
            state.set_listening(false),
            Some(EngineCommand::StopListening)
        );
        assert_eq!(state.record.phase(), RecordPhase::Stopping);
        assert_reported_take(&mut state);
    }

    #[test]
    fn report_error_counts_repeated_texts() {
        let mut state = AppState::default();
        state.report_error("export failed".to_string());
        state.report_error("export failed".to_string());
        assert_eq!(state.error(), Some(("export failed", 2)));
    }

    #[test]
    fn the_latest_live_frame_is_stored() {
        let mut state = AppState::default();
        let first = frame(1.0);
        let second = LiveFrame {
            time_s: 2.0,
            freq_hz: None,
            confidence: 0.0,
            level: 0.1,
        };
        state.apply_live(first);
        assert_eq!(state.latest_live(), Some(first));
        state.apply_live(second);
        assert_eq!(state.latest_live(), Some(second));
    }

    #[test]
    fn three_voiced_frames_show_the_live_note() {
        let mut state = AppState::default();
        feed_three_voiced_frames(&mut state);
        assert_eq!(state.live_note().map(|note| note.midi), Some(69));
    }

    #[test]
    fn stopping_listening_clears_the_live_note() {
        let mut state = AppState::default();
        feed_three_voiced_frames(&mut state);
        assert!(state.live_note().is_some());
        state.set_listening(true);
        assert!(state.live_note().is_some());
        state.set_listening(false);
        assert_eq!(state.live_note(), None);
    }

    #[test]
    fn the_live_readout_clears_when_frames_stop() {
        let mut state = AppState::default();
        state.set_listening(true);
        feed_three_voiced_frames(&mut state);
        state.live_tick(10.0, true);
        assert!(state.live_note().is_some());
        assert!(state.latest_live().is_some());
        state.live_tick(10.5, false);
        assert_eq!(state.live_note(), None);
        assert_eq!(state.latest_live(), None);
    }

    #[test]
    fn the_live_readout_stays_within_the_clear_time() {
        let mut state = AppState::default();
        state.set_listening(true);
        feed_three_voiced_frames(&mut state);
        state.live_tick(10.0, true);
        assert!(state.live_note().is_some());
        assert!(state.latest_live().is_some());
        state.live_tick(10.2, false);
        assert!(state.live_note().is_some());
        assert!(state.latest_live().is_some());
    }

    #[test]
    fn a_tick_without_any_frames_changes_nothing() {
        let mut state = AppState::default();
        let before = state.clone();
        state.live_tick(5.0, false);
        assert_eq!(state, before);
    }

    #[test]
    fn other_events_leave_the_state_unchanged() {
        let mut state = AppState::default();
        state.apply_event(devices(&["a"], &["c"]));
        state.select_input(Some("a".to_string()));
        state.set_listening(true);
        state.set_latency_ms(10.0);
        state.apply_live(frame(1.0));
        state.apply_event(EngineEvent::Error("x".to_string()));
        let before = state.clone();

        for event in [
            EngineEvent::Beat {
                index: 0,
                accent: true,
                phase: BeatPhase::Free,
                time_s: 1.0,
            },
            EngineEvent::RecordingStarted { origin_s: 1.0 },
            EngineEvent::RecordingFinished {
                frames: vec![PitchFrame {
                    time_s: 1.0,
                    freq_hz: Some(440.0),
                    confidence: 0.9,
                }],
                origin_s: 1.0,
            },
        ] {
            state.apply_event(event);
        }
        assert_eq!(state, before);
    }

    #[test]
    fn the_input_rate_follows_the_engine() {
        let mut state = AppState::default();
        assert_eq!(state.input_rate_hz(), None);
        state.apply_event(EngineEvent::InputRate(Some(8_000)));
        assert_eq!(state.input_rate_hz(), Some(8_000));
        state.apply_event(EngineEvent::InputRate(None));
        assert_eq!(state.input_rate_hz(), None);
    }

    #[test]
    fn rate_labels_use_khz() {
        assert_eq!(rate_label(48_000), "48 kHz");
        assert_eq!(rate_label(44_100), "44.1 kHz");
        assert_eq!(rate_label(8_000), "8 kHz");
        assert_eq!(rate_label(22_050), "22.1 kHz");
    }

    #[test]
    fn onset_events_feed_the_bpm_state() {
        let mut state = AppState::default();
        for time_s in [0.0, 0.5, 1.0] {
            state.apply_event(EngineEvent::Onset { time_s });
        }
        assert_eq!(state.bpm.onset_count(), 3);
        assert_close(
            state.bpm.estimate().expect("a tempo is estimated"),
            120.0,
            0.5,
        );
    }

    #[test]
    fn live_frames_build_the_preview_of_the_running_recording() {
        let mut state = AppState::default();
        state.record.start(120.0);
        // `start` enters the count-in, so the start event moves the state into the recording.
        state.apply_event(EngineEvent::RecordingStarted { origin_s: 0.0 });
        for i in 0..30 {
            state.apply_live(LiveFrame {
                time_s: i as f64 * 0.01,
                freq_hz: Some(440.0),
                confidence: 0.95,
                level: 0.01,
            });
        }
        let preview = state
            .record
            .preview()
            .expect("a preview after 30 live frames");
        assert_eq!(preview.notes().len(), 1);
        assert_eq!(preview.notes()[0].midi, 69);
    }
}
