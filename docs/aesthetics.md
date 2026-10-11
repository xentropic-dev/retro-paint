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

- File menu at the top (save, open, PNG export, see [File Format](file-format.md); and Project Settings, see [Canvas, output, and screen resolutions](#canvas-output-and-screen-resolutions)).
- Toolbar ribbon at the top.
- Full set of tools.
- Layer selection panel, inspired by GIMP or Photoshop.
- Reorder layers.
- Insert images.
- Apply a brush. GIMP brushes are supported in `.gbr` (bitmap) and `.vbr` (parametric) formats.

### Layers panel

Top of the right-hand panel column. The list shows the top of the stack at the top. Each row has a visibility toggle, the name, and badges for an image layer (IMG), a pixel or position lock (LOCK) and a mask (MASK); the selected row is highlighted. Click a row to select it; double-click it to rename inline (Enter applies, Escape cancels, clicking elsewhere applies; an empty name keeps the old one). Above the list are controls for the selected layer: blend mode (Normal, Multiply, Screen, Overlay), opacity (a click-to-set bar in 5% segments plus −/+ steppers of 5%), Pixels and Position lock toggles, and Add/Remove mask. Below it are New, Image, Delete, Up and Down.

- **New** adds a transparent, canvas-sized paint layer directly above the selected layer, named `Layer N` (one more than the highest existing `Layer N`), and selects it.
- **Image** is File > Insert Image… (see below).
- **Delete** removes the selected layer, then selects the layer that was below it (or the new bottom layer). The last remaining layer cannot be deleted.
- **Up / Down** move the selected layer one step and stop at the ends.
- **Add mask** creates an all-white (fully showing) canvas-sized grayscale mask. Masks live in canvas space: moving, scaling or rotating the layer leaves its mask where it is (v1 decision).
- A new or opened document selects its top layer.

### Inserting images

- **File > Insert Image…** (Ctrl+I), the Layers panel's **Image** button, or dropping files from a file manager onto the canvas. The dialog is the desktop's file chooser (XDG portal) and allows several files; each becomes its own layer, in order.
- PNG, JPEG and WebP are read by content, not extension, as RGBA8 with straight alpha; the EXIF orientation is applied so the picture is upright. Decoding runs off the UI thread; the status bar shows `Loading <file>…`, then the result or why it failed (unreadable, unsupported format, corrupt, too large).
- The new image layer goes directly above the selected layer, is named after the file stem, and becomes selected. It is centered on the canvas at 1:1 on whole pixels; an image larger than the canvas is scaled down uniformly to fit inside it, never up.
- The source is kept at full resolution under its `rp_format::source_hash`; inserting the same picture again reuses the stored source.

### Moving and transforming

- **Move** drags the selected layer (paint or image) from wherever the left button goes down. **Select** does the same, and on an image layer it also draws the transformed bounding box with four square corner handles and a round rotation handle on a stem above the top edge. Handles are drawn and hit-tested in screen pixels, so they keep their size at any zoom. Paint layers have no handles; they only move.
- Moves are whole canvas pixels for both layer kinds (the drag offset is rounded), so a 1:1 image stays pixel-aligned; an image keeps any fractional position it already had.
- With Move or Select active, the arrow keys nudge the selected layer 1 px, Shift+arrow 10 px.
- A corner handle scales about the opposite corner, which stays fixed (also when rotated). The aspect ratio is kept; hold Shift to scale freely. The box never flips and never gets smaller than 1 canvas pixel on a side.
- The rotation handle rotates about the image center; hold Shift to snap to 15° steps. Pressing or releasing Shift mid-drag takes effect immediately.
- A position-locked layer refuses moves, nudges, scaling and rotation; the status bar says why. A pixel lock does not stop moving.
- The status bar shows the move offset, scale percentages or angle while dragging.

### Painting

- **Brush** paints the selected paint layer: the left button paints the foreground color, the right button the background color. **Eraser** lowers alpha with the same brush (either button). **Picker** sets the foreground (left) or background (right) to the flattened color under the cursor, as the canvas shows it; fully transparent pixels are not picked.
- The default brush is a built-in round brush with a **Hard / Soft** toggle in the ribbon. Size is 1–256 canvas pixels: the ribbon's −/+ stepper or `[` / `]` step through 1–10, then by 5 to 50, by 10 to 100, by 25 to 250, then 256.
- Dabs are spaced along the pointer path at the brush's spacing, so fast strokes stay continuous. Strokes follow the layer's offset and are clipped to the layer.
- Locked layers and image layers refuse strokes; the status bar says why.
- The DOS cursor's brush outline is the brush size in output pixels (size × output / canvas).

### Colors and swatches

- The ribbon's right end shows the foreground chip over the background chip. **⇄** next to them (or `X`) swaps them; **◩** (or `D`) resets to black foreground, white background, GIMP-style. `X` and `D` work with no modifiers held and not while renaming a layer.
- The **Swatches** panel (bottom of the right column) is a grid of the document's saved full-color swatches, oldest first. Click a swatch to set the foreground, right-click to set the background, middle-click to remove it. The swatch equal to the foreground has a white ring.
- **+ Add fg** saves the foreground at the end. A color that is already saved is not duplicated; it moves to the end. A document keeps at most 64 swatches; adding to a full list is refused with a status message (re-adding a saved color still moves it).
- **− Remove fg** removes the swatch equal to the foreground (pick it first by clicking it); if none matches, the status bar says so.
- The picker and the swatches are the ways to set a color in v1; there is no RGB/HSV color editor yet.
- Colors and swatches are stored in the document (see [File Format](file-format.md)) but are not pixel edits: changing them never re-renders or re-dithers (`doc_version` does not move). A foreground change reaches the DOS view as an overlay update of its active color. For undo: swatch add/remove are document edits and should be undoable steps; fg/bg changes (picker, swatch click, swap, reset) are not undo steps, like GIMP.

## Canvas, output, and screen resolutions

Three separate sizes are involved:

| Size | What it is | Set by |
|------|------------|--------|
| **Canvas** | The full-color working image in the native editor. | User, any size. |
| **Output** | The dithered CGA image. The dither runs at this size, PNG export uses it, and the DOS app's canvas window shows it at 1:1. | User, per project. For example, canvas ÷ 4. |
| **DOS screen** | The fake DOS app's whole screen, including its toolbars and canvas window. | Fixed: 320×200 (desktop) or 200×320 (mobile). |

**Project Settings** (File > Project Settings…, Ctrl+Shift+P) is a modal dialog that edits a draft of all three settings and applies it as one document edit (one undo step; the edit returns the document as it was). Nothing changes until Apply; Cancel or Escape discards the draft, Enter applies.

- **Layout:** Mobile (9:16, 200×320 DOS screen) or Desktop (16:9, 320×200). Switching resizes the DOS window.
- **Canvas:** W×H fields (click a number and type, Enter or Tab sets it), −100/−10/+10/+100 steppers, presets 800×1280, 1280×800, 1600×2560, 2560×1600, and a 3×3 anchor grid (default center). Sides are clamped to 1–8192.
- **Output:** W×H fields with −10/−1/+1/+10 steppers, presets canvas ÷ 1, ÷ 2, ÷ 3, ÷ 4, ÷ 8 (rounded down, at least 1) and DOS-native (the layout's DOS screen size).
- **Output link:** while a ÷ N preset is chosen, the output follows canvas ÷ N when the canvas size changes. The link is on when the dialog opens if the output already equals canvas ÷ N for a preset N (so a new document, 800×1280 → 200×320, stays ÷ 4). Typing or stepping the output, or picking DOS-native, unlinks it. An unlinked DOS-native output follows a layout switch (200×320 ↔ 320×200).
- **Canvas resize never resamples.** All content moves by the anchor offset (0 at the anchored edge, the whole growth at the far edge, half of it rounded down in the middle). Paint layers are cropped or padded with transparency and rebased to offset 0, so pixels already off the canvas are dropped. Masks are cropped or padded with 255 (unmasked). Image layers keep their full-resolution source, scale and rotation and only shift. Locks do not block a canvas resize. The view refits afterwards.
- File > New still makes the default document (800×1280, mobile, output ÷ 4); change it with Project Settings.

Navigation:

- **Native editor:** the canvas can be scrolled and zoomed. It opens fitted to the window (and keeps refitting on resize until you pan or zoom). Zoom runs 12.5%–3200%: ctrl+wheel zooms about the cursor, ctrl+= / ctrl+- step the zoom, ctrl+1 is 100%, ctrl+0 fits. The wheel scrolls; middle-drag or space+drag pans. Transparency shows as a checkerboard on a dark surround, and pixels stay sharp when zoomed in. A new document starts with one opaque white paint layer named "Background".
- **DOS app:** the output image can only be scrolled, never zoomed, because it is always shown 1:1 in DOS screen pixels. It auto-scrolls to keep the cursor's position in view, like an old paint program panning as you paint near the edge. It ignores the native editor's zoom level. The window pans when the cursor comes within 16 output pixels of an edge and never scrolls past the image; on an axis where the image is smaller than the window, the image is centered on a dark gray backdrop.

## DOS view (right)

- Looks like a complete retro DOS CGA paint app, with its own toolbars and a canvas window around the image.
- Toolbars are a Deluxe Paint-style tool strip and a CGA palette strip. They are display-only and mirror the native editor's state, highlighting the active tool and color so viewers see tool changes happen. The DOS window does not accept input.
- Before the editor's first frame, it shows the DOS screen with an empty (black) canvas in the mobile layout. After that it always shows the newest frame and keeps the last one when nothing new arrives.
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
