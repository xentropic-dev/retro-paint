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

## PNG export

retro-paint can export a final rasterized render to PNG.

- **Default: dithered CGA output.** Pure palette pixels, with no CRT shader effect, so every pixel follows the palette rule (see [Filter and Palette](filter-and-palette.md)).
- **Size:** the project's output resolution by default, with a 1×–8× nearest-neighbor integer upscale option. Pixels stay square, and the output still contains only palette colors.
- **Optional: flattened full-color image.** A checkbox also exports the full-color composition before dithering.

## File menu

Saving, opening, and exporting are done from the File menu. See [Aesthetics](aesthetics.md).
