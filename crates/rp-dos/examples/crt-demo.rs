//! Opens the DOS window with the sample screen drawn through the CRT shader.
//!
//! ```text
//! cargo run -p rp-dos --example crt-demo -- mobile|desktop
//! ```
//!
//! The full app does the same with `RP_DOS_DEMO=mobile|desktop`.

use rp_doc::Layout;

fn main() {
    let layout = match std::env::args().nth(1).as_deref() {
        None | Some("mobile") => Layout::Mobile,
        Some("desktop") => Layout::Desktop,
        Some(v) => {
            eprintln!("usage: crt-demo [mobile|desktop] (got {v:?})");
            std::process::exit(2);
        }
    };
    rp_dos::run_crt_demo(layout);
}
