//! Asking Hyprland to reshape the DOS window when the layout switches.
//!
//! On Wayland the compositor owns the window size and Hyprland ignores
//! client resizes, so a layout switch only refits the tube into whatever box
//! the window already has (see [`crate::frame_tube`]). Under Hyprland the DOS
//! thread can do better: it finds its own window over the Hyprland IPC socket
//! (by PID and class [`crate::APP_ID`]), floats it if it is tiled, and resizes
//! it to the 9:16 or 16:9 box that fits its monitor, centered on that monitor.
//!
//! The decisions are pure ([`plan_resize`], [`batch_request`]); the socket
//! talk runs on its own thread ([`HyprResizer`]) so frames never wait on it.
//! Any failure (no Hyprland, no socket, window not found, odd JSON) is
//! silently ignored: the per-frame refit stays the source of truth.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use rp_doc::Layout;
use serde_json::Value;

/// Minimum time between two resize requests. Layout switches arrive from the
/// editor at frame rate at worst; only the newest one within this window is
/// applied.
pub const RATE_LIMIT: Duration = Duration::from_millis(250);

/// Socket reads and writes give up after this long, so a stuck compositor
/// only stalls the resize thread, never frames.
const IO_TIMEOUT: Duration = Duration::from_millis(500);

/// A monitor as far as sizing goes, in Hyprland's logical layout space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Monitor {
    pub id: i64,
    /// Top-left in the global layout.
    pub x: i32,
    pub y: i32,
    /// Logical size: mode size divided by scale, swapped when rotated.
    pub w: i32,
    pub h: i32,
}

/// Our window as Hyprland reports it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Client {
    pub address: String,
    pub floating: bool,
    pub fullscreen: bool,
    pub monitor: i64,
    pub at: (i32, i32),
    pub size: (i32, i32),
}

/// What to ask Hyprland for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResizePlan {
    pub address: String,
    /// Tiled windows can't take an exact size; float first.
    pub float: bool,
    pub size: (i32, i32),
    /// Global top-left that centers `size` on the window's monitor.
    pub at: (i32, i32),
}

/// Logical monitor size: `mode` divided by `scale`, width and height swapped
/// for the 90° and 270° transforms (odd values, flipped or not).
pub fn logical_size(mode: (i32, i32), scale: f64, transform: i64) -> (i32, i32) {
    let s = if scale > 0.0 { scale } else { 1.0 };
    let w = (f64::from(mode.0) / s).round() as i32;
    let h = (f64::from(mode.1) / s).round() as i32;
    if transform % 2 == 1 { (h, w) } else { (w, h) }
}

/// The largest 9:16 (mobile) or 16:9 (desktop) box that fits in `area`.
/// On a monitor with the layout's own shape (portrait for mobile, landscape
/// for desktop) that fills the monitor, as the contrib window rules do; on
/// the other orientation it spans the short side.
pub fn fit_size(area: (i32, i32), layout: Layout) -> (i32, i32) {
    let (aw, ah) = (area.0.max(1), area.1.max(1));
    let (rw, rh) = match layout {
        Layout::Mobile => (9, 16),
        Layout::Desktop => (16, 9),
    };
    if aw * rh <= ah * rw {
        // Width-bound.
        (aw, aw * rh / rw)
    } else {
        (ah * rw / rh, ah)
    }
}

/// Decides how to reshape `client` for `layout`, or `None` when there is
/// nothing to do: its monitor is unknown, it is fullscreen, or it already
/// floats at the target size and place.
pub fn plan_resize(client: &Client, monitors: &[Monitor], layout: Layout) -> Option<ResizePlan> {
    if client.fullscreen {
        return None;
    }
    let mon = monitors.iter().find(|m| m.id == client.monitor)?;
    let size = fit_size((mon.w, mon.h), layout);
    let at = (mon.x + (mon.w - size.0) / 2, mon.y + (mon.h - size.1) / 2);
    if client.floating && client.size == size && client.at == at {
        return None;
    }
    Some(ResizePlan {
        address: client.address.clone(),
        float: !client.floating,
        size,
        at,
    })
}

