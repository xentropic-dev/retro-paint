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

## Canvas, output, and screen resolutions

Three separate sizes are involved:

| Size | What it is | Set by |
|------|------------|--------|
| **Canvas** | The full-color working image in the native editor. | User, any size. |
| **Output** | The dithered CGA image. The dither runs at this size, PNG export uses it, and the DOS app's canvas window shows it at 1:1. | User, per project. For example, canvas ÷ 4. |
| **DOS screen** | The fake DOS app's whole screen, including its toolbars and canvas window. | Fixed: 320×200 (desktop) or 200×320 (mobile). |

Navigation:

- **Native editor:** the canvas can be scrolled and zoomed.
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
