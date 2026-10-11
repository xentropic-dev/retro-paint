//! Project settings (canvas size, output size, layout), with no GPUI types.
//!
//! File > Project Settings… opens a dialog that edits a [`ProjectDraft`];
//! nothing touches the document until Apply, which runs
//! [`EditorState::apply_project`] once and returns a [`ProjectEdit`] holding
//! the document as it was (for undo).
//!
//! Canvas resize never resamples pixels. Everything moves by the anchor
//! offset ([`Anchor::offset`]): paint layers are cropped or padded with
//! transparency (and rebased to offset 0, so pixels that were already off
//! the canvas are dropped), masks are cropped or padded with 255 (unmasked,
//! matching "outside the mask is unmasked"), and image layers keep their
//! full-resolution source and scale / rotation and only shift. Locks do not
//! stop a canvas resize: it moves the canvas, not the layer.
//!
//! Output size is independent of the canvas but can be **linked** to it:
//! while linked to `÷ N`, a canvas change recomputes the output as canvas ÷ N.
//! The link is on by default when the dialog opens and the output already
//! equals canvas ÷ N for a preset N (a fresh document is 800×1280 → 200×320,
//! ÷ 4). Picking a ÷ N preset sets the link; typing or stepping a custom
//! output size, or picking DOS-native, clears it.

use rp_doc::{Document, GrayImage, LayerKind, Layout, RgbaImage, Size};

use crate::state::EditorState;

/// Smallest canvas or output side.
pub const MIN_SIZE: u32 = 1;
/// Largest canvas or output side.
pub const MAX_SIZE: u32 = 8192;

/// Canvas size presets: 4× and 8× the DOS screens, both orientations.
pub const CANVAS_PRESETS: [Size; 4] = [
    Size::new(800, 1280),
    Size::new(1280, 800),
    Size::new(1600, 2560),
    Size::new(2560, 1600),
];

/// Output presets as canvas divisors.
pub const OUTPUT_DIVISORS: [u32; 5] = [1, 2, 3, 4, 8];

/// Clamps one side to `MIN_SIZE..=MAX_SIZE`.
pub fn clamp_dim(v: i64) -> u32 {
    v.clamp(MIN_SIZE as i64, MAX_SIZE as i64) as u32
}

/// Clamps both sides to `MIN_SIZE..=MAX_SIZE`.
pub fn clamp_size(s: Size) -> Size {
    Size::new(clamp_dim(s.width as i64), clamp_dim(s.height as i64))
}

/// `canvas ÷ n`, rounded down, at least 1×1.
pub fn divided(canvas: Size, n: u32) -> Size {
    let n = n.max(1);
    clamp_size(Size::new(canvas.width / n, canvas.height / n))
}

/// The first preset divisor `n` with `canvas ÷ n == output`.
pub fn divisor_of(canvas: Size, output: Size) -> Option<u32> {
    OUTPUT_DIVISORS
        .into_iter()
        .find(|&n| divided(canvas, n) == output)
}

/// Where the old canvas sits inside the new one on resize.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// Row-major, as drawn in the 3×3 anchor grid.
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];

    /// Horizontal and vertical position: 0 start, 1 middle, 2 end.
    const fn cell(self) -> (i64, i64) {
        let i = self as i64;
        (i % 3, i / 3)
    }

    /// Arrow glyph for the anchor grid.
    pub const fn glyph(self) -> &'static str {
        match self {
            Anchor::TopLeft => "↖",
            Anchor::Top => "↑",
            Anchor::TopRight => "↗",
            Anchor::Left => "←",
            Anchor::Center => "•",
            Anchor::Right => "→",
            Anchor::BottomLeft => "↙",
            Anchor::Bottom => "↓",
            Anchor::BottomRight => "↘",
        }
    }

    /// How far old canvas content moves when the canvas goes from `old` to
    /// `new`: 0 at the start edge, the whole growth at the end edge, half of
    /// it (rounded down) in the middle. Negative when shrinking.
    pub fn offset(self, old: Size, new: Size) -> (i64, i64) {
        let (cx, cy) = self.cell();
        let along = |o: u32, n: u32, c: i64| {
            let grow = n as i64 - o as i64;
            match c {
                0 => 0,
                1 => grow.div_euclid(2),
                _ => grow,
            }
        };
        (
            along(old.width, new.width, cx),
            along(old.height, new.height, cy),
        )
    }
}

