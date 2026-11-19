//! Desktop application: window, views and the state they share.
//!
//! [`run`] opens the main window and runs the application until the window
//! closes. The window has a top bar with the input and output device combos,
//! the refresh button, the latency offset, the listen toggle and the tab
//! buttons, a status bar at the bottom that shows whether the engine listens
//! and the latest error, and a central panel with the heading of the
//! selected tab. The views share the [`state::AppState`], which folds the
//! events of the engine into the state and turns user actions into engine
//! commands.

pub mod bpm_mode;
pub mod live;
pub mod record_mode;
pub mod state;

use std::time::Duration;

use eframe::egui;

use self::state::{AppState, MAX_LATENCY_MS, Tab};
use crate::audio::cpal_backend::CpalBackend;
use crate::engine::{Engine, EngineCommand};

/// Repaint interval while the engine listens.
const REPAINT_LISTENING: Duration = Duration::from_millis(16);

/// Repaint interval while the engine is idle; the engine reports device
/// lists on its own and cannot wake the window.
const REPAINT_IDLE: Duration = Duration::from_millis(100);

/// Width of the device combo boxes in the top bar.
const DEVICE_COMBO_WIDTH: f32 = 220.0;

/// Green of the in-tune needle in the live view and the lit onset indicator in the BPM tab.
pub(crate) const GOOD_COLOR: egui::Color32 = egui::Color32::from_rgb(60, 180, 75);

/// Opens the main window and runs the application until the window closes.
pub fn run() -> eframe::Result {
    log::info!("whistlenote {} starting", env!("CARGO_PKG_VERSION"));
    let engine = Engine::spawn(CpalBackend::new());
    let app = WhistlenoteApp {
        engine,
        state: AppState::default(),
    };
    eframe::run_native(
        "whistlenote",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([900.0, 560.0])
                .with_min_inner_size([560.0, 360.0]),
            ..Default::default()
        },
        Box::new(|_cc| Ok(Box::new(app))),
    )
}

/// The application: the engine handle and the state the views share.
struct WhistlenoteApp {
    /// The engine thread; dropping the app stops the engine.
    engine: Engine,
    /// The state shared by the views.
    state: AppState,
}

impl WhistlenoteApp {
    /// Sends `command` to the engine when it is `Some`.
    fn send(&self, command: Option<EngineCommand>) {
        if let Some(command) = command {
            self.engine.send(command);
        }
    }

    /// Shows the top bar in two rows: the device combos, the refresh button
    /// and the latency offset, which wrap in a narrow window; then the listen
    /// toggle, disabled while a recording runs, and the tab buttons.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Input");
            let input = device_combo(
                ui,
                "input",
                self.state.inputs(),
                self.state.input(),
                self.state.input_missing(),
            );
            let command = self.state.select_input(input);
            self.send(command);

            ui.label("Output");
            let output = device_combo(
                ui,
                "output",
                self.state.outputs(),
                self.state.output(),
                self.state.output_missing(),
            );
            let command = self.state.select_output(output);
            self.send(command);

            if ui
                .button("Refresh")
                .on_hover_text("Refresh the device lists and reopen a lost input or output")
                .clicked()
            {
                self.send(Some(EngineCommand::RefreshDevices));
            }

            ui.label("Latency");
            let mut ms = self.state.latency_ms();
            ui.add(
                egui::DragValue::new(&mut ms)
                    .range(-MAX_LATENCY_MS..=MAX_LATENCY_MS)
                    .suffix(" ms"),
            );
            let command = self.state.set_latency_ms(ms);
            self.send(command);
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(self.state.record.is_idle(), |ui| {
                let mut listening = self.state.listening();
                ui.toggle_value(&mut listening, "Listen");
                let command = self.state.set_listening(listening);
                self.send(command);
            });

