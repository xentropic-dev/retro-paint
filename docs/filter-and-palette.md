# Filter and Palette

Draft. This document covers the output color palette and the dithering methods we might implement.

## Palette

CGA (Color Graphics Adapter) was a graphics card released in 1981 for the IBM PC. The standard mode uses one of two 4-color palettes, each with a low-intensity and a high-intensity mode. A hack allows use of all 16 colors.

The full 16-color palette:

| #  | Hex       | R   | G   | B   |
|----|-----------|-----|-----|-----|
| 0  | `#000000` | 0   | 0   | 0   |
| 1  | `#555555` | 85  | 85  | 85  |
| 2  | `#aaaaaa` | 170 | 170 | 170 |
| 3  | `#ffffff` | 255 | 255 | 255 |
| 4  | `#0000aa` | 0   | 0   | 170 |
| 5  | `#5555ff` | 85  | 85  | 255 |
| 6  | `#00aa00` | 0   | 170 | 0   |
| 7  | `#55ff55` | 85  | 255 | 85  |
| 8  | `#00aaaa` | 0   | 170 | 170 |
| 9  | `#55ffff` | 85  | 255 | 255 |
| 10 | `#aa0000` | 170 | 0   | 0   |
| 11 | `#ff5555` | 255 | 85  | 85  |
| 12 | `#aa00aa` | 170 | 0   | 170 |
| 13 | `#ff55ff` | 255 | 85  | 255 |
| 14 | `#aa5500` | 170 | 85  | 0   |
| 15 | `#ffff55` | 255 | 255 | 85  |

The index numbers above follow the order of the source list. They are only for reference in this document and are not hardware color numbers.

Every channel value is one of 0, 85, 170, or 255.

### Palette rule

Every pixel in an exported image must exactly match one of these 16 colors. Dithering is how the image suggests colors that are not in the palette.

## Color matching

Every method below has to answer one question for each pixel: which palette color is closest to this color? Options:

1. **Euclidean RGB distance.** `(r1-r2)² + (g1-g2)² + (b1-b2)²`. Simplest and fast, but it treats all channels as equally important to the eye.
2. **Weighted RGB distance.** Weight the channels by perceived brightness, for example `0.299`, `0.587`, `0.114` for R, G, B. Cheap and usually closer to what the eye sees.
3. **Perceptual color space (CIELAB / OKLab).** Convert both colors and measure distance there. Most accurate, but costs more to compute.
4. **Precomputed lookup table.** With only 16 target colors, we can precompute the nearest index for a quantized RGB cube, such as 32×32×32 or 64×64×64 entries. Any of the distance functions above can fill the table.

### Chroma-weighted OKLab

retro-paint measures nearest-color distance in OKLab with the chroma terms doubled: `ΔL² + 2·(Δa² + Δb²)` (`CHROMA_WEIGHT = 2` in `rp-palette-dither`). Plain Euclidean OKLab puts sRGB mid-grays (about 124–131) closer to Cyan than to DarkGray or LightGray. The CGA grays sit far apart in lightness (OKLab L 0.45 and 0.74), and Cyan (L 0.67, chroma 0.11) falls between them. That gave a cyan band in undithered gray ramps and cyan or brown specks in dithered ones.

Weighting chroma is principled for this palette. Dithering reproduces lightness well, because the four grays and the two intensities of each hue mix spatially into in-between lightness levels. A hue cast on a neutral, though, is conspicuous and cannot be averaged back out. Weight 2 is the smallest round value that keeps every color within 0.02 OKLab chroma (about one JND) of the gray axis mapping to a gray at every lightness. Weight 1.75 only reaches 0.019. The metric is still Euclidean (OKLab with `a`, `b` scaled by √2), so the LUT's triangle-inequality accuracy bound still holds.

Saturated colors keep their hue family. The weight mostly shifts low-chroma colors onto the grays and moves some lightness-vs-hue tradeoffs toward the matching hue, for example light yellow-green to Yellow and pale red to Brown. About 8% of random sRGB colors change their nearest color.

Error diffusion works in floating-point linear light: sRGB is decoded before diffusion, so a 50/50 dither of two colors averages to the right brightness. The nearest-color lookup re-encodes to sRGB to index the table.

## Dithering methods

### 1. No dither (nearest color)

Map each pixel to its nearest palette color. Smooth areas turn into flat bands, and this gives a hard posterized look. It is useful as a baseline and as an option for flat-color art.