/// The dispatchers for `plan`, every one addressed to our own window.
///
/// Resize and move go out twice. Hyprland resizes a floating window about
/// its center, so growing by an odd amount leaves a half-pixel offset that
/// the first move rounds into a 1 px taller window (seen live on 0.56: a
/// 1080x607 window asked for 1080x1920 at y=0 ended up 1080x1921). The
/// second pass starts from an aligned box and lands exactly.
pub fn dispatches(plan: &ResizePlan) -> Vec<String> {
    let target = format!("address:{}", plan.address);
    let mut out = Vec::new();
    if plan.float {
        out.push(format!("dispatch setfloating {target}"));
    }
    for _ in 0..2 {
        out.push(format!(
            "dispatch resizewindowpixel exact {} {},{target}",
            plan.size.0, plan.size.1
        ));
        out.push(format!(
            "dispatch movewindowpixel exact {} {},{target}",
            plan.at.0, plan.at.1
        ));
    }
    out
}

/// One IPC request running `plan`'s dispatchers in order.
pub fn batch_request(plan: &ResizePlan) -> String {
    format!("[[BATCH]]{}", dispatches(plan).join(";"))
}

/// Our DOS window in `hyprctl clients -j` output: same PID, class
/// `class`, mapped.
pub fn find_client(clients_json: &str, pid: u32, class: &str) -> Option<Client> {
    let v: Value = serde_json::from_str(clients_json).ok()?;
    v.as_array()?.iter().find_map(|c| {
        let mapped = c.get("mapped").and_then(Value::as_bool).unwrap_or(true);
        if !mapped
            || c.get("pid")?.as_u64()? != u64::from(pid)
            || c.get("class")?.as_str()? != class
        {
            return None;
        }
        Some(Client {
            address: c.get("address")?.as_str()?.to_owned(),
            floating: c.get("floating")?.as_bool()?,
            fullscreen: c.get("fullscreen").and_then(Value::as_i64).unwrap_or(0) != 0,
            monitor: c.get("monitor")?.as_i64()?,
            at: pair(c.get("at")?)?,
            size: pair(c.get("size")?)?,
        })
    })
}

/// Monitors from `hyprctl monitors -j` output, in logical size.
pub fn parse_monitors(monitors_json: &str) -> Vec<Monitor> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(monitors_json) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|m| {
            let int = |k: &str| m.get(k)?.as_i64();
            let (w, h) = logical_size(
                (int("width")? as i32, int("height")? as i32),
                m.get("scale")?.as_f64()?,
                int("transform").unwrap_or(0),
            );
            Some(Monitor {
                id: int("id")?,
                x: int("x")? as i32,
                y: int("y")? as i32,
                w,
                h,
            })
        })
        .collect()
}

fn pair(v: &Value) -> Option<(i32, i32)> {
    let a = v.as_array()?;
    Some((a.first()?.as_i64()? as i32, a.get(1)?.as_i64()? as i32))
}

/// The Hyprland request socket, from the environment Hyprland gives its
/// children. `None` outside Hyprland.
pub fn socket_path(signature: Option<&str>, runtime_dir: Option<&str>) -> Option<PathBuf> {
    let sig = signature.filter(|s| !s.is_empty())?;
    let base = runtime_dir
        .filter(|s| !s.is_empty())
        .map(|d| PathBuf::from(d).join("hypr"))
        .unwrap_or_else(|| PathBuf::from("/tmp/hypr"));
    Some(base.join(sig).join(".socket.sock"))
}

/// Sends one request and reads the whole reply (Hyprland closes the
/// connection after answering).
fn request(socket: &PathBuf, req: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(socket)?;
    s.set_read_timeout(Some(IO_TIMEOUT))?;
    s.set_write_timeout(Some(IO_TIMEOUT))?;
    s.write_all(req.as_bytes())?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    Ok(out)
}

/// Looks up our window and reshapes it for `layout`. Errors are dropped by
/// the caller.
fn apply(socket: &PathBuf, pid: u32, layout: Layout) -> std::io::Result<()> {
    let clients = request(socket, "j/clients")?;
    let Some(client) = find_client(&clients, pid, crate::APP_ID) else {
        return Ok(());
    };
    let monitors = parse_monitors(&request(socket, "j/monitors")?);
    if let Some(plan) = plan_resize(&client, &monitors, layout) {
        request(socket, &batch_request(&plan))?;
    }
    Ok(())
}

/// Background resizer: [`HyprResizer::request`] never blocks; a thread
/// applies the newest requested layout, at most once per [`RATE_LIMIT`].
/// The thread ends when the resizer is dropped.
pub struct HyprResizer {
    tx: Sender<Layout>,
}

