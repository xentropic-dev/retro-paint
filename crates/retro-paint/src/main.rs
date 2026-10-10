//! retro-paint: GPUI editor on the main thread, DOS view on its own thread.
//!
//! Shutdown: when the editor quits (`rp_editor::run` returns), main signals
//! the DOS thread to stop, joins it so the DOS window closes cleanly, and
//! exits. Closing the DOS window first only ends the view.

use std::time::{Duration, Instant};

use rp_doc::{FrameMsg, IndexedFrame, Layout, Size, Tool};
use rp_dos::{StopSignal, latest_channel};
use rp_palette_dither::{CgaColor, Palette};

/// How long main waits for the DOS thread to close its window before exiting anyway.
const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Wraps the editor's indexed test pattern `(width, height, indices)` as a
/// DOS frame (doc-order CGA indices): full palette, mobile layout, no cursor, brush tool.
/// Out-of-range indices draw black. Temporary until `render-worker` sends
/// real frames.
fn pattern_frame(width: u32, height: u32, pixels: &[u8]) -> FrameMsg {
    let pixels = pixels
        .iter()
        .map(|&i| CgaColor::from_index(i).unwrap_or(CgaColor::Black))
        .collect();
    FrameMsg {
        image: IndexedFrame {
            size: Size::new(width, height),
            pixels,
            palette: Palette::full(),
        },
        layout: Layout::Mobile,
        cursor: None,
        tool: Tool::Brush,
        brush_diameter: 1,
        active_color: CgaColor::White,
    }
}

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

    rp_editor::run(Box::new(move |width, height, pixels| {
        tx.send(pattern_frame(width, height, &pixels))
    }));

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_frame_maps_indices_to_cga() {
        let f = pattern_frame(2, 2, &[0, 4, 15, 200]);
        assert_eq!(f.image.size, Size::new(2, 2));
        assert_eq!(
            f.image.pixels,
            [
                CgaColor::Black,
                CgaColor::Blue,
                CgaColor::Yellow,
                CgaColor::Black
            ]
        );
        assert_eq!(
            (f.layout, f.cursor, f.tool),
            (Layout::Mobile, None, Tool::Brush)
        );
        assert!(f.image.pixels.iter().all(|&c| f.image.palette.contains(c)));
    }
}
