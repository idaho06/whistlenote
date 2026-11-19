//! Starts the whistlenote desktop application. The `RUST_LOG` environment
//! variable selects the log output, for example `RUST_LOG=whistlenote=debug`.

fn main() -> eframe::Result {
    env_logger::init();
    whistlenote::ui::run()
}