### 2. Ordered dithering (Bayer matrix)

Add a position-dependent offset from a threshold matrix to each pixel, then pick the nearest palette color. The result is a regular, repeating crosshatch pattern. This look is strongly associated with retro graphics.

Bayer 2×2:

```
0 2
3 1
```

Bayer 4×4:

```
 0  8  2 10
12  4 14  6
 3 11  1  9
15  7 13  5
```

An 8×8 matrix follows the same recursive construction.

For a palette that is not evenly spaced, a common approach is:

```
threshold = (M[y mod n][x mod n] + 0.5) / n² - 0.5     // range about -0.5 .. 0.5
color'    = color + threshold * spread
output    = nearest_palette(color')
```

`spread` controls how strong the pattern is. It could be a user-facing parameter.

In retro-paint the offset is added to the gamma-encoded sRGB channels (`0.0..=1.0`), not to linear light: a fixed step is roughly perceptually even in sRGB, and the CGA levels (0, 85, 170, 255) are evenly spaced there. The default spread is one CGA step, `85/255`.

Pros:
- each pixel is independent, so it is fast and parallelizable
- stable: small edits don't ripple across the image, which matters for interactive editing
- tiles cleanly

Cons:
- visible grid pattern
- less accurate color reproduction than error diffusion

### 3. Other ordered or threshold variants

- **Cluster-dot / halftone matrices.** Threshold patterns that grow dots instead of scattering them, like a print halftone look.
- **Blue-noise threshold map.** Uses a precomputed blue-noise texture instead of a Bayer matrix. It has no visible grid, keeps the independence of ordered dithering, and looks more organic.
- **Random (white-noise) dither.** Adds random noise before quantizing. Simple but grainy. It is mostly useful for comparison.

### 4. Error diffusion

Quantize each pixel, then spread the difference between the original and chosen color (the error) onto neighbors that have not been processed yet. This gives the most accurate color reproduction, and the patterns look organic rather than gridded.

In the kernels below, `X` marks the current pixel. Weights are divided by the listed divisor.

**Floyd–Steinberg** (÷16):
```
    X  7
 3  5  1
```

**Jarvis–Judice–Ninke** (÷48):
```
       X  7  5
 3  5  7  5  3
 1  3  5  3  1
```

**Stucki** (÷42):
```
       X  8  4
 2  4  8  4  2
 1  2  4  2  1
```

**Burkes** (÷32):
```
       X  8  4
 2  4  8  4  2
```

**Sierra** (÷32):
```
       X  5  3
 2  4  5  4  2
    2  3  2
```

**Two-Row Sierra** (÷16):
```
       X  4  3
 1  2  3  2  1
```

**Sierra Lite** (÷4):
```
    X  2
 1  1
```

**Atkinson** (÷8, only 6/8 of the error is spread):
```
    X  1  1
 1  1  1
    1
```
Because Atkinson throws away part of the error, it gives higher contrast and cleaner highlights and shadows. With a small palette, that can look crisper.

Error diffusion options:
- **Serpentine scanning.** Alternate left-to-right and right-to-left on each row to reduce directional streaking.
- **Error clamping or strength.** Scale the diffused error (for example, 0–100%) to tone down noise.

In retro-paint each pixel plus its received error is clamped to `-1.0..=2.0` in linear light before quantizing. Clamping at the gamut (`0..=1`) would bias the average, because OKLab nearest-color splits are not at linear midpoints; the wider clamp still bounds the error when a palette subset cannot reach the input color. Taps outside the image are dropped.

Pros:
- best color accuracy for a 16-color palette

Cons:
- sequential, so harder to parallelize
- a small local edit can change pixels far away, which may be distracting during interactive editing

### 5. Hybrid or palette-subset approaches

- **Restrict to a subset of the 16 colors.** For example, let the user choose which colors are allowed, such as one of the 4-color modes. The same dithering code works on any subset.
- **Per-layer or per-region dither settings.** Different methods or strengths for different layers or regions, before flattening.

## Pipeline placement

1. Flatten the layers into the full-color image.
2. Downscale from canvas to output resolution using area averaging (box filter), so detail blends into colors the dither can reproduce.
3. Pre-adjust: brightness, contrast, saturation. Pushing colors toward the palette before dithering often improves results.
4. Apply the dither filter with the chosen method, palette (subset), and color-matching function.
5. Output an image that contains only palette colors.

