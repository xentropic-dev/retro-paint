//! retro-paint: GPUI editor on the main thread, DOS view on its own thread.
//!
//! Shutdown: when the editor quits (`rp_editor::run` returns, after joining
//! its render worker), main signals
//! the DOS thread to stop, joins it so the DOS window closes cleanly, and
//! exits. Closing the DOS window first only ends the view.

use std::time::{Duration, Instant};

use rp_dos::{StopSignal, latest_channel};

/// How long main waits for the DOS thread to close its window before exiting anyway.
const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

fn main() {
    let (tx, rx) = latest_channel();
    let stop = StopSignal::new();
    let dos = std::thread::Builder::new()
        .name("dos-view".into())
        .spawn({
            let stop = stop.clone();
            move || rp_dos::run_window(rx, stop)
        })
        .expect("spawn DOS view thread");

    // The editor's render worker sends each frame; the channel keeps only the newest.
    rp_editor::run(Box::new(move |frame| tx.send(frame)));

    stop.stop();
    let deadline = Instant::now() + JOIN_TIMEOUT;
    while !dos.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if dos.is_finished() {
        let _ = dos.join();
    }
    // Exit now: GPUI may leave background threads and resources behind.
    std::process::exit(0);
}