            ui.separator();
            for tab in [Tab::Live, Tab::Bpm, Tab::Record] {
                ui.selectable_value(&mut self.state.tab, tab, tab.label());
            }
        });
    }

    /// Shows the status bar: whether the engine listens, and the latest
    /// error with a dismiss button.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(if self.state.listening() {
                "Listening"
            } else {
                "Idle"
            });
            let error = self
                .state
                .error()
                .map(|(text, count)| error_label(text, count));
            if let Some(label) = error {
                ui.separator();
                ui.colored_label(ui.visuals().error_fg_color, label);
                if ui.small_button("Dismiss").clicked() {
                    self.state.dismiss_error();
                }
            }
        });
    }
}

impl eframe::App for WhistlenoteApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for frame in self.engine.live().try_iter() {
            self.state.apply_live(frame);
        }
        for event in self.engine.events().try_iter() {
            log::trace!("event: {event:?}");
            self.state.apply_event(event);
        }
        ctx.request_repaint_after(if self.state.listening() || !self.state.record.is_idle() {
            REPAINT_LISTENING
        } else {
            REPAINT_IDLE
        });
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_bar").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| match self.state.tab {
            Tab::Live => live::show(ui, &self.state),
            Tab::Bpm => bpm_mode::show(ui, &mut self.state),
            Tab::Record => {
                let command = record_mode::show(ui, &mut self.state);
                self.send(command);
            }
        });
    }
}

/// Shows `add_contents` in a horizontal row centered in `ui`.
///
/// egui lays a horizontal row out from the left, so the row stores its
/// width in egui's memory under `id_salt`, which is unique within `ui`, and
/// is padded by half the free space on the next pass; the first pass of a
/// new row is left-aligned.
pub(crate) fn centered_row(
    ui: &mut egui::Ui,
    id_salt: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let id = ui.make_persistent_id(id_salt);
    let width = ui.ctx().data(|data| data.get_temp::<f32>(id));
    ui.horizontal(|ui| {
        if let Some(width) = width {
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
        }
        let start = ui.cursor().left();
        add_contents(ui);
        let width = ui.min_rect().right() - start;
        ui.ctx().data_mut(|data| data.insert_temp(id, width));
    });
}

/// Shows a combo box with "Default" and every name in `names` and returns
/// the chosen device, `None` for the default. The combo shows
/// [`selected_label`] as its text and as its hover text, which holds the
/// full name when the text is truncated.
fn device_combo(
    ui: &mut egui::Ui,
    id: &str,
    names: &[String],
    selected: Option<&str>,
    missing: bool,
) -> Option<String> {
    let mut choice = selected.map(str::to_string);
    let label = selected_label(selected, missing);
    let response = egui::ComboBox::from_id_salt(id)
        .selected_text(label.as_str())
        .width(DEVICE_COMBO_WIDTH)
        .truncate()
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut choice, None, "Default");
            for name in names {
                ui.selectable_value(&mut choice, Some(name.clone()), name.as_str())
                    .on_hover_text(name.as_str());
            }
        })
        .response;
    response.on_hover_text(label);
    choice
}

/// Returns the label of the selected device: "Default" for `None`, the name
/// for `Some`, with " (missing)" appended when the device is missing.
fn selected_label(selected: Option<&str>, missing: bool) -> String {
    match selected {
        None => "Default".to_string(),
        Some(name) if missing => format!("{name} (missing)"),
        Some(name) => name.to_string(),
    }
}

/// Returns the error label: `text`, with " (×N)" appended when the error
/// arrived `count` times in a row.
fn error_label(text: &str, count: u32) -> String {
    if count > 1 {
        format!("{text} (×{count})")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_selection_is_labelled_default() {
        assert_eq!(selected_label(None, false), "Default");
    }

    #[test]
    fn a_selected_device_is_labelled_with_its_name() {
        assert_eq!(selected_label(Some("a"), false), "a");
    }

    #[test]
    fn a_missing_device_is_marked_as_missing() {
        assert_eq!(selected_label(Some("a"), true), "a (missing)");
    }

    #[test]
    fn a_single_error_has_no_count() {
        assert_eq!(error_label("x", 1), "x");
    }

    #[test]
    fn a_repeated_error_is_counted() {
        assert_eq!(error_label("x", 3), "x (×3)");
    }
}
