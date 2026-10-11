//! Opens the DOS window with the sample screen and flips between the mobile
//! and desktop layouts every few seconds, to check what a layout switch does
//! to the window (refit, and under Hyprland the resize request).
//!
//! ```text
//! cargo run -p rp-dos --example layout-flip -- [seconds-per-layout] [flips]
//! ```

use std::time::Duration;

use rp_doc::Layout;
use rp_dos::{StopSignal, latest_channel, run_window, sample::sample_frame};

fn main() {
    let mut args = std::env::args().skip(1);
    let secs: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(4.0);
    let flips: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(4);
    let (tx, rx) = latest_channel();
    let stop = StopSignal::new();
    let remote = stop.clone();
    std::thread::spawn(move || {
        let mut layout = Layout::Mobile;
        tx.send(sample_frame(layout));
        for _ in 0..flips {
            std::thread::sleep(Duration::from_secs_f64(secs));
            layout = match layout {
                Layout::Mobile => Layout::Desktop,
                Layout::Desktop => Layout::Mobile,
            };
            eprintln!("layout-flip: {layout:?}");
            tx.send(sample_frame(layout));
        }
        std::thread::sleep(Duration::from_secs_f64(secs));
        remote.stop();
    });
    run_window(rx, stop);
}