impl HyprResizer {
    /// Starts the resizer when running under Hyprland
    /// (`HYPRLAND_INSTANCE_SIGNATURE` set), else `None`.
    pub fn from_env() -> Option<Self> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
        let run = std::env::var("XDG_RUNTIME_DIR").ok();
        let socket = socket_path(sig.as_deref(), run.as_deref())?;
        let (tx, rx) = mpsc::channel();
        let pid = std::process::id();
        std::thread::Builder::new()
            .name("rp-dos-hypr".into())
            .spawn(move || worker(&rx, &socket, pid))
            .ok()?;
        Some(Self { tx })
    }

    /// Asks for the window shape of `layout`. Never blocks.
    pub fn request(&self, layout: Layout) {
        let _ = self.tx.send(layout);
    }
}

fn worker(rx: &Receiver<Layout>, socket: &PathBuf, pid: u32) {
    while let Ok(mut layout) = rx.recv() {
        // Coalesce: only the newest layout matters.
        while let Ok(newer) = rx.try_recv() {
            layout = newer;
        }
        let _ = apply(socket, pid, layout);
        std::thread::sleep(RATE_LIMIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Abridged from real `hyprctl monitors -j` / `clients -j` output on
    // Hyprland 0.56: a landscape DP-1 and a rotated HDMI-A-1 to its right.
    const MONITORS: &str = r#"[
      {"id":0,"name":"HDMI-A-1","width":1920,"height":1080,"x":1920,"y":0,
       "scale":1.00,"transform":1,"reserved":[0,26,0,0]},
      {"id":1,"name":"DP-1","width":1920,"height":1080,"x":0,"y":0,
       "scale":1.00,"transform":0,"reserved":[0,26,0,0]}
    ]"#;

    const CLIENTS: &str = r#"[
      {"address":"0x1111","mapped":true,"at":[10,36],"size":[1900,1034],
       "floating":false,"monitor":1,"class":"retro-paint-editor","pid":4242,
       "fullscreen":0},
      {"address":"0x2222","mapped":true,"at":[1920,0],"size":[1080,1920],
       "floating":true,"monitor":0,"class":"retro-paint-dos","pid":4242,
       "fullscreen":0},
      {"address":"0x3333","mapped":true,"at":[0,0],"size":[540,960],
       "floating":false,"monitor":1,"class":"retro-paint-dos","pid":999,
       "fullscreen":0}
    ]"#;

    fn monitors() -> Vec<Monitor> {
        parse_monitors(MONITORS)
    }

    #[test]
    fn logical_size_divides_scale_and_swaps_rotations() {
        assert_eq!(logical_size((1920, 1080), 1.0, 0), (1920, 1080));
        assert_eq!(logical_size((1920, 1080), 1.0, 1), (1080, 1920));
        assert_eq!(logical_size((1920, 1080), 1.0, 3), (1080, 1920));
        assert_eq!(logical_size((1920, 1080), 1.0, 5), (1080, 1920));
        assert_eq!(logical_size((1920, 1080), 1.0, 2), (1920, 1080));
        assert_eq!(logical_size((3840, 2160), 2.0, 0), (1920, 1080));
        assert_eq!(logical_size((2880, 1800), 1.5, 0), (1920, 1200));
    }

    #[test]
    fn fit_fills_a_matching_monitor() {
        assert_eq!(fit_size((1080, 1920), Layout::Mobile), (1080, 1920));
        assert_eq!(fit_size((1920, 1080), Layout::Desktop), (1920, 1080));
        assert_eq!(fit_size((2560, 1440), Layout::Desktop), (2560, 1440));
    }

    #[test]
    fn fit_spans_the_short_side_on_the_other_orientation() {
        assert_eq!(fit_size((1080, 1920), Layout::Desktop), (1080, 607));
        assert_eq!(fit_size((1920, 1080), Layout::Mobile), (607, 1080));
        // Not exactly 16:9 / 9:16 monitors still fit inside.
        assert_eq!(fit_size((1920, 1200), Layout::Desktop), (1920, 1080));
        assert_eq!(fit_size((1200, 1920), Layout::Mobile), (1080, 1920));
        for area in [(1080, 1920), (1920, 1080), (1366, 768), (1200, 1920)] {
            for layout in [Layout::Mobile, Layout::Desktop] {
                let (w, h) = fit_size(area, layout);
                assert!(w <= area.0 && h <= area.1, "{area:?} {layout:?}");
                assert!(w == area.0 || h == area.1, "{area:?} {layout:?}");
            }
        }
    }

    #[test]
    fn parses_monitors_in_logical_space() {
        assert_eq!(
            monitors(),
            vec![
                Monitor {
                    id: 0,
                    x: 1920,
                    y: 0,
                    w: 1080,
                    h: 1920
                },
                Monitor {
                    id: 1,
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080
                },
            ]
        );
        assert!(parse_monitors("not json").is_empty());
    }

    #[test]
    fn finds_only_our_dos_window() {
        let c = find_client(CLIENTS, 4242, "retro-paint-dos").unwrap();
        assert_eq!(
            c,
            Client {
                address: "0x2222".into(),
                floating: true,
                fullscreen: false,
                monitor: 0,
                at: (1920, 0),
                size: (1080, 1920),
            }
        );
        // Another process's DOS window, or our editor, is never picked.
        assert_eq!(
            find_client(CLIENTS, 999, "retro-paint-dos")
                .unwrap()
                .address,
            "0x3333"
        );
        assert_eq!(find_client(CLIENTS, 5, "retro-paint-dos"), None);
        assert_eq!(find_client("garbage", 4242, "retro-paint-dos"), None);
    }

    #[test]
    fn rule_placed_mobile_window_needs_nothing() {
        let c = find_client(CLIENTS, 4242, "retro-paint-dos").unwrap();
        assert_eq!(plan_resize(&c, &monitors(), Layout::Mobile), None);
    }

    #[test]
    fn desktop_on_the_portrait_monitor_spans_its_width_centered() {
        let c = find_client(CLIENTS, 4242, "retro-paint-dos").unwrap();
        let plan = plan_resize(&c, &monitors(), Layout::Desktop).unwrap();
        assert_eq!(
            plan,
            ResizePlan {
                address: "0x2222".into(),
                float: false,
                size: (1080, 607),
                at: (1920, 656),
            }
        );
        assert_eq!(
            batch_request(&plan),
            "[[BATCH]]dispatch resizewindowpixel exact 1080 607,address:0x2222;\
             dispatch movewindowpixel exact 1920 656,address:0x2222;\
             dispatch resizewindowpixel exact 1080 607,address:0x2222;\
             dispatch movewindowpixel exact 1920 656,address:0x2222"
        );
    }

    #[test]
    fn tiled_window_is_floated_first() {
        let c = find_client(CLIENTS, 999, "retro-paint-dos").unwrap();
        let plan = plan_resize(&c, &monitors(), Layout::Desktop).unwrap();
        assert_eq!(plan.size, (1920, 1080));
        assert_eq!(plan.at, (0, 0));
        assert_eq!(
            dispatches(&plan),
            vec![
                "dispatch setfloating address:0x3333".to_owned(),
                "dispatch resizewindowpixel exact 1920 1080,address:0x3333".to_owned(),
                "dispatch movewindowpixel exact 0 0,address:0x3333".to_owned(),
                "dispatch resizewindowpixel exact 1920 1080,address:0x3333".to_owned(),
                "dispatch movewindowpixel exact 0 0,address:0x3333".to_owned(),
            ]
        );
    }

    #[test]
    fn fullscreen_or_unknown_monitor_is_left_alone() {
        let mut c = find_client(CLIENTS, 4242, "retro-paint-dos").unwrap();
        c.fullscreen = true;
        assert_eq!(plan_resize(&c, &monitors(), Layout::Desktop), None);
        c.fullscreen = false;
        c.monitor = 7;
        assert_eq!(plan_resize(&c, &monitors(), Layout::Desktop), None);
    }

    #[test]
    fn socket_path_needs_the_hyprland_signature() {
        assert_eq!(socket_path(None, Some("/run/user/1000")), None);
        assert_eq!(socket_path(Some(""), Some("/run/user/1000")), None);
        assert_eq!(
            socket_path(Some("abc"), Some("/run/user/1000")),
            Some(PathBuf::from("/run/user/1000/hypr/abc/.socket.sock"))
        );
        assert_eq!(
            socket_path(Some("abc"), None),
            Some(PathBuf::from("/tmp/hypr/abc/.socket.sock"))
        );
    }

    #[test]
    fn resizer_request_never_blocks_without_a_socket() {
        let (tx, rx) = mpsc::channel();
        let socket = PathBuf::from("/nonexistent/hypr/.socket.sock");
        let h = std::thread::spawn(move || worker(&rx, &socket, 1));
        let r = HyprResizer { tx };
        for _ in 0..100 {
            r.request(Layout::Desktop);
        }
        drop(r);
        h.join().unwrap();
    }
}
