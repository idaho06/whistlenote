//! Checks the engine-clock mapping of the default input and output streams.
//!
//! For 10 seconds it prints, twice per second, how far each stream's block
//! time lies from the engine-clock time of its callback: the input delay
//! (callback time minus capture time of the block's first sample) and the
//! output lead (playback time of the block's first sample minus callback
//! time). Both stay nearly constant when the mapping holds; a steady change
//! is clock drift. The output plays silence.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use whistlenote::audio::backend::AudioBackend;
use whistlenote::audio::cpal_backend::CpalBackend;

/// Seconds the check runs.
const RUN_S: u64 = 10;

/// An `f64` shared between threads without locking.
#[derive(Clone, Default)]
struct SharedF64(Arc<AtomicU64>);

impl SharedF64 {
    fn set(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

fn elapsed_s(epoch: Instant) -> f64 {
    epoch.elapsed().as_secs_f64()
}

fn main() {
    let backend = CpalBackend::new();
    let epoch = backend.epoch();
    for (direction, names) in [
        ("input", backend.list_inputs()),
        ("output", backend.list_outputs()),
    ] {
        match names {
            Ok(names) => names
                .iter()
                .for_each(|name| println!("{direction}: {name}")),
            Err(error) => eprintln!("cannot list the {direction}s: {error}"),
        }
    }
    let input_delay = SharedF64::default();
    let output_lead = SharedF64::default();

    let delay = input_delay.clone();
    let input = backend.open_input(
        None,
        Box::new(move |_, info| delay.set(elapsed_s(epoch) - info.time_s)),
        Box::new(|error| eprintln!("input error: {error}")),
    );
    let lead = output_lead.clone();
    let output = backend.open_output(
        None,
        Box::new(move |_, info| lead.set(info.time_s - elapsed_s(epoch))),
        Box::new(|error| eprintln!("output error: {error}")),
    );
    let (input, output) = match (input, output) {
        (Ok(input), Ok(output)) => (input, output),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("cannot open the default streams: {error}");
            std::process::exit(1);
        }
    };
    println!(
        "input {} Hz, output {} Hz",
        input.sample_rate(),
        output.sample_rate()
    );

    let mut first: Option<(f64, f64)> = None;
    for _ in 0..RUN_S * 2 {
        thread::sleep(Duration::from_millis(500));
        let (delay, lead) = (input_delay.get(), output_lead.get());
        let (first_delay, first_lead) = *first.get_or_insert((delay, lead));
        println!(
            "t {:>5.1} s  input delay {:>7.2} ms (change {:>+6.2})  output lead {:>7.2} ms (change {:>+6.2})",
            elapsed_s(epoch),
            delay * 1e3,
            (delay - first_delay) * 1e3,
            lead * 1e3,
            (lead - first_lead) * 1e3,
        );
    }
}
