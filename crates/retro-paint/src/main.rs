//! retro-paint: GPUI editor on the main thread, DOS view on its own thread.

use rp_dos::{FrameMsg, latest_channel};

fn main() {
    let (tx, rx) = latest_channel();
    std::thread::Builder::new()
        .name("dos-view".into())
        .spawn(move || rp_dos::run_window(rx))
        .expect("spawn DOS view thread");

    rp_editor::run(Box::new(move |width, height, pixels| {
        tx.send(FrameMsg {
            width,
            height,
            pixels,
        })
    }));
}
