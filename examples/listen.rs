//! Listens on a microphone and prints the note detected while the user
//! whistles, plus onsets such as claps, until interrupted.
//!
//! The first argument names the input device as listed; without it the
//! default input is used. Ctrl-C stops the example.

use crossbeam_channel::select;
use whistlenote::audio::cpal_backend::CpalBackend;
use whistlenote::engine::{Engine, EngineCommand, EngineEvent, LiveFrame};
use whistlenote::music::note::{from_freq, midi_to_name};

/// Minimum frame time between two printed live frames, in seconds.
const PRINT_INTERVAL_S: f64 = 0.1;

fn main() {
    let engine = Engine::spawn(CpalBackend::new());
    if let Some(name) = std::env::args().nth(1) {
        engine.send(EngineCommand::SelectInput(Some(name)));
    }
    engine.send(EngineCommand::StartListening);
    println!("listening; press Ctrl-C to stop");

    // Engine-clock time of the last printed frame; the first frame prints.
    let mut last_printed = f64::NEG_INFINITY;
    'listen: loop {
        select! {
            recv(engine.live()) -> frame => match frame {
                Ok(frame) => print_frame(frame, &mut last_printed),
                // The engine thread is gone.
                Err(_) => break 'listen,
            },
            recv(engine.events()) -> event => match event {
                Ok(event) => print_event(event),
                Err(_) => break 'listen,
            },
        }
    }
}

/// Prints `frame` when at least [`PRINT_INTERVAL_S`] of frame time passed
/// since the last printed frame.
fn print_frame(frame: LiveFrame, last_printed: &mut f64) {
    let LiveFrame {
        time_s,
        freq_hz,
        confidence,
        level,
    } = frame;
    if time_s - *last_printed < PRINT_INTERVAL_S {
        return;
    }
    *last_printed = time_s;
    match freq_hz.and_then(|freq| from_freq(freq).map(|note| (freq, note))) {
        Some((freq, note)) => println!(
            "{time_s:>8.3} s  {:<4} {:+5.1} cents  {freq:7.1} Hz  conf {confidence:.2}  level {level:.3}",
            midi_to_name(note.midi),
            note.cents,
        ),
        None => println!("{time_s:>8.3} s  -  level {level:.3}"),
    }
}

/// Prints the input devices, onsets and errors of `event`; the other
/// events are ignored.
fn print_event(event: EngineEvent) {
    match event {
        EngineEvent::Devices { inputs, .. } => {
            for name in inputs {
                println!("input: {name}");
            }
        }
        EngineEvent::Onset { time_s } => println!("{time_s:>8.3} s  onset"),
        EngineEvent::Error(message) => eprintln!("error: {message}"),
        _ => {}
    }
}