/// Copies `src` (placed at `(dx, dy)`) into a `new`-sized raster filled with
/// `fill`. `C` is the channel count.
fn shift_raw<const C: usize>(
    src: &[u8],
    src_size: Size,
    new: Size,
    dx: i64,
    dy: i64,
    fill: [u8; C],
) -> Vec<u8> {
    let mut out: Vec<u8> = fill.repeat(new.area());
    let (sw, sh) = (src_size.width as i64, src_size.height as i64);
    let (nw, nh) = (new.width as i64, new.height as i64);
    // Destination columns covered by the source.
    let x0 = dx.max(0);
    let x1 = (dx + sw).min(nw);
    if x0 >= x1 {
        return out;
    }
    let len = (x1 - x0) as usize * C;
    for y in dy.max(0)..(dy + sh).min(nh) {
        let sy = y - dy;
        let s = ((sy * sw + (x0 - dx)) as usize) * C;
        let d = ((y * nw + x0) as usize) * C;
        out[d..d + len].copy_from_slice(&src[s..s + len]);
    }
    out
}

/// `img` placed at `(dx, dy)` in a transparent `new`-sized raster.
pub fn shift_rgba(img: &RgbaImage, new: Size, dx: i64, dy: i64) -> RgbaImage {
    let raw = shift_raw(img.as_raw(), img.size(), new, dx, dy, [0u8; 4]);
    RgbaImage::from_raw(new, raw).expect("sized from new")
}

/// `mask` placed at `(dx, dy)` in a `new`-sized mask padded with 255.
pub fn shift_gray(mask: &GrayImage, new: Size, dx: i64, dy: i64) -> GrayImage {
    let raw = shift_raw(mask.as_raw(), mask.size(), new, dx, dy, [255u8]);
    GrayImage::from_raw(new, raw).expect("sized from new")
}

/// Resizes the canvas to `new` (clamped) around `anchor`. See the module
/// docs for what happens to each layer kind. Does not touch the output size.
pub fn resize_canvas(doc: &mut Document, new: Size, anchor: Anchor) {
    let new = clamp_size(new);
    let old = doc.canvas_size;
    let (dx, dy) = anchor.offset(old, new);
    for layer in doc.layers_mut() {
        match &mut layer.kind {
            LayerKind::Paint(p) => {
                let (ox, oy) = (p.offset_x as i64 + dx, p.offset_y as i64 + dy);
                p.pixels = shift_rgba(&p.pixels, new, ox, oy);
                p.offset_x = 0;
                p.offset_y = 0;
            }
            LayerKind::Image(i) => {
                i.transform.translate_x += dx as f32;
                i.transform.translate_y += dy as f32;
            }
        }
        if let Some(m) = &layer.mask {
            layer.mask = Some(shift_gray(m, new, dx, dy));
        }
    }
    doc.canvas_size = new;
}

/// The three project settings plus the resize anchor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProjectSettings {
    pub canvas: Size,
    pub anchor: Anchor,
    pub output: Size,
    pub layout: Layout,
}

impl ProjectSettings {
    /// The document's current settings (center anchor).
    pub fn of(doc: &Document) -> Self {
        Self {
            canvas: doc.canvas_size,
            anchor: Anchor::default(),
            output: doc.output_size,
            layout: doc.layout,
        }
    }

    /// Whether applying these to `doc` would change it.
    pub fn differs_from(&self, doc: &Document) -> bool {
        clamp_size(self.canvas) != doc.canvas_size
            || clamp_size(self.output) != doc.output_size
            || self.layout != doc.layout
    }
}

/// Applies `s` to `doc`. Returns whether anything changed.
pub fn apply_settings(doc: &mut Document, s: &ProjectSettings) -> bool {
    if !s.differs_from(doc) {
        return false;
    }
    if clamp_size(s.canvas) != doc.canvas_size {
        resize_canvas(doc, s.canvas, s.anchor);
    }
    doc.output_size = clamp_size(s.output);
    doc.layout = s.layout;
    true
}

/// A one-step document edit made by [`EditorState::apply_project`]: the
/// document before it, for undo.
#[derive(Clone, Debug)]
pub struct ProjectEdit {
    pub before: Document,
}

/// A size field that takes typed digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Field {
    CanvasW,
    CanvasH,
    OutputW,
    OutputH,
}

