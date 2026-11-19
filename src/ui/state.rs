//! The application state shared by the views of the desktop application.
//!
//! The state is pure: [`AppState::apply_event`] folds the events the engine
//! reports into the state, and the setter methods turn user actions into
//! the commands the engine expects.

use crate::engine::{EngineCommand, EngineEvent, LiveFrame};

/// The largest absolute latency offset in milliseconds.
pub(crate) const MAX_LATENCY_MS: f64 = 200.0;

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
    /// Latest error text and how many times in a row it arrived.
    error: Option<(String, u32)>,
    /// Latest live pitch frame.
    latest_live: Option<LiveFrame>,
}

impl AppState {
    /// Applies `event`: a device list replaces the stored list and keeps the
    /// selections, a repeated error raises its count, other events change nothing.
    pub fn apply_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Devices { inputs, outputs } => {
                self.inputs = inputs;
                self.outputs = outputs;
            }
            EngineEvent::Error(text) => match &mut self.error {
                Some((current, count)) if *current == text => *count += 1,
                slot => *slot = Some((text, 1)),
            },
            _ => {}
        }
    }

    /// Stores `frame` as the latest live frame.
    pub fn apply_live(&mut self, frame: LiveFrame) {
        self.latest_live = Some(frame);
    }

    /// Selects the input device `name` and returns the command for the engine, or `None` when unchanged.
    pub fn select_input(&mut self, name: Option<String>) -> Option<EngineCommand> {
        set_selection(&mut self.input, name).map(EngineCommand::SelectInput)
    }

    /// Selects the output device `name` and returns the command for the engine, or `None` when unchanged.
    pub fn select_output(&mut self, name: Option<String>) -> Option<EngineCommand> {
        set_selection(&mut self.output, name).map(EngineCommand::SelectOutput)
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

    /// Sets whether the engine listens and returns the command for it, or `None` when unchanged.
    pub fn set_listening(&mut self, on: bool) -> Option<EngineCommand> {
        if self.listening == on {
            return None;
        }
        self.listening = on;
        Some(if on {
            EngineCommand::StartListening
        } else {
            EngineCommand::StopListening
        })
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

    /// Returns whether the selected input is not in the reported input list; `None` is never missing.
    pub fn input_missing(&self) -> bool {
        missing(&self.input, &self.inputs)
    }

    /// Returns whether the selected output is not in the reported output list; `None` is never missing.
    pub fn output_missing(&self) -> bool {
        missing(&self.output, &self.outputs)
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
    use crate::music::segment::PitchFrame;

    /// Returns a live frame at `time_s`.
    fn frame(time_s: f64) -> LiveFrame {
        LiveFrame {
            time_s,
            freq_hz: Some(440.0),
            confidence: 0.9,
            level: 0.2,
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
            EngineEvent::Onset { time_s: 1.0 },
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
}
