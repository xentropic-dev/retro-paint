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

The two views can be split across monitors, and this is the default (see `contrib/hyprland.conf`):

- **Left monitor:** the full-color view with the real UI.
- **Right monitor (vertical):** the DOS view with the filter applied, in the mobile view. The vertical monitor captures well for mobile.

## Single-monitor layout

The editor and DOS view are always two separate OS windows. With one screen, the user arranges them, and OBS captures the DOS view window by itself.

## Full-color view (left)

- File and Edit menus at the top. File: New (Ctrl+N), Open… (Ctrl+O), Insert Image… (Ctrl+I), Save (Ctrl+S), Save As… (Ctrl+Shift+S), Export PNG… (Ctrl+E), see [File Format](file-format.md); Project Settings… (Ctrl+Shift+P), see [Canvas, output, and screen resolutions](#canvas-output-and-screen-resolutions); Quit (Ctrl+Q). Edit: Undo (Ctrl+Z), Redo (Ctrl+Shift+Z or Ctrl+Y), see [Undo and redo](#undo-and-redo); Paste (Ctrl+V), see [Inserting images](#inserting-images); Duplicate Layer (Ctrl+J), Flip Horizontal, Flip Vertical, Rasterize Layer, see [Layers panel](#layers-panel); Select All (Ctrl+A), Deselect (Ctrl+D), Clear (Delete), see [Selecting](#selecting).
- Toolbar ribbon at the top, all icon buttons (the File/Edit menus stay words): the tools (Select, Lasso, Rect Select, Move, Brush, Eraser, Picker), Insert Image, the brush size −/+ around the size readout, the Hard/Soft tip toggle, then at the right end the fg/bg chips with swap and reset. Icons are 16×16 one-bit pixel art (a DOS-paint look) drawn as SVG and tinted with the theme's text color (white on the accent fill when selected, dimmed when a size step is at its limit); buttons are 28 px square. Hovering a button shows its name and shortcut, e.g. "Brush (B)". Tool keys, with no modifiers held and not while renaming a layer: `S` Select, `L` Lasso, `R` Rect Select, `M` Move, `B` Brush, `E` Eraser, `I` Picker.
- Full set of tools.
- Layer selection panel, inspired by GIMP or Photoshop.
- Reorder layers.
- Insert images.
- Apply a brush. v1 paints with a built-in round brush (Hard or Soft). `rp-brush` already reads GIMP `.gbr` (bitmap) and `.vbr` (parametric) brushes, but the editor has no way to load one yet.

### Panel column

The right-hand column (260 px wide) holds Layers, Filter and Swatches, top to bottom, each under a clickable header (▼ expanded, ▶ collapsed). Collapsed state is editor-session state: it survives New and Open but is not stored in the document or any file. Everything starts expanded. Sections take their natural height and the column scrolls as a whole when the window is too short; the layer list shows 3 to 8 rows and scrolls past that. The ribbon wraps onto a second line rather than clipping in narrow windows.

### Layers panel

Top of the right-hand panel column. The list shows the top of the stack at the top. Each row has a visibility toggle, a small mask thumbnail (masked layers only), the name, and badges for an image layer (IMG), a pixel or position lock (LOCK) and a mask (MASK); the selected row is highlighted. Click a row to select it; double-click it to rename inline (Enter applies, Escape cancels, clicking elsewhere applies; an empty name keeps the old one). Above the list are controls for the selected layer: blend mode (Normal, Multiply, Screen, Overlay), opacity (a click-to-set bar in 5% segments plus −/+ steppers of 5%), Pixels and Position lock toggles, and the mask row (Add mask; or Layer / Edit mask target toggles and Remove). Below it are two rows of buttons: New, Image, Delete, Up and Down; then Duplicate, Flip H, Flip V and Rasterize.

- **New** adds a transparent, canvas-sized paint layer directly above the selected layer, named `Layer N` (one more than the highest existing `Layer N`), and selects it.
- **Image** is File > Insert Image… (see below).
- **Delete** removes the selected layer, then selects the layer that was below it (or the new bottom layer). The last remaining layer cannot be deleted.
- **Up / Down** move the selected layer one step and stop at the ends.
- **Duplicate** (also Edit > Duplicate Layer, Ctrl+J) inserts a copy of the selected layer directly above it, named `<name> copy`, and selects it. Everything is copied: visibility, opacity, blend mode, locks, mask, and offset or transform. Paint pixels and masks are shared copy-on-write and an image copy references the same source, so a duplicate costs no memory until one side is edited. Locks never refuse a duplicate.
- **Flip H / Flip V** (also Edit > Flip Horizontal / Flip Vertical, no shortcut) mirror the selected layer as seen on screen. A paint layer mirrors its pixels about its own bounds (it does not move); a pixel lock refuses this, a position lock does not. An image layer mirrors its transform about the image center: the scale on that axis is negated and so is the rotation, so a rotated image flips about the canvas axis; the center stays put. A position lock refuses this, a pixel lock does not. The status bar says why a flip was refused.
- Flipping a paint layer also flips its mask about the same bounds, so they stay matched. Where the layer reaches past the canvas (and so past the mask), mask values mirrored out are dropped and the part mirrored in shows the layer (255, like an absent mask pixel); mask pixels outside the layer's bounds don't change. Flipping an image layer leaves its mask in place, like a transform.
- **Rasterize** (also Edit > Rasterize Layer, no shortcut) turns the selected image layer into a canvas-sized paint layer at offset 0, so the brush and eraser work on it. The pixels are that layer rendered on its own with the same sampling as flatten (as if visible, Normal, 100% opacity, no mask); the layer keeps its name, place in the stack, visibility, opacity, blend mode, mask and locks, so the flattened image does not change (scaled or rotated images are now stored as 8-bit pixels, at most one level off per channel; whole-pixel placements and flips are exact). Locks never refuse it: it changes neither how the layer looks nor where it is (a pixel-locked result still refuses strokes). A paint layer or a missing source is refused with a status message; success shows "Rasterized <name>". Undo brings the image layer back exactly.
- **Add mask** creates an all-white (fully showing) canvas-sized grayscale mask. Masks live in canvas space and stay canvas-sized. Moving the layer (Move/Select drag or arrow nudge) shifts its mask by the same whole-pixel delta: the uncovered edge is padded with white (showing) and mask values pushed off the canvas are lost (a single drag shifts the mask from where it was at the press, so dragging out and back within one drag loses nothing). Scaling or rotating an image layer leaves its mask where it is (v1 decision).
- **Editing a mask.** Each layer is edited either as **Layer** (its pixels) or **Mask**. Click a row's mask thumbnail or MASK badge (or the Edit mask toggle above the list) to select that layer and paint its mask; click it again (or Layer) to go back. The targeted thumbnail gets a white frame, the MASK badge turns white, and the status bar shows "Editing mask". The target belongs to the selected layer: selecting another layer, adding, duplicating, deleting or inserting a layer, New and Open, removing the mask, or undoing its Add Mask all go back to Layer. The thumbnail is the mask box-averaged to at most 40 px (shown at 20 px), rebuilt when the document changes.
- A new or opened document selects its top layer.

### Inserting images

- **File > Insert Image…** (Ctrl+I), the ribbon's Insert Image icon button (picture with a plus, right of the tools), the Layers panel's **Image** button, or dropping files from a file manager onto the canvas. The dialog is the desktop's file chooser (XDG portal) and allows several files; each becomes its own layer, in order.
- PNG, JPEG and WebP are read by content, not extension, as RGBA8 with straight alpha; the EXIF orientation is applied so the picture is upright. Decoding runs off the UI thread; the status bar shows `Loading <file>…`, then the result or why it failed (unreadable, unsupported format, corrupt, too large).
- The new image layer goes directly above the selected layer, is named after the file stem, and becomes selected. It is centered on the canvas at 1:1 on whole pixels; an image larger than the canvas is scaled down uniformly to fit inside it, never up.
- The source is kept at full resolution under its `rp_format::source_hash`; inserting the same picture again reuses the stored source.
- **Edit > Paste** (Ctrl+V) inserts from the Wayland clipboard the same way: image data (PNG first, then JPEG, WebP, other raster types) becomes a layer named "Pasted image"; copied files (`text/uri-list`, or text that is only `file://` URIs or absolute paths of existing files) insert those files, named after their stems. Anything else shows "Clipboard has no image". The clipboard is read through GPUI; when that yields no image or file list (GPUI prefers text and skips `text/uri-list`), `wl-paste` is asked off the UI thread. Paste does nothing while a dialog is open. Each pasted image is one undo step.

### Moving and transforming

- **Move** drags the selected layer (paint or image) from wherever the left button goes down. **Select** does the same, and on an image layer it also draws the transformed bounding box with four square corner handles and a round rotation handle on a stem above the top edge. Handles are drawn and hit-tested in screen pixels, so they keep their size at any zoom. Paint layers have no handles; they only move.
- Moves are whole canvas pixels for both layer kinds (the drag offset is rounded), so a 1:1 image stays pixel-aligned; an image keeps any fractional position it already had.
- With Move or Select active, the arrow keys nudge the selected layer 1 px, Shift+arrow 10 px.
- A corner handle scales about the opposite corner, which stays fixed (also when rotated). The aspect ratio is kept; hold Shift to scale freely. The box never flips and never gets smaller than 1 canvas pixel on a side.
- The rotation handle rotates about the image center; hold Shift to snap to 15° steps. Pressing or releasing Shift mid-drag takes effect immediately.
- A position-locked layer refuses moves, nudges, scaling and rotation; the status bar says why. A pixel lock does not stop moving.
- The status bar shows the move offset, scale percentages or angle while dragging.

### Transforming a selection

With a selection and the **Select** or **Move** tool (or **Edit > Transform Selection**, Ctrl+T, which switches to Select), the canvas shows a transform box on the selection bounds with the same handles as image layers. The editor logic is `rp-editor`'s `float_ops`.

- Dragging inside the box moves, a corner scales (aspect kept, Shift frees it, the opposite corner stays fixed), the knob rotates (Shift snaps to 15°); arrows nudge 1 px, Shift+arrows 10 px. The first of these lifts the selected pixels out of the selected paint layer (the hole is transparent; soft selection edges split alpha by coverage).
- While floating, the canvas and the DOS view show the result live (the piece is drawn as a temporary layer just above its layer, never in the document).
- **Enter**, a click outside the box, switching tool, layer or edit target, any File command (Save, Export, …) and any other edit or selection change apply it: the piece is stamped back into the same layer. **Escape** cancels and restores the layer byte for byte; the selection stays (a second Escape deselects). Lift, transforms and apply are one undo step, "Transform Selection"; Ctrl+Z while floating cancels. A piece applied where it was lifted records nothing.
- After applying, the selection follows the piece: its 50 % edge is moved, scaled and rotated with it (a whole-pixel move keeps it exact; soft edges become hard at 50 %). While floating, the dashed edge moves with a plain move and is hidden while scaled or rotated (the box shows the piece).
- Refused, with the reason in the status bar: image layers ("Rasterize it to transform a selection"), pixel- or position-locked layers, an empty selection, and the mask as edit target.
- While a selection exists its box replaces an image layer's own handles; a press outside the box still moves the layer. Deselect (Ctrl+D) to get the image handles back.

### Selecting

Select parts of the canvas with **Rect Select** (`R`) or **Lasso** (`L`), then erase them with Delete. The selection is soft-edged coverage (see `rp_doc::selection`); the editor logic is `rp-editor`'s `select_ops`.

- **Rect Select:** drag a rectangle; it snaps to whole pixels and is clipped to the canvas. Pressing Shift *during* the drag squares it. Shift held at the press means Add instead (to square an Add drag, release Shift and press it again). A click without a drag deselects.
- **Lasso:** an outline that starts with a drag is a freehand lasso and closes when the button is released. An outline that starts with a click is polygonal: each click adds a straight segment and a drag between clicks adds a freehand piece. It closes on a double-click, Enter, or a click on its first point (a marker appears there when the pointer is close enough). Escape cancels an unfinished outline; switching tools drops it too.
- **Modes**, from the modifiers at the press that starts the outline: Shift adds to the selection, Alt subtracts, neither replaces it.
- **Edit > Select All** (Ctrl+A) selects the whole canvas; **Edit > Deselect** (Ctrl+D) clears the selection.
- **Escape** steps back one thing per press. Whatever already owns Escape keeps it (dialogs, the color picker, a layer rename, Project Settings fields). Otherwise it drops an unfinished outline; else cancels a floating selection (layer restored, selection kept); else clears the selection like Ctrl+D (not an undo step).
- **Edit > Clear** (Delete or Backspace) erases the selected pixels of the selected paint layer (soft edges keep part of their alpha) as one undo step, "Clear Selection". It refuses on image layers ("Rasterize it to erase"), on pixel-locked layers, and with nothing selected; the status bar says why. With the mask as the edit target it fills the selected mask area white (shown) instead.
- The outline being drawn is a light line over a dark one; the selection edge is a dashed black-and-white line along pixel boundaries (where coverage reaches 50%). Both are 1 screen pixel wide at any zoom. The edge is traced once per selection change, not per frame.
- The selection belongs to the open document but is not part of the file or an undo step. It survives tool switches and undo/redo; New, Open and a canvas size change drop it.

### Painting

- **Brush** paints the selected paint layer: the left button paints the foreground color, the right button the background color. **Eraser** lowers alpha with the same brush (either button). **Picker** sets the foreground (left) or background (right) to the flattened color under the cursor, as the canvas shows it; fully transparent pixels are not picked.
- The default brush is a built-in round brush with a **Hard / Soft** toggle in the ribbon (a solid disc and a dithered disc). Size is 1–256 canvas pixels: the ribbon's −/+ icon buttons or `[` / `]` step through 1–10, then by 5 to 50, by 10 to 100, by 25 to 250, then 256.
- Dabs are spaced along the pointer path at the brush's spacing, so fast strokes stay continuous. Strokes follow the layer's offset and are clipped to the layer.
- Locked layers and image layers refuse strokes; the status bar says why (for an image layer it suggests Rasterize).
- With the mask as the edit target, Brush and Eraser paint the selected layer's mask instead: Brush with the left button paints black (hides), Brush right button and Eraser (either button) paint white (reveal). Each dab moves the mask toward black or white by the brush's coverage, so the soft brush feathers. Masks are canvas-space, so strokes ignore the layer's offset or transform, and image layers' masks paint the same way (their pixels are never touched). A pixel lock does not lock the mask (as in Photoshop). The Picker ignores the target and always picks the flattened color.
- The DOS cursor's brush outline is the brush size in output pixels (size × output / canvas).

### Colors and swatches

- The ribbon's right end shows the foreground chip over the background chip. the swap-arrows icon next to them (or `X`) swaps them; the small-chips icon (or `D`) resets to black foreground, white background, GIMP-style. `X` and `D` work with no modifiers held and not while renaming a layer.
- The **Swatches** panel (bottom of the right column) is a grid of the document's saved full-color swatches, oldest first. Click a swatch to set the foreground, right-click to set the background, middle-click to remove it. The swatch equal to the foreground has a white ring.
- **+ Add fg** saves the foreground at the end. A color that is already saved is not duplicated; it moves to the end. A document keeps at most 64 swatches; adding to a full list is refused with a status message (re-adding a saved color still moves it).
- **− Remove fg** removes the swatch equal to the foreground (pick it first by clicking it); if none matches, the status bar says so.
- **+ New color…** opens the color picker (below) to make a swatch; its OK (labelled **Add**) saves the picked color like **+ Add fg**, and the foreground is left alone.
- **Color picker.** Clicking the foreground or background chip opens a popover under the ribbon for that color:
  - A saturation/value square (saturation left → right, value bottom → top) and a vertical hue bar (red at the top, through yellow, green, cyan, blue, magenta, back to red). Click or drag either; a drag keeps going outside the control and clamps at its edges. HSV is over gamma-encoded sRGB. Picking a gray or black keeps the current hue (and black keeps the saturation), so the square does not jump.
  - Typed fields: **Hex** (`#rrggbb`; 3-digit `#rgb` also works) and **R / G / B** (0–255, larger values clamp to 255), the same entry as Project Settings: click a field and type, Enter or Tab (or a click elsewhere in the picker) sets it, Escape drops the typing. Tab moves on to the next field (Hex → R → G → B → Hex). A hex that is not 3 or 6 digits, or an empty field, keeps the color.
  - A row of the 16 CGA colors: one click picks one exactly.
  - **Old / New** side by side; clicking Old goes back to it. A readout shows H°, S% and V%.
  - **Dithers to**: the color a flat area of the picked color becomes in the current filter: the active pre-adjust, then the nearest color of the active dither palette (full 16, 4-color mode or custom subset), with no dither pattern.
  - **Add to swatches** saves the picked color (an undo step; the picker stays open). **OK** (Enter) keeps the color, **Cancel** (Escape, or a click outside the popover) puts the old one back.
  - The target color follows the picker **live** while it is open, so the ribbon chip and the DOS view's active color update as you drag (overlay only, no re-dither); Cancel restores the old color. A new swatch has no live target.
  - The picker is modal: editor keys (`[` `]` `X` `D`, space), File and Edit commands, layer commands, paste, undo and redo do nothing while it is open.
- Colors and swatches are stored in the document (see [File Format](file-format.md)) but are not pixel edits: changing them never re-renders or re-dithers (`doc_version` does not move). A foreground change reaches the DOS view as an overlay update of its active color. Swatch add and remove are undo steps; fg/bg changes (Picker tool, color picker, swatch click, swap, reset) are not, like GIMP.

### Undo and redo

- **Edit > Undo** (Ctrl+Z) and **Edit > Redo** (Ctrl+Shift+Z or Ctrl+Y) cover every document edit. The Edit menu sits next to File and names the step ("Undo Brush Stroke"); an entry is greyed out when there is nothing to undo or redo.
- One step each: a brush or eraser stroke (press to release), a mask stroke ("Mask Stroke"), a move / scale / rotate drag, each Layers-panel command including Duplicate, each Flip and Rasterize (an inline rename counts once, when committed), each filter change, each inserted image, Project Settings Apply, swatch add and remove.
- Runs of the same control merge into one step while nothing else is recorded in between: one filter slider (bar clicks and −/+ steps; Reset is its own step), one layer's opacity, and arrow-key nudges of one layer.
- Not steps: fg/bg colors, layer selection, tool, brush, zoom and scroll, panel collapse. Undo and redo keep the current fg/bg.
- Undo during a stroke or drag finishes it and undoes it; it cancels an inline rename. Undo and redo do nothing while a dialog is open. If the selected layer is gone afterwards, the top layer is selected; a changed canvas size refits the view.
- The history is bounded by memory, about 512 MiB; the oldest steps are dropped first and the newest one is always kept. Strokes store only the changed rectangle of the layer; mask strokes and other steps keep a copy of the document (image layers only reference their source). Paint rasters and masks are copy-on-write, so unchanged ones are shared between steps and the document, and each shared raster counts once against the budget.
- New and Open clear the history. Undoing back to the last-saved state makes the document clean again (the • goes away).

## Canvas, output, and screen resolutions

Three separate sizes are involved:

| Size | What it is | Set by |
|------|------------|--------|
| **Canvas** | The full-color working image in the native editor. | User, any size. |
| **Output** | The dithered CGA image. The dither runs at this size, PNG export uses it, and the DOS app's canvas window shows it at 1:1. | User, per project. For example, canvas ÷ 4. |
| **DOS screen** | The fake DOS app's whole screen, including its toolbars and canvas window. | Fixed: 320×200 (desktop) or 200×320 (mobile). |

**Project Settings** (File > Project Settings…, Ctrl+Shift+P) is a modal dialog that edits a draft of all three settings and applies it as one document edit (one undo step; the edit returns the document as it was). Nothing changes until Apply; Cancel or Escape discards the draft, Enter applies.

- **Layout:** Mobile (9:16, 200×320 DOS screen) or Desktop (16:9, 320×200). Switching refits the tube into the DOS window (under Hyprland the window is also resized to 9:16 / 16:9 for its monitor; on other Wayland compositors it keeps its compositor-given size; see tech-stack).
- **Canvas:** W×H fields (click a number and type, Enter or Tab sets it), −100/−10/+10/+100 steppers, presets 800×1280, 1280×800, 1600×2560, 2560×1600, and a 3×3 anchor grid (default center). Sides are clamped to 1–8192.
- **Output:** W×H fields with −10/−1/+1/+10 steppers, presets canvas ÷ 1, ÷ 2, ÷ 3, ÷ 4, ÷ 8 (rounded down, at least 1) and DOS-native (the layout's DOS screen size).
- **Output link:** while a ÷ N preset is chosen, the output follows canvas ÷ N when the canvas size changes. The link is on when the dialog opens if the output already equals canvas ÷ N for a preset N (so a new document, 800×1280 → 200×320, stays ÷ 4). Typing or stepping the output, or picking DOS-native, unlinks it. An unlinked DOS-native output follows a layout switch (200×320 ↔ 320×200).
- **Canvas resize never resamples.** All content moves by the anchor offset (0 at the anchored edge, the whole growth at the far edge, half of it rounded down in the middle). Paint layers are cropped or padded with transparency and rebased to offset 0, so pixels already off the canvas are dropped. Masks are cropped or padded with 255 (unmasked). Image layers keep their full-resolution source, scale and rotation and only shift. Locks do not block a canvas resize. The view refits afterwards.
- File > New still makes the default document (800×1280, mobile, output ÷ 4); change it with Project Settings.

Navigation:

- **Native editor:** the canvas can be scrolled and zoomed. It opens fitted to the window (and keeps refitting on resize until you pan or zoom). Zoom runs 12.5%–3200%: ctrl+wheel zooms about the cursor, ctrl+= (also ctrl++ and ctrl+shift+=) / ctrl+- step the zoom, ctrl+1 is 100%, ctrl+0 fits. The wheel scrolls; middle-drag or space+drag pans. Transparency shows as a checkerboard on a dark surround, and pixels stay sharp when zoomed in. A new document starts with one opaque white paint layer named "Background".
- **DOS app:** the output image can only be scrolled, never zoomed, because it is always shown 1:1 in DOS screen pixels. It auto-scrolls to keep the cursor's position in view, like an old paint program panning as you paint near the edge. It ignores the native editor's zoom level. The window pans when the cursor comes within 16 output pixels of an edge and never scrolls past the image; on an axis where the image is smaller than the window, the image is centered on a dark gray backdrop.

## DOS view (right)

- Looks like a complete retro DOS CGA paint app, with its own toolbars and a canvas window around the image.
- Toolbars are a Deluxe Paint-style tool strip and a CGA palette strip. They are display-only and mirror the native editor's state, highlighting the active tool and color so viewers see tool changes happen. The DOS window does not accept input.
- Before the editor's first frame, it shows the DOS screen with an empty (black) canvas in the mobile layout. After that it always shows the newest frame and keeps the last one when nothing new arrives.
- Shows a retro version of the user's cursor, mirroring what is done in the full-color view. The cursor changes with the active tool: a classic DOS arrow for select and move, a crosshair for the picker, lasso, and rectangle select, and a brush outline for painting. It is drawn in palette colors at DOS screen resolution, so it goes through the CRT shader too.

### Screen resolution and tube

| Layout | DOS screen | Tube shape |
|--------|------------|------------|
| Mobile (vertical) | 200×320 (CGA 320×200 mode turned on its side) | 9:16, filling the frame |
| Desktop | 320×200 | 4:3 |

The DOS screen is stretched to fill the tube. On desktop, the 4:3 tube gives pixels about 1.2× tall, as on real CGA hardware. On mobile, the tube is an imaginary vertical CRT that fills the whole 9:16 frame, with pixels about 1.1× tall, and there is no bezel art or anything else outside the tube. The desktop layout centers the 4:3 tube in a 16:9 frame with plain black sides, which can be cropped in OBS.

### CRT shader

The DOS view is rendered through a CRT post-process shader ported to raylib GLSL from an earlier Bevy project's `crt.wgsl` (WGSL/Bevy). The DOS screen is drawn to an offscreen texture, then shown on a fullscreen quad with:

- barrel curvature and a black bezel where the tube curves away
- scanlines matched to the DOS screen's row count
- subtle time-driven flicker
- vignette
- phosphor tint, left at white so CGA colors pass through unchanged

The GLSL source is `crates/rp-dos/shaders/crt.fs`; `crates/rp-dos/src/crt.rs` holds its parameters (original defaults: curvature 0.18, scanlines 0.35, flicker 0.04, white phosphor), a pure-Rust mirror of the shader math for tests, and `tube_rect`, which places the tube in the window. The window passes the real framebuffer size every frame, so the mobile tube always fills the actual window and the desktop tube is the largest centered 4:3 rect in it, whatever size the compositor or the user picks. One change from the original: the scanline term draws one dark gap per DOS screen row, on the boundary between rows, where the original darkened every other row. Row centers stay at full brightness, so CGA colors come through exactly. `cargo run -p rp-dos --example crt-demo -- mobile|desktop` (or `RP_DOS_DEMO=mobile|desktop` on the app) shows the sample screen through the shader.