impl Field {
    /// Step sizes for the small and large stepper buttons.
    pub const fn steps(self) -> (i64, i64) {
        match self {
            Field::CanvasW | Field::CanvasH => (10, 100),
            Field::OutputW | Field::OutputH => (1, 10),
        }
    }
}

/// Digits typed into a [`Field`] so far.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FieldEdit {
    pub field: Field,
    pub text: String,
}

/// Longest number a field accepts.
const MAX_DIGITS: usize = 5;

/// The Project Settings dialog's working copy.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectDraft {
    pub settings: ProjectSettings,
    /// While `Some(n)`, the output follows canvas ÷ n.
    pub link: Option<u32>,
    pub editing: Option<FieldEdit>,
}

impl ProjectDraft {
    /// Starts from `doc`; linked if its output is canvas ÷ N for a preset N.
    pub fn from_document(doc: &Document) -> Self {
        Self {
            settings: ProjectSettings::of(doc),
            link: divisor_of(doc.canvas_size, doc.output_size),
            editing: None,
        }
    }

    pub fn value(&self, field: Field) -> u32 {
        let s = &self.settings;
        match field {
            Field::CanvasW => s.canvas.width,
            Field::CanvasH => s.canvas.height,
            Field::OutputW => s.output.width,
            Field::OutputH => s.output.height,
        }
    }

    /// Sets one side (clamped). Canvas sides keep a linked output in step;
    /// output sides clear the link.
    pub fn set_value(&mut self, field: Field, v: i64) {
        let v = clamp_dim(v);
        let mut canvas = self.settings.canvas;
        let mut output = self.settings.output;
        match field {
            Field::CanvasW => canvas.width = v,
            Field::CanvasH => canvas.height = v,
            Field::OutputW => output.width = v,
            Field::OutputH => output.height = v,
        }
        match field {
            Field::CanvasW | Field::CanvasH => self.set_canvas(canvas),
            Field::OutputW | Field::OutputH => self.set_output(output),
        }
    }

    /// Adds `delta` to a side (clamped).
    pub fn step(&mut self, field: Field, delta: i64) {
        self.set_value(field, self.value(field) as i64 + delta);
    }

    /// New canvas size (clamped); a linked output follows.
    pub fn set_canvas(&mut self, canvas: Size) {
        self.settings.canvas = clamp_size(canvas);
        if let Some(n) = self.link {
            self.settings.output = divided(self.settings.canvas, n);
        }
    }

    /// Custom output size (clamped); clears the link.
    pub fn set_output(&mut self, output: Size) {
        self.settings.output = clamp_size(output);
        self.link = None;
    }

    /// Output = canvas ÷ n, and keep it so while the canvas changes.
    pub fn set_divisor(&mut self, n: u32) {
        self.link = Some(n.max(1));
        self.settings.output = divided(self.settings.canvas, n);
    }

    /// Output = the DOS screen of the draft layout (unlinked).
    pub fn set_dos_native(&mut self) {
        self.set_output(self.settings.layout.dos_screen());
    }

    /// Whether the output is the draft layout's DOS screen size.
    pub fn is_dos_native(&self) -> bool {
        self.settings.output == self.settings.layout.dos_screen()
    }

    /// Switches layout. An unlinked output that was the old layout's DOS
    /// screen size becomes the new one's.
    pub fn set_layout(&mut self, layout: Layout) {
        if self.link.is_none() && self.is_dos_native() {
            self.settings.output = layout.dos_screen();
        }
        self.settings.layout = layout;
    }

    pub fn set_anchor(&mut self, anchor: Anchor) {
        self.settings.anchor = anchor;
    }

    /// Starts typing into `field` (commits any other field first).
    pub fn begin_edit(&mut self, field: Field) {
        if self.editing.as_ref().is_some_and(|e| e.field == field) {
            return;
        }
        self.commit_edit();
        self.editing = Some(FieldEdit {
            field,
            text: String::new(),
        });
    }

    /// Appends the digits in `text`; other characters are ignored.
    pub fn type_text(&mut self, text: &str) {
        if let Some(e) = &mut self.editing {
            for c in text.chars().filter(char::is_ascii_digit) {
                if e.text.len() < MAX_DIGITS {
                    e.text.push(c);
                }
            }
        }
    }

    pub fn backspace(&mut self) {
        if let Some(e) = &mut self.editing {
            e.text.pop();
        }
    }

