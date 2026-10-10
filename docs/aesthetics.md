# Aesthetics

Draft.

## A tool within a tool

retro-paint runs as two twin views of the same image on screen at once:

- **Full-color view (the real UI).** This is where the actual work happens. Photobashing is done here at higher color and resolution.
- **DOS view (the retro twin).** Looks like a complete little DOS CGA paint app with its own toolbars. Its canvas window shows the image downscaled, dithered, and recolored to the CGA palette in real time (see [Filter and Palette](filter-and-palette.md)).

## Screen capture

The DOS view is the part of the screen captured with OBS Studio. The goal is to make videos that look as though the image is being drawn and bashed in retro space, on an old DOS machine.

The main target is short-form video content on mobile.

## DOS view layouts

The DOS view supports two layouts:

- **Mobile view:** vertical, for short-form mobile video. This is the primary target.
- **Desktop view:** for desktop video.

## Multi-monitor layout (default)

The two views can be split across monitors, and this is the planned default:

- **Left monitor:** the full-color view with the real UI.
- **Right monitor (vertical):** the DOS view with the filter applied, in the mobile view. The vertical monitor captures well for mobile.

## Single-monitor layout

The editor and DOS view are always two separate OS windows. With one screen, the user arranges them, and OBS captures the DOS view window by itself.

## Full-color view (left)

- File menu at the top (save, open, and PNG export; see [File Format](file-format.md)).
- Toolbar ribbon at the top.
- Full set of tools.
- Layer selection panel, inspired by GIMP or Photoshop.
- Reorder layers.
- Insert images.
- Apply a brush. GIMP brushes are supported in `.gbr` (bitmap) and `.vbr` (parametric) formats.

### Layers panel

Top of the right-hand panel column. The list shows the top of the stack at the top. Each row has a visibility toggle, the name, and badges for an image layer (IMG), a pixel or position lock (LOCK) and a mask (MASK); the selected row is highlighted. Click a row to select it; double-click it to rename inline (Enter applies, Escape cancels, clicking elsewhere applies; an empty name keeps the old one). Above the list are controls for the selected layer: blend mode (Normal, Multiply, Screen, Overlay), opacity (a click-to-set bar in 5% segments plus −/+ steppers of 5%), Pixels and Position lock toggles, and Add/Remove mask. Below it are New, Delete, Up and Down.

- **New** adds a transparent, canvas-sized paint layer directly above the selected layer, named `Layer N` (one more than the highest existing `Layer N`), and selects it.
- **Delete** removes the selected layer, then selects the layer that was below it (or the new bottom layer). The last remaining layer cannot be deleted.
- **Up / Down** move the selected layer one step and stop at the ends.
- **Add mask** creates an all-white (fully showing) canvas-sized grayscale mask.
- A new or opened document selects its top layer.

## Canvas, output, and screen resolutions

Three separate sizes are involved:

| Size | What it is | Set by |
|------|------------|--------|
| **Canvas** | The full-color working image in the native editor. | User, any size. |
| **Output** | The dithered CGA image. The dither runs at this size, PNG export uses it, and the DOS app's canvas window shows it at 1:1. | User, per project. For example, canvas ÷ 4. |
| **DOS screen** | The fake DOS app's whole screen, including its toolbars and canvas window. | Fixed: 320×200 (desktop) or 200×320 (mobile). |

Navigation:

- **Native editor:** the canvas can be scrolled and zoomed. It opens fitted to the window (and keeps refitting on resize until you pan or zoom). Zoom runs 12.5%–3200%: ctrl+wheel zooms about the cursor, ctrl+= / ctrl+- step the zoom, ctrl+1 is 100%, ctrl+0 fits. The wheel scrolls; middle-drag or space+drag pans. Transparency shows as a checkerboard on a dark surround, and pixels stay sharp when zoomed in. A new document starts with one opaque white paint layer named "Background".
- **DOS app:** the output image can only be scrolled, never zoomed, because it is always shown 1:1 in DOS screen pixels. It auto-scrolls to keep the cursor's position in view, like an old paint program panning as you paint near the edge. It ignores the native editor's zoom level. The window pans when the cursor comes within 16 output pixels of an edge and never scrolls past the image; on an axis where the image is smaller than the window, the image is centered on a dark gray backdrop.

## DOS view (right)

- Looks like a complete retro DOS CGA paint app, with its own toolbars and a canvas window around the image.
- Toolbars are a Deluxe Paint-style tool strip and a CGA palette strip. They are display-only and mirror the native editor's state, highlighting the active tool and color so viewers see tool changes happen. The DOS window does not accept input.
- Shows a retro version of the user's cursor, mirroring what is done in the full-color view. The cursor changes with the active tool: a classic DOS arrow for select and move, and a crosshair or brush outline for painting. It is drawn in palette colors at DOS screen resolution, so it goes through the CRT shader too.

### Screen resolution and tube

| Layout | DOS screen | Tube shape |
|--------|------------|------------|
| Mobile (vertical) | 200×320 (CGA 320×200 mode turned on its side) | 9:16, filling the frame |
| Desktop | 320×200 | 4:3 |

The DOS screen is stretched to fill the tube. On desktop, the 4:3 tube gives pixels about 1.2× tall, as on real CGA hardware. On mobile, the tube is an imaginary vertical CRT that fills the whole 9:16 frame, with pixels about 1.1× tall, and there is no bezel art or anything else outside the tube. The desktop layout centers the 4:3 tube in a 16:9 frame with plain black sides, which can be cropped in OBS.

### CRT shader

The DOS view is rendered through a CRT post-process shader ported from an earlier Bevy project's `crt.wgsl` (WGSL/Bevy, to be ported to raylib GLSL). The DOS screen is drawn to an offscreen texture, then shown on a fullscreen quad with:

- barrel curvature and a black bezel where the tube curves away
- scanlines matched to the DOS screen's row count
- subtle time-driven flicker
- vignette
- phosphor tint, left at white so CGA colors pass through unchanged

The GLSL source is `crates/rp-dos/shaders/crt.fs`; `crates/rp-dos/src/crt.rs` holds its parameters (original defaults: curvature 0.18, scanlines 0.35, flicker 0.04, white phosphor), a pure-Rust mirror of the shader math for tests, and `tube_rect`, which places the tube in the window. One change from the original: the scanline term draws one dark gap per DOS screen row, on the boundary between rows, where the original darkened every other row. Row centers stay at full brightness, so CGA colors come through exactly. `cargo run -p rp-dos --example crt-demo -- mobile|desktop` (or `RP_DOS_DEMO=mobile|desktop` on the app) shows the sample screen through the shader.