In `rp-palette-dither` this is `render(rgba, canvas_size, output_size, &DitherSettings) -> IndexedImage`, the single entry point for the live view and export. The render worker holds a `Renderer`, which runs the same function but caches the `NearestLut` and rebuilds it only when the palette changes. Details per step:

- **Transparency.** `flatten` returns straight-alpha RGBA on a transparent canvas. Each pixel is composited over **black** in linear light (`linear(rgb) · alpha`) before downscaling, so transparent areas render as black, the CGA border color, and a half-transparent edge averages toward black. New documents start with an opaque white Background layer, so this only shows when the user hides or deletes it.
- **Box downscale.** Area average in linear light. Output pixel `i` covers source `[i·src/dst, (i+1)·src/dst)` on each axis, and each source pixel is weighted by the fraction of it inside that span, so non-integer ratios are handled exactly. It is separable (a horizontal pass, then a vertical one). Equal canvas and output sizes are the identity.
- **Pre-adjust.** Brightness, contrast and saturation are each `-1.0..=1.0` (the UI's −100..100), neutral `0`, applied in gamma-encoded sRGB in that order and clamped to the gamut: brightness `c + b`; contrast `(c − 0.5)·(1 + k) + 0.5`; saturation `Y + (c − Y)·(1 + s)` with `Y` the Rec. 709 luma of the sRGB values. sRGB because a slider step then looks even across the tone range and the CGA levels are evenly spaced there. All-neutral settings skip the step, so it is exactly the identity.
- **Measured cost.** An 800×1280 canvas to 200×320 output takes about 8 ms in a release build with Floyd–Steinberg (about 14 ms with non-neutral pre-adjust), plus about 10 ms once whenever the palette changes.

### User controls (v1)

- Dither method (none, Bayer 4×4, Floyd–Steinberg, Atkinson)
- Palette: full 16, a 4-color preset with its background color, or a custom subset
- Pre-adjust: brightness, contrast, saturation
- Bayer spread
- Error-diffusion strength (0–100%)
- Serpentine scanning (on by default)

### Global vs per-layer settings

v1 has one global set of dither settings, applied once after flattening.

**After v1 (not built):** per-layer dither settings. Each layer can override the method, strength, and palette subset, and is dithered before compositing. The UI will let the user choose either mode: global (flatten, then dither) or per-layer. One example use is keeping a flat-color logo layer undithered over a dithered photo background.

## Decisions

- **First dithering methods:** none (nearest color), Bayer 4×4, Floyd–Steinberg, and Atkinson. The others in this document are candidates for later.
- **Default color distance:** chroma-weighted OKLab (see "Chroma-weighted OKLab"), baked into a 64×64×64 lookup table at startup, so each pixel costs one table read.
- **Live and export:** the dither is both. The DOS view re-dithers the full frame live on the CPU, and export uses the same code. With error diffusion, edits can shift patterns elsewhere in the image. This is accepted as part of the look.
- **Transparency before dithering:** composite over black in linear light, then downscale (see "Pipeline placement").
- **Pre-adjust space:** gamma-encoded sRGB, controls in `-1.0..=1.0` with `0` neutral.
- **Palette subsets:** supported. The full 16 colors is the default. The authentic CGA 4-color modes are presets, and the user can also pick any custom subset.

### 4-color mode presets

In each mode, color 0 is a background color the user can choose from all 16 (black by default).

| Preset | Colors 1–3 (low intensity) | Colors 1–3 (high intensity) |
|--------|-----------------------------|------------------------------|
| Palette 0 | green, red, brown (`#00aa00` `#aa0000` `#aa5500`) | light green, light red, yellow (`#55ff55` `#ff5555` `#ffff55`) |
| Palette 1 | cyan, magenta, light gray (`#00aaaa` `#aa00aa` `#aaaaaa`) | light cyan, light magenta, white (`#55ffff` `#ff55ff` `#ffffff`) |
| Mode 5 | cyan, red, light gray (`#00aaaa` `#aa0000` `#aaaaaa`) | light cyan, light red, white (`#55ffff` `#ff5555` `#ffffff`) |

## Resolution

The dither runs at the project's output resolution, which is separate from both the canvas size and the fixed DOS screen. See [Aesthetics](aesthetics.md#canvas-output-and-screen-resolutions).

## Output

Output file formats are covered in [File Format](file-format.md).
