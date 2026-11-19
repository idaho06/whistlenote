//! Lists the input devices, then prints the RMS level of one of them as a
//! bar, ten times per second, until interrupted.
//!
//! The first argument names the input device as listed; without it the
//! default input is used.

use std::thread;
use std::time::Duration;

use whistlenote::audio::backend::AudioBackend;
use whistlenote::audio::cpal_backend::CpalBackend;
use whistlenote::audio::ring::{DEFAULT_HEADER_CAPACITY, DEFAULT_SAMPLE_CAPACITY, input_ring};
use whistlenote::audio::rms;

/// Width of the level bar at full scale.
const BAR_WIDTH: f32 = 60.0;

fn main() {
    let backend = CpalBackend::new();
    match backend.list_inputs() {
        Ok(names) => names.iter().for_each(|name| println!("input: {name}")),
        Err(error) => eprintln!("cannot list the inputs: {error}"),
    }
    let device = std::env::args().nth(1);
    let (writer, mut reader) = input_ring(
        DEFAULT_SAMPLE_CAPACITY,
        DEFAULT_HEADER_CAPACITY,
        thread::current(),
    );
    let stream = match backend.open_input(
        device.as_deref(),
        writer.into_sink(),
        Box::new(|error| eprintln!("stream error: {error}")),
    ) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("cannot open the input: {error}");
            std::process::exit(1);
        }
    };
    let window = stream.sample_rate() as usize / 10;
    println!(
        "{} at {} Hz; press Ctrl-C to stop",
        device.as_deref().unwrap_or("default input"),
        stream.sample_rate()
    );

    let mut samples = Vec::new();
    loop {
        thread::park_timeout(Duration::from_millis(20));
        while reader.read_block(&mut samples).is_some() {}
        while samples.len() >= window {
            let level = rms(&samples[..window]);
            samples.drain(..window);
            // A logarithmic bar: -60 dBFS is empty, 0 dBFS is full.
            let db = 20.0 * level.max(1e-6).log10();
            let filled = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * BAR_WIDTH) as usize;
            println!(
                "{:>7.1} dBFS  rms {:.4}  dropped {:>6}  {}",
                db,
                level,
                reader.dropped_samples(),
                "#".repeat(filled)
            );
        }
    }
}
