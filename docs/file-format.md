# File Format

Draft.

## Project file

retro-paint saves work as a **`.rpaint`** file: a zip archive, in the same style as `.ora` and `.kra`.

```
project.rpaint
├── manifest.json        # format version, project settings, layer stack
├── layers/
│   ├── 0001.png         # layer pixels (lossless PNG)
│   ├── 0001-mask.png    # layer mask, grayscale, if present
│   └── ...
└── sources/
    └── <hash>.png       # inserted images at their original resolution
```

### What is stored

- **Canvas size:** any size, chosen by the user.
- **Raw original resolution:** inserted images are kept in `sources/` at their original full resolution. The layer stores a transform on top of the source, so resizing never degrades it. Paint layers are canvas-sized rasters.
- **Layers**, in stack order, each with:
  - name
  - pixel data (paint layer) or source reference plus transform (image layer)
  - position / translation; image layers also store scale and rotation
  - visibility
  - opacity (0–100%)
  - blend mode
  - layer mask (optional, grayscale)
  - lock (pixels and/or position)
- **Filter settings:** dither method, palette or subset (including 4-color preset and background color), pre-adjust values, Bayer spread, error strength, serpentine. Reopening a project gives identical CGA output.
- **Brush colors:** current foreground and background.
- **Swatches:** user-saved full-color swatches.
- **Layout and output:** mobile or desktop DOS layout, and output resolution.

### Versioning

`manifest.json` carries an integer `format_version`. On load, older versions are migrated step by step to the current one. Files with a newer version than the app understands are refused with a clear message rather than partly loaded.

### Decisions

Implemented in `crates/rp-format`.

- **Current version: 1.** Version 0 is the pre-release draft (flat `canvas_width` / `canvas_height` / `output_width` / `output_height` fields). No release wrote it; its migration step exists so the chain is tested. Each step rewrites the raw manifest JSON from version `n` to `n + 1`, and only the final result is parsed.
- **Manifest names are pinned in rp-format**, not derived from Rust types: snake_case enum names (`floyd_steinberg`, `palette1`, `light_magenta`, …), blend modes by `BlendMode::name`, colors as `#rrggbb`, sizes as `{"width", "height"}`. Palette is `{"kind": "full"}`, `{"kind": "four_color", "preset", "intensity", "background"}` or `{"kind": "custom", "colors": [...]}` in palette order. Floats are written in shortest round-trip form, so they load back bit-exact; NaN or infinity is refused at save.
- **Entries:** layers are numbered by stack position, 1-based, bottom first (`layers/0001.png`). The manifest names each layer's pixel and mask entries. Pixel PNGs are RGBA8 with straight alpha, masks are 8-bit grayscale, and PNG entries are stored uncompressed in the zip (they are already compressed). `manifest.json` is deflated.
- **Source hash:** SHA-256 of the decoded image (`width` and `height` as big-endian `u32`, then the RGBA8 bytes), as 64 lowercase hex digits (`rp_format::source_hash`). It hashes pixels, not file bytes. Save and load both check that each source's pixels match its hash. Only sources used by image layers are saved, and each is stored once.
- **Layer ids are not stored.** Loading assigns fresh ids in stack order.
- **Saving to a path is atomic:** write `<name>.tmp` next to it, fsync, then rename it over the target.
- **Errors:** a corrupt archive, missing entry, bad manifest, bad PNG, or hash mismatch each has its own typed error. Loading never panics.

## PNG export

retro-paint can export a final rasterized render to PNG.

- **Default: dithered CGA output.** Pure palette pixels, with no CRT shader effect, so every pixel follows the palette rule (see [Filter and Palette](filter-and-palette.md)).
- **Size:** the project's output resolution by default, with a 1×–8× nearest-neighbor integer upscale option. Pixels stay square, and the output still contains only palette colors.
- **Optional: flattened full-color image.** A checkbox also exports the full-color composition before dithering.

### Decisions

Implemented in `crates/rp-format` (`export_png`, `encode_dithered_png`).

- **Same render as the live view.** Export runs `Document::flatten` then `rp_palette_dither::render` (or a `Renderer`, same output) at the output resolution. There is no separate dither path.
- **Dithered file is a 4-bit indexed PNG.** Its `PLTE` holds the active palette's colors in palette order: all 16 for the full palette, 4 for a 4-color mode, or the custom subset. No `tRNS`; every pixel is opaque. The file cannot contain a non-palette color, and decoding it gives exactly the palette RGB values.
- **Scale** is an integer `1..=8`. Each output pixel becomes an exact `s×s` block, so the file is `output × s` pixels. Any other value is `FormatError::InvalidScale` and nothing is written.
- **Full-color file** is written next to the dithered one as `<stem>-full.png` (`foo.png` → `foo-full.png`). It is the flattened canvas at **canvas** resolution (not upscaled), RGBA8 with straight alpha, byte-equal to `flatten`.
- **Writes are atomic**, like project saves: temp file, fsync, rename. Both PNGs are encoded in memory before either is written.

## File menu

Saving, opening, and exporting are done from the File menu. See [Aesthetics](aesthetics.md).
