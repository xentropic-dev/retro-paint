//! Composes the DOS screen for the built-in sample image and writes it as a PNG.
//!
//! ```text
//! cargo run -p rp-dos --example dump-screen -- --layout mobile|desktop --out <path>
//!     [--scale N] [--tool select|move|brush|eraser|picker]
//! ```
//!
//! `--scale` upscales with nearest-neighbor (default 1: the raw screen).

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::process::ExitCode;

use rp_doc::{Layout, Tool};
use rp_dos::{ScrollState, compose, sample};

struct Args {
    layout: Layout,
    out: PathBuf,
    scale: u32,
    tool: Option<Tool>,
}

const USAGE: &str = "usage: dump-screen --layout mobile|desktop --out <path> [--scale N] \
                     [--tool select|move|brush|eraser|picker]";

fn parse() -> Result<Args, String> {
    let mut layout = Layout::Mobile;
    let mut out = None;
    let mut scale = 1;
    let mut tool = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--layout" => {
                layout = match value()?.as_str() {
                    "mobile" => Layout::Mobile,
                    "desktop" => Layout::Desktop,
                    v => return Err(format!("unknown layout {v:?}")),
                }
            }
            "--out" => out = Some(PathBuf::from(value()?)),
            "--scale" => {
                scale = value()?
                    .parse()
                    .ok()
                    .filter(|&n| (1..=16).contains(&n))
                    .ok_or("--scale must be 1..=16")?
            }
            "--tool" => {
                tool = Some(match value()?.as_str() {
                    "select" => Tool::Select,
                    "move" => Tool::Move,
                    "brush" => Tool::Brush,
                    "eraser" => Tool::Eraser,
                    "picker" => Tool::Picker,
                    v => return Err(format!("unknown tool {v:?}")),
                })
            }
            "-h" | "--help" => return Err(USAGE.into()),
            f => return Err(format!("unknown argument {f:?}\n{USAGE}")),
        }
    }
    Ok(Args {
        layout,
        out: out.ok_or(format!("--out is required\n{USAGE}"))?,
        scale,
        tool,
    })
}

fn write_png(path: &PathBuf, w: u32, h: u32, rgb: &[u8]) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgb).map_err(|e| e.to_string())
}

fn run() -> Result<(), String> {
    let args = parse()?;
    let mut frame = sample::sample_frame(args.layout);
    if let Some(tool) = args.tool {
        frame.tool = tool;
    }
    let screen = compose(&frame, args.layout, &mut ScrollState::default());
    let (w, h) = (screen.size().width, screen.size().height);
    let rgb = screen.to_rgb();
    let s = args.scale;
    let mut scaled = Vec::with_capacity(rgb.len() * (s * s) as usize);
    for y in 0..h * s {
        for x in 0..w * s {
            let i = ((y / s) * w + x / s) as usize * 3;
            scaled.extend_from_slice(&rgb[i..i + 3]);
        }
    }
    write_png(&args.out, w * s, h * s, &scaled)?;
    println!("wrote {} ({}x{})", args.out.display(), w * s, h * s);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
