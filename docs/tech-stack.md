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

1. On each document change, the editor flattens the layers, downscales to output resolution, and dithers on the CPU (off the UI thread). The result is an indexed image at output resolution, one palette index per pixel.
2. The editor sends that image, plus cursor position, active tool, and active color, to the DOS thread over a channel. Only the newest frame matters, so older ones are dropped.
3. The DOS thread draws the DOS screen (toolbars, palette strip, canvas window scrolled to follow the cursor, retro cursor) at 320×200 or 200×320 into an offscreen texture.
4. The CRT shader (GLSL port of an earlier Bevy project's `crt.wgsl`) renders that texture to the window.

## Where processing runs

- **Dither:** CPU, in Rust. Error diffusion is sequential, and the output image is small.
- **CRT effect:** GPU, as a raylib fragment shader.

## Window placement

Each window sets its own Wayland app_id: `retro-paint-editor` and `retro-paint-dos`. The repo ships a sample Hyprland config snippet, [contrib/hyprland.conf](../contrib/hyprland.conf) (see [contrib/README.md](../contrib/README.md)), with window rules that put the editor on the left monitor and the DOS view on the vertical right monitor, at the right size.

The mobile or desktop DOS layout is a per-project setting (saved in the project file). Switching it resizes the DOS window to 9:16 or 16:9. The DOS window asks for 540×960 (mobile) or 960×540 (desktop) when it opens and whenever `FrameMsg.layout` changes, and resets the canvas scroll; a tiling compositor may override the size; placement and size on Hyprland come from the window rules in `contrib/hyprland.conf`.