    /// Ends typing, setting the field (clamped). Nothing typed keeps the
    /// old value.
    pub fn commit_edit(&mut self) {
        if let Some(e) = self.editing.take()
            && let Ok(v) = e.text.parse::<i64>()
        {
            self.set_value(e.field, v);
        }
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }
}

impl EditorState {
    /// File > Project Settings…: opens the dialog on the current settings.
    pub fn open_project_settings(&mut self) {
        self.file_menu_open = false;
        self.project_dialog = Some(ProjectDraft::from_document(&self.document));
    }

    /// Closes the dialog without changing the document.
    pub fn cancel_project_settings(&mut self) {
        self.project_dialog = None;
    }

    /// Runs `f` on the open dialog's draft.
    pub fn edit_project_draft(&mut self, f: impl FnOnce(&mut ProjectDraft)) {
        if let Some(d) = &mut self.project_dialog {
            f(d);
        }
    }

    /// Apply in the dialog: commits a field being typed, applies the draft
    /// and closes the dialog.
    pub fn apply_project_settings(&mut self) -> Option<ProjectEdit> {
        let mut draft = self.project_dialog.take()?;
        draft.commit_edit();
        self.apply_project(&draft.settings)
    }

    /// Applies project settings as one document edit. A canvas change ends
    /// any stroke or drag and refits the view. Returns the before-state, or
    /// `None` if nothing changed.
    /// The edit is recorded as one undo step.
    pub fn apply_project(&mut self, s: &ProjectSettings) -> Option<ProjectEdit> {
        self.finish_gestures();
        let content_before = self.content;
        let before = self.document.clone();
        if !apply_settings(&mut self.document, s) {
            return None;
        }
        let d = &self.document;
        if d.canvas_size != before.canvas_size {
            self.stroke = None;
            self.transform_drag = None;
            self.viewport
                .set_canvas_size(d.canvas_size.width, d.canvas_size.height);
            self.auto_fit = true;
            self.viewport.fit();
        }
        self.status = format!(
            "Canvas {}×{} → output {}×{}, {}",
            d.canvas_size.width,
            d.canvas_size.height,
            d.output_size.width,
            d.output_size.height,
            layout_label(d.layout)
        );
        self.mark_document_changed();
        self.history.push_snapshot(
            before.clone(),
            crate::undo_ops::EditLabel::ProjectSettings,
            content_before,
            self.content,
        );
        Some(ProjectEdit { before })
    }
}

