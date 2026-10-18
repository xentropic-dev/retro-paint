# Tech Stack

Draft.

## Language

- **Rust**

## Libraries

| Part | Library | Role |
|------|---------|------|
| Main editor (full-color view) | **GPUI** | The real UI: tools, layer panel, brushes, image insertion. See [Aesthetics](aesthetics.md). |
| Preview screen (DOS view) | **raylib** | The retro twin: downscaled, dithered, CGA-recolored image with the retro cursor. Captured with OBS Studio. See [Aesthetics](aesthetics.md) and [Filter and Palette](filter-and-palette.md). |

## Dependencies

- **GPUI:** from the Zed git repository, pinned to a specific commit. The crates.io releases lag behind and the API changes often.
- **raylib:** the `raylib` crate (raylib-rs **6.x**, features `GLFW_BUILD_WAYLAND` and `SUPPORT_IMAGE_GENERATION`), which bundles and builds raylib's C source. We use 6.x rather than 5.x because the 5.x `wayland` feature links against a system `libglfw` (not installed by default on Arch), while 6.x builds raylib's bundled GLFW with the Wayland backend.

## Target platform

Linux only for v1 (Arch, Hyprland on Wayland).

## Process model

One process, two threads:

- **Main thread:** GPUI editor, which owns the document (canvas, layers, settings).
- **DOS view thread:** raylib window, which draws the fake DOS app and runs the CRT shader.

The `spike-two-windows` spike confirmed this works on Hyprland/Wayland: both windows are native Wayland clients (`xwayland: false`) of one process, with app_ids `retro-paint-editor` (GPUI, main thread) and `retro-paint-dos` (raylib, thread `dos-view`). No child-process fallback is needed.

**DOS window app_id.** raylib's `InitWindow` calls `glfwDefaultWindowHints()` just before creating the window, so a `GLFW_WAYLAND_APP_ID` hint set beforehand is lost. `rp-dos` defines `__wrap_glfwDefaultWindowHints`, which calls the real function and then sets the Wayland app_id and X11 class. Every binary that links `rp-dos` must pass `-Wl,--wrap=glfwDefaultWindowHints`; the `build.rs` of `rp-dos` (for its tests) and `retro-paint` emit it.

**Shutdown.** `rp_dos::run_window(rx, stop)` runs until its `StopSignal` fires or its window is closed. When the editor quits (ctrl-q or closing the editor window), `rp_editor::run` returns; `retro-paint` then fires the stop signal, joins the `dos-view` thread (which closes the DOS window within a frame; it waits at most 2 s), and exits. Closing the DOS window first only ends the view: the editor keeps running and its frames are dropped.

## Data flow between views

1. On each document change, the editor flattens the layers, downscales to output resolution, and dithers on the CPU (off the UI thread). The result is an indexed image at output resolution, one palette index per pixel. The same flattened full-color image (converted to BGRA, tagged with the document version) goes back to the editor's canvas view through a latest-only slot that a GPUI task awaits; the UI thread never flattens, ignores results older than the image it shows, and keeps showing the previous image while a render is in flight. The worker is two threads: `render-worker` flattens and publishes the canvas image first, then hands the flattened image to `render-dos`, which downscales, dithers and sends the DOS frame, so the canvas never waits for the dither. The worker drops its document snapshot right after flattening; while it still holds one, a brush stroke queues its dabs instead of writing to the shared layer (which would copy the whole layer on the UI thread) and lays them as soon as the snapshot is released.
2. The editor sends that image, plus cursor position, active tool, and active color, to the DOS thread over a channel. Only the newest frame matters, so older ones are dropped.
3. The DOS thread draws the DOS screen (toolbars, palette strip, canvas window scrolled to follow the cursor, retro cursor) at 320×200 or 200×320 into an offscreen texture.
4. The CRT shader (GLSL port of an earlier Bevy project's `crt.wgsl`) renders that texture to the window.

## Where processing runs

- **Dither:** CPU, in Rust. Error diffusion is sequential, and the output image is small.
- **CRT effect:** GPU, as a raylib fragment shader.

## Window placement

Each window sets its own Wayland app_id: `retro-paint-editor` and `retro-paint-dos`. The repo ships a sample Hyprland config snippet, [contrib/hyprland.example.conf](../contrib/hyprland.example.conf) (see [contrib/README.md](../contrib/README.md)), with window rules that put the editor on the left monitor and the DOS view on the vertical right monitor, at the right size.

The mobile or desktop DOS layout is a per-project setting (saved in the project file). The DOS window asks for 540×960 (mobile) or 960×540 (desktop) when it opens. When `FrameMsg.layout` changes it resets the canvas scroll and, except on Wayland, asks for the new layout's size again. On Wayland it never resizes itself through GLFW: the compositor owns the size (Hyprland ignores client resizes even for floating windows, and the buffer would end up cropped or in a corner), so initial placement and size come from the window rules in `contrib/hyprland.example.conf`.

Under Hyprland (`HYPRLAND_INSTANCE_SIGNATURE` set) a layout change also asks the compositor for a new window shape (`rp-dos` `hypr` module). A background thread talks to the Hyprland request socket (`$XDG_RUNTIME_DIR/hypr/<sig>/.socket.sock`, the same one `hyprctl` uses): it finds the DOS window by our PID and class `retro-paint-dos` in `j/clients`, reads its monitor from `j/monitors` (logical size: mode ÷ scale, swapped when rotated), and sends one batch: `setfloating` if the window is tiled, then `resizewindowpixel exact` and `movewindowpixel exact` to the largest 9:16 (mobile) or 16:9 (desktop) box that fits that monitor, centered on it. On a monitor of the layout's own shape that fills it (bars included, like the contrib `size (monitor_w) (monitor_h)` rule); on the other orientation it spans the short side (desktop on a 1080×1920 portrait monitor is 1080×607, centered). Resize and move are sent twice because Hyprland resizes floating windows about their center and an odd growth otherwise leaves the window 1 px too tall. Nothing is sent when the window is fullscreen or already at that box. Decisions:

- Only layout *changes* trigger it, never the first frame: the window rules own initial placement, and a window without rules is left as the compositor placed it until the user switches layout. (The DOS window starts on the idle mobile frame, so opening a desktop project counts as a change and does resize.)
- Requests never block frames (a channel to the thread), and the thread applies only the newest layout, at most once per 250 ms. Socket I/O times out after 500 ms.
- Any failure (not Hyprland, no socket, window not found, unexpected JSON) is ignored silently; the window keeps its size and the tube refit below still draws correctly. Only our own window is ever addressed (`address:<addr>`).
- The contrib rules are static (applied when the window maps), so they do not fight the resize; checked live on Hyprland 0.56 with the rules applied.

Every frame the tube is fit to the real framebuffer size (raylib's render size, not the requested screen size), so it stays correct when the compositor tiles, pins or the user resizes the window.
