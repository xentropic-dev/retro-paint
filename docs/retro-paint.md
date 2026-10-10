# retro-paint

Draft.

## Overview

retro-paint is a utility for creating DOS-like images using photobashing.

Images produced by retro-paint:

- use dither filtering, and
- use only colors from the CGA color palette.

retro-paint is a tool within a tool. It has a full-color editing view and a twin DOS CGA view meant for screen capture, mainly for short-form mobile video. The DOS view looks like a complete little DOS paint app, with toolbars, shown through a CRT shader. Both mobile and desktop views are supported. See [Aesthetics](aesthetics.md).

## Photobashing

Photobashing is the core way of working in retro-paint: images are built by combining source material into a composition, which is then dithered down to the CGA palette.

Photobashing is done at higher color and resolution in the full-color view, and the result is shown in real time in the DOS view. See [Aesthetics](aesthetics.md).

## Related documents

- [Filter and Palette](filter-and-palette.md): the CGA palette and the dithering methods we might implement.
- [Aesthetics](aesthetics.md): the look of the app, the two-view layout, and screen capture.
- [File Format](file-format.md): the native project format and PNG export.
- [Tech Stack](tech-stack.md): Rust, with GPUI for the main editor and raylib for the preview screen, in one process, on Linux.