/// Dialog label of a layout.
pub const fn layout_label(layout: Layout) -> &'static str {
    match layout {
        Layout::Mobile => "Mobile 9:16",
        Layout::Desktop => "Desktop 16:9",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{FileCommand, NEW_CANVAS_SIZE};
    use rp_doc::{Layer, SourceHash, Transform};

    fn sz(w: u32, h: u32) -> Size {
        Size::new(w, h)
    }

    /// A paint raster whose pixel (x, y) is `[x, y, 7, 255]`.
    fn coords(size: Size) -> RgbaImage {
        let mut img = RgbaImage::new(size);
        for y in 0..size.height {
            for x in 0..size.width {
                img.put(x, y, [x as u8, y as u8, 7, 255]);
            }
        }
        img
    }

    fn doc_with_paint(size: Size) -> Document {
        let mut doc = Document::new(size);
        let mut l = Layer::paint("P", size);
        if let LayerKind::Paint(p) = &mut l.kind {
            p.pixels = coords(size);
        }
        doc.push_layer(l);
        doc
    }

    fn paint(doc: &Document, i: usize) -> &rp_doc::PaintLayer {
        match &doc.layers()[i].kind {
            LayerKind::Paint(p) => p,
            _ => panic!("not paint"),
        }
    }

    #[test]
    fn presets_and_divisors() {
        assert_eq!(divided(sz(800, 1280), 4), sz(200, 320));
        assert_eq!(divided(sz(1280, 800), 4), sz(320, 200));
        assert_eq!(divided(sz(800, 1280), 3), sz(266, 426));
        assert_eq!(divided(sz(5, 3), 8), sz(1, 1));
        assert_eq!(divided(sz(800, 1280), 0), sz(800, 1280));
        assert_eq!(divisor_of(sz(800, 1280), sz(200, 320)), Some(4));
        assert_eq!(divisor_of(sz(800, 1280), sz(800, 1280)), Some(1));
        assert_eq!(divisor_of(sz(800, 1280), sz(201, 320)), None);
        for p in CANVAS_PRESETS {
            assert_eq!(divisor_of(p, divided(p, 8)), Some(8));
            assert!(p == sz(800, 1280) || divided(p, 4) != Layout::Mobile.dos_screen());
        }
        assert!(CANVAS_PRESETS.contains(&NEW_CANVAS_SIZE));
    }

    #[test]
    fn limits_clamp_to_1_and_8192() {
        assert_eq!(clamp_dim(0), 1);
        assert_eq!(clamp_dim(-5), 1);
        assert_eq!(clamp_dim(9000), MAX_SIZE);
        assert_eq!(clamp_size(sz(0, 10000)), sz(1, 8192));
        let mut doc = doc_with_paint(sz(4, 4));
        resize_canvas(&mut doc, sz(0, 0), Anchor::TopLeft);
        assert_eq!(doc.canvas_size, sz(1, 1));
        assert_eq!(paint(&doc, 0).pixels.get(0, 0), [0, 0, 7, 255]);
    }

    #[test]
    fn anchor_offsets() {
        let (o, n) = (sz(10, 10), sz(15, 6));
        assert_eq!(Anchor::TopLeft.offset(o, n), (0, 0));
        assert_eq!(Anchor::Center.offset(o, n), (2, -2));
        assert_eq!(Anchor::BottomRight.offset(o, n), (5, -4));
        assert_eq!(Anchor::Top.offset(o, n), (2, 0));
        assert_eq!(Anchor::Left.offset(o, n), (0, -2));
        assert_eq!(Anchor::BottomLeft.offset(o, n), (0, -4));
        // Odd shrink rounds toward the start edge.
        assert_eq!(Anchor::Center.offset(sz(10, 10), sz(7, 7)), (-2, -2));
        assert_eq!(Anchor::ALL.len(), 9);
        assert_eq!(Anchor::default(), Anchor::Center);
    }

    #[test]
    fn grow_top_left_pads_right_and_bottom_with_transparency() {
        let mut doc = doc_with_paint(sz(4, 3));
        resize_canvas(&mut doc, sz(6, 5), Anchor::TopLeft);
        let p = &paint(&doc, 0).pixels;
        assert_eq!(p.size(), sz(6, 5));
        assert_eq!(p.get(3, 2), [3, 2, 7, 255]);
        assert_eq!(p.get(4, 0), [0, 0, 0, 0]);
        assert_eq!(p.get(0, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn grow_center_pads_evenly_without_resampling() {
        let mut doc = doc_with_paint(sz(4, 4));
        resize_canvas(&mut doc, sz(8, 6), Anchor::Center);
        let p = &paint(&doc, 0).pixels;
        assert_eq!(p.get(1, 0), [0, 0, 0, 0]);
        assert_eq!(p.get(2, 1), [0, 0, 7, 255]);
        assert_eq!(p.get(5, 4), [3, 3, 7, 255]);
        assert_eq!(p.get(6, 4), [0, 0, 0, 0]);
        // Every opaque pixel is one of the originals, 1:1.
        let opaque = p.as_raw().chunks(4).filter(|c| c[3] == 255).count();
        assert_eq!(opaque, 16);
    }

    #[test]
    fn shrink_crops_around_anchor() {
        let mut doc = doc_with_paint(sz(6, 6));
        let mut br = doc.clone();
        resize_canvas(&mut doc, sz(2, 2), Anchor::Center);
        let p = &paint(&doc, 0).pixels;
        assert_eq!(p.get(0, 0), [2, 2, 7, 255]);
        assert_eq!(p.get(1, 1), [3, 3, 7, 255]);
        resize_canvas(&mut br, sz(2, 2), Anchor::BottomRight);
        assert_eq!(paint(&br, 0).pixels.get(0, 0), [4, 4, 7, 255]);
    }

    #[test]
    fn paint_offset_is_folded_in_and_reset() {
        let mut doc = doc_with_paint(sz(4, 4));
        doc.layers_mut()[0].translate_by(1, 0).unwrap();
        resize_canvas(&mut doc, sz(4, 4), Anchor::Center);
        assert_eq!(doc.canvas_size, sz(4, 4));
        let p = paint(&doc, 0);
        assert_eq!((p.offset_x, p.offset_y), (0, 0));
        assert_eq!(p.pixels.get(0, 0), [0, 0, 0, 0]);
        assert_eq!(p.pixels.get(1, 0), [0, 0, 7, 255]);
    }

    #[test]
    fn same_canvas_flattens_identically_after_resize_and_back() {
        let mut doc = doc_with_paint(sz(6, 4));
        let flat = doc.flatten(&rp_doc::SourceStore::new());
        resize_canvas(&mut doc, sz(10, 8), Anchor::BottomRight);
        resize_canvas(&mut doc, sz(6, 4), Anchor::BottomRight);
        assert_eq!(doc.flatten(&rp_doc::SourceStore::new()), flat);
    }

    #[test]
    fn masks_crop_and_pad_with_255() {
        let mut doc = doc_with_paint(sz(4, 4));
        doc.layers_mut()[0].mask = Some(GrayImage::filled(sz(4, 4), 0));
        resize_canvas(&mut doc, sz(6, 4), Anchor::Right);
        let m = doc.layers()[0].mask.as_ref().unwrap();
        assert_eq!(m.size(), sz(6, 4));
        assert_eq!(m.get(0, 0), 255);
        assert_eq!(m.get(1, 3), 255);
        assert_eq!(m.get(2, 0), 0);
        assert_eq!(m.get(5, 3), 0);
        resize_canvas(&mut doc, sz(2, 2), Anchor::TopLeft);
        let m = doc.layers()[0].mask.as_ref().unwrap();
        assert_eq!(m.as_raw(), &[255, 255, 255, 255]);
    }

    #[test]
    fn image_layers_shift_and_keep_scale_and_rotation() {
        let mut doc = Document::new(sz(100, 100));
        let t = Transform {
            translate_x: 10.5,
            translate_y: 20.0,
            scale_x: 0.5,
            scale_y: 2.0,
            rotation: 0.3,
        };
        doc.push_layer(Layer::image("I", SourceHash::new("ab"), t));
        // Locks do not stop a canvas resize.
        doc.layers_mut()[0].lock.position = true;
        resize_canvas(&mut doc, sz(120, 80), Anchor::Center);
        match &doc.layers()[0].kind {
            LayerKind::Image(i) => {
                assert_eq!(
                    i.transform,
                    Transform {
                        translate_x: 20.5,
                        translate_y: 10.0,
                        ..t
                    }
                );
                assert_eq!(i.source, SourceHash::new("ab"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn locked_paint_layer_still_resizes() {
        let mut doc = doc_with_paint(sz(4, 4));
        doc.layers_mut()[0].lock.pixels = true;
        resize_canvas(&mut doc, sz(5, 5), Anchor::TopLeft);
        assert_eq!(paint(&doc, 0).pixels.size(), sz(5, 5));
    }

    #[test]
    fn draft_links_output_to_canvas_divisor() {
        let st = EditorState::new();
        let mut d = ProjectDraft::from_document(&st.document);
        assert_eq!(d.link, Some(4));
        d.set_canvas(sz(1600, 2560));
        assert_eq!(d.settings.output, sz(400, 640));
        d.step(Field::CanvasW, -100);
        assert_eq!(d.settings.canvas, sz(1500, 2560));
        assert_eq!(d.settings.output, sz(375, 640));
        d.set_divisor(8);
        assert_eq!(d.settings.output, sz(187, 320));
        d.step(Field::OutputW, 1);
        assert_eq!(d.link, None);
        assert_eq!(d.settings.output, sz(188, 320));
        d.set_canvas(sz(800, 1280));
        assert_eq!(d.settings.output, sz(188, 320));
    }

    #[test]
    fn unlinked_on_open_when_output_is_custom() {
        let mut doc = Document::new(sz(800, 1280));
        doc.output_size = sz(123, 45);
        assert_eq!(ProjectDraft::from_document(&doc).link, None);
    }

    #[test]
    fn dos_native_follows_layout_switch() {
        let st = EditorState::new();
        let mut d = ProjectDraft::from_document(&st.document);
        d.set_dos_native();
        assert_eq!(d.link, None);
        assert_eq!(d.settings.output, sz(200, 320));
        d.set_layout(Layout::Desktop);
        assert_eq!(d.settings.output, sz(320, 200));
        assert!(d.is_dos_native());
        // A linked output does not follow the layout.
        d.set_divisor(4);
        d.set_layout(Layout::Mobile);
        assert_eq!(d.settings.output, sz(200, 320));
        d.set_layout(Layout::Desktop);
        assert_eq!(d.settings.output, sz(200, 320));
        assert_eq!(d.settings.layout, Layout::Desktop);
    }

    #[test]
    fn typed_digits_commit_clamped() {
        let mut d = ProjectDraft::from_document(&Document::new(sz(800, 1280)));
        d.begin_edit(Field::CanvasW);
        d.type_text("1a2");
        d.type_text("80");
        d.backspace();
        d.type_text("0");
        assert_eq!(d.editing.as_ref().unwrap().text, "1280");
        d.begin_edit(Field::CanvasH); // commits CanvasW
        assert_eq!(d.settings.canvas, sz(1280, 1280));
        assert_eq!(d.settings.output, sz(320, 320));
        d.type_text("999999");
        assert_eq!(d.editing.as_ref().unwrap().text, "99999");
        d.commit_edit();
        assert_eq!(d.settings.canvas.height, MAX_SIZE);
        d.begin_edit(Field::OutputW);
        d.type_text("0");
        d.commit_edit();
        assert_eq!(d.settings.output.width, 1);
        d.begin_edit(Field::OutputH);
        d.commit_edit(); // nothing typed: unchanged
        assert_eq!(d.settings.output.height, MAX_SIZE / 4);
        d.begin_edit(Field::OutputH);
        d.type_text("5");
        d.cancel_edit();
        assert_eq!(d.settings.output.height, MAX_SIZE / 4);
    }

    #[test]
    fn apply_bumps_version_and_returns_before_state() {
        let mut st = EditorState::new();
        st.set_view_size(1000.0, 700.0);
        st.wheel(crate::viewport::Vec2::new(5.0, 5.0), 0.0, 2.0, true, true);
        let before = st.document.clone();
        let v = st.doc_version;
        st.open_project_settings();
        st.edit_project_draft(|d| {
            d.set_layout(Layout::Desktop);
            d.set_canvas(sz(1280, 800));
        });
        assert_eq!(st.document, before, "draft edits leave the document alone");
        let edit = st.apply_project_settings().expect("changed");
        assert!(st.project_dialog.is_none());
        assert_eq!(edit.before, before);
        assert_ne!(st.doc_version, v);
        let d = &st.document;
        assert_eq!(d.canvas_size, sz(1280, 800));
        assert_eq!(d.output_size, sz(320, 200));
        assert_eq!(d.layout, Layout::Desktop);
        assert_eq!(paint(d, 0).pixels.size(), sz(1280, 800));
        assert!(st.auto_fit);
        assert_eq!(st.viewport.zoom(), st.viewport.fit_zoom());
        assert!(st.status.contains("Desktop"), "{}", st.status);
    }

    #[test]
    fn layout_only_change_bumps_version_and_keeps_pixels() {
        let mut st = EditorState::new();
        let v = st.doc_version;
        let mut s = ProjectSettings::of(&st.document);
        s.layout = Layout::Desktop;
        let edit = st.apply_project(&s).unwrap();
        assert_eq!(edit.before.layout, Layout::Mobile);
        assert_eq!(st.document.layout, Layout::Desktop);
        assert_eq!(st.document.layers(), edit.before.layers());
        assert_eq!(st.doc_version, v + 1);
    }

    #[test]
    fn no_change_is_not_an_edit() {
        let mut st = EditorState::new();
        let v = st.doc_version;
        st.open_project_settings();
        assert!(st.apply_project_settings().is_none());
        assert_eq!(st.doc_version, v);
        st.open_project_settings();
        st.edit_project_draft(|d| d.set_canvas(sz(5, 5)));
        st.cancel_project_settings();
        assert!(st.project_dialog.is_none());
        assert_eq!(st.document.canvas_size, NEW_CANVAS_SIZE);
        assert_eq!(st.doc_version, v);
    }

    #[test]
    fn project_settings_command_opens_dialog() {
        let mut st = EditorState::new();
        st.file_menu_open = true;
        assert_eq!(
            st.dispatch(FileCommand::ProjectSettings),
            crate::state::Outcome::Done
        );
        assert!(!st.file_menu_open);
        assert_eq!(
            st.project_dialog,
            Some(ProjectDraft::from_document(&st.document))
        );
    }
}
