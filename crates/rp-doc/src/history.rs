//! Undo/redo: a memory-bounded [`History`] of [`Edit`] steps.
//!
//! # Step kinds
//!
//! - **Snapshot**: a whole [`Document`]. Used for structural and settings
//!   edits: add/remove/reorder/rename layers, visibility, opacity, blend,
//!   locks, masks (v1), move/transform drags, insert image, filter (dither)
//!   changes, Project Settings, swatch add/remove. Image layers only hold a
//!   [`SourceHash`](crate::SourceHash), so they are cheap. Paint pixels
//!   and masks are copy-on-write ([`RgbaImage`] keeps them behind an `Arc`),
//!   so a snapshot shares every raster that is unchanged with the document
//!   and with other snapshots; only rasters edited later get their own copy.
//! - **Paint patch**: a layer id, a dirty rectangle in the paint raster's own
//!   coordinates, and the RGBA pixels of that rectangle. Used for brush and
//!   eraser strokes. Only the smallest rectangle that contains every changed
//!   pixel is stored (or a caller-supplied rectangle, see
//!   [`Edit::paint_rect`]).
//!
//! # Exactness: steps swap
//!
//! A step always holds the state that is **not** currently in the document.
//! Undo swaps it with the document (the whole document for a snapshot, the
//! rectangle's pixels for a patch) and moves the step to the redo stack; redo
//! swaps it back. So a patch keeps one rectangle of pixels, not a before and
//! an after copy, and undo → redo restores the document byte for byte,
//! including layer ids and the id counter.
//!
//! This relies on every document edit going through the history, in order:
//! when a step is undone, the document must be exactly as it was right after
//! that step was recorded. If a patch's layer is missing or its raster size
//! no longer matches, the history is out of sync: [`History::undo`] /
//! [`History::redo`] clear it and return `false`.
//!
//! Undo ignores [`Lock`](crate::Lock): it restores a state, it does not
//! make a new edit.
//!
//! # Using it from the editor
//!
//! ```text
//! stroke / drag start:  history.begin_paint(&doc, layer_id)   // brush, eraser
//!                       history.begin_snapshot(&doc)          // move/transform drag
//! during:               mutate doc freely (many dabs / pointer moves)
//! stroke / drag end:    history.commit(&doc)                   // one step, or none if nothing changed
//! aborted (Escape):     history.cancel()                       // after restoring the doc yourself
//!
//! one-shot edit:        history.begin_snapshot(&doc); apply edit; history.commit(&doc)
//!                       // layer ops, filter/dither changes, Project Settings,
//!                       // insert image, swatch add/remove
//! ctrl-z / ctrl-shift-z: history.undo(&mut doc) / history.redo(&mut doc)
//! New / Open:           history.clear()
//! ```
//!
//! Not undo steps: fg/bg color changes (picker, swatch click, swap, reset),
//! selection, viewport, active tool. Make them directly on the document,
//! outside `begin`/`commit`. Because a snapshot swaps the whole document,
//! undoing one would also bring back the fg/bg of that moment; the editor
//! should keep the current colors across the call (copy `fg`/`bg` before
//! `undo`/`redo` and write them back after). Paint patches never touch them.
//!
//! Inserted image pixels live in the [`SourceStore`](crate::SourceStore),
//! outside the document. Undoing an insert removes the layer but leaves the
//! source in the store, which is harmless and lets redo work.
//!
//! # Memory
//!
//! [`History::memory_used`] is the memory the undo and redo stacks keep
//! reachable: every step's own bytes (patch pixels, swatches, names, fixed
//! overhead) plus every **distinct** paint raster and mask buffer the
//! stacks hold, each counted once however many steps share it. After each
//! push, undo or redo the oldest undo steps are evicted until the total is
//! within the budget ([`DEFAULT_BUDGET`], 512 MiB, see
//! [`History::with_budget`]). The redo stack is never evicted, and the
//! newest undo step is kept even if it alone is over budget, so the last
//! edit can always be undone. The open `begin`/`commit` state is transient
//! and not counted.
//!
//! Rasters are copy-on-write, so a run of snapshots that only changed
//! settings (swatches, filter, opacity, ...) shares one copy of each raster.
//! The history keeps a refcount table keyed by buffer identity (the `Arc`
//! pointer): a buffer is charged when the first step holding it enters a
//! stack and released when the last one leaves (eviction, redo
//! invalidation, [`History::clear`], or an undo/redo swap that hands the
//! buffer back to the document). The key is safe because a buffer in the
//! table is kept alive by a step, so its address cannot be reused. A
//! buffer's charge depends only on the stacks, never on the document, so
//! the running total stays exact while the document is edited.
//!
//! Buffers a step shares with the **live document** are still charged. The
//! alternative (charge only bytes the history holds exclusively) would let
//! the charge go stale without the history noticing: when the document
//! paints a shared raster it copies it (copy-on-write) and the history's
//! old buffer silently becomes exclusive memory. Charging them keeps
//! `memory_used` an upper bound on the extra memory the history really
//! holds; the overcount is at most one copy of the document's rasters
//! (16 MiB for four 800×1280 layers). So real history memory never exceeds
//! the budget (bar the newest-step exception above).
//!
//! [`Edit::memory_bytes`] is a step's size on its own (every raster
//! counted), not its share of `memory_used`.

use std::collections::{HashMap, VecDeque};
use std::mem::size_of;

use crate::document::Document;
use crate::layer::{Layer, LayerId, LayerKind};
use crate::raster::{RgbaImage, Size};

/// Default memory budget: 512 MiB.
pub const DEFAULT_BUDGET: usize = 512 * 1024 * 1024;

/// A pixel rectangle: `x..x + width` × `y..y + height`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub const fn area(self) -> usize {
        self.width as usize * self.height as usize
    }

    /// The part of `self` inside `0..size.width` × `0..size.height`.
    pub fn clip_to(self, size: Size) -> Rect {
        let x0 = self.x.min(size.width);
        let y0 = self.y.min(size.height);
        let x1 = self.x.saturating_add(self.width).min(size.width);
        let y1 = self.y.saturating_add(self.height).min(size.height);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// The smallest rectangle containing every pixel that differs between
/// `before` and `after`, searched only inside `within` (clipped to the
/// image). `None` if nothing differs there.
///
/// # Panics
///
/// If the two images differ in size.
pub fn dirty_rect(before: &RgbaImage, after: &RgbaImage, within: Option<Rect>) -> Option<Rect> {
    assert_eq!(before.size(), after.size(), "dirty_rect: size mismatch");
    let size = before.size();
    let area = within
        .unwrap_or(Rect::new(0, 0, size.width, size.height))
        .clip_to(size);
    if area.is_empty() {
        return None;
    }
    let stride = size.width as usize * 4;
    let (a, b) = (before.as_raw(), after.as_raw());
    let (mut x0, mut x1, mut y0, mut y1) = (u32::MAX, 0u32, u32::MAX, 0u32);
    for y in area.y..area.y + area.height {
        let start = y as usize * stride + area.x as usize * 4;
        let end = start + area.width as usize * 4;
        let (ra, rb) = (&a[start..end], &b[start..end]);
        if ra == rb {
            continue;
        }
        let (pa, pb) = (ra.as_chunks::<4>().0, rb.as_chunks::<4>().0);
        let differs = |i: &usize| pa[*i] != pb[*i];
        let first = (0..pa.len()).find(differs).expect("rows differ");
        let last = (0..pa.len()).rev().find(differs).expect("rows differ");
        x0 = x0.min(area.x + first as u32);
        x1 = x1.max(area.x + last as u32 + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (y0 != u32::MAX).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// One undoable step. See the [module docs](self).
#[derive(Clone, PartialEq, Debug)]
pub struct Edit(Step);

#[derive(Clone, PartialEq, Debug)]
enum Step {
    Snapshot(Box<Document>),
    Paint(PaintPatch),
}

#[derive(Clone, PartialEq, Debug)]
struct PaintPatch {
    layer: LayerId,
    /// Size of the full paint raster when recorded; checked on apply.
    raster: Size,
    rect: Rect,
    /// `rect.area() * 4` bytes, row-major: the side not in the document.
    pixels: Vec<u8>,
}

/// What kind of step an [`Edit`] is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EditKind {
    Snapshot,
    Paint,
}

impl Edit {
    /// A snapshot step; `before` is the document as it was before the edit
    /// that has since been applied.
    pub fn snapshot(before: Document) -> Self {
        Edit(Step::Snapshot(Box::new(before)))
    }

    /// A paint step for `layer`, whose raster went from `before` to `after`
    /// (`after` is what the document holds now). Stores only the dirty
    /// rectangle. `None` if nothing changed.
    ///
    /// # Panics
    ///
    /// If `before` and `after` differ in size.
    pub fn paint(layer: LayerId, before: &RgbaImage, after: &RgbaImage) -> Option<Self> {
        let rect = dirty_rect(before, after, None)?;
        Some(Self::paint_rect(layer, before, rect))
    }

    /// A paint step for `layer` with a caller-supplied dirty rectangle
    /// (clipped to the raster). `before` is the full raster before the edit.
    /// Pixels changed outside `rect` will not be restored by undo, so the
    /// rectangle must cover the whole edit.
    pub fn paint_rect(layer: LayerId, before: &RgbaImage, rect: Rect) -> Self {
        let raster = before.size();
        let rect = rect.clip_to(raster);
        Edit(Step::Paint(PaintPatch {
            layer,
            raster,
            rect,
            pixels: crop(before, rect),
        }))
    }

    pub fn kind(&self) -> EditKind {
        match self.0 {
            Step::Snapshot(_) => EditKind::Snapshot,
            Step::Paint(_) => EditKind::Paint,
        }
    }

    /// For a paint step, its layer and dirty rectangle.
    pub fn paint_target(&self) -> Option<(LayerId, Rect)> {
        match &self.0 {
            Step::Paint(p) => Some((p.layer, p.rect)),
            Step::Snapshot(_) => None,
        }
    }

    /// Bytes this step holds on its own, counting every raster and mask
    /// it references in full, shared or not. A [`History`] charges shared
    /// rasters once (see "Memory" in the module docs), so this is an upper
    /// bound on what the step adds to [`History::memory_used`].
    pub fn memory_bytes(&self) -> usize {
        let mut bytes = self.own_bytes();
        self.for_each_buffer(|_, len| bytes += len);
        bytes
    }

    /// Bytes owned by the step itself: everything except shared rasters.
    fn own_bytes(&self) -> usize {
        size_of::<Edit>()
            + match &self.0 {
                Step::Snapshot(d) => document_own_bytes(d),
                Step::Paint(p) => p.pixels.len(),
            }
    }

    /// Calls `f(id, len)` for every copy-on-write buffer the step
    /// references (snapshot paint rasters and masks), once per reference.
    fn for_each_buffer(&self, mut f: impl FnMut(usize, usize)) {
        let Step::Snapshot(d) = &self.0 else {
            return;
        };
        for l in d.layers() {
            if let Some(m) = &l.mask {
                f(m.buffer_id(), m.as_raw().len());
            }
            if let LayerKind::Paint(p) = &l.kind {
                f(p.pixels.buffer_id(), p.pixels.as_raw().len());
            }
        }
    }

    /// Swaps the stored state with the document's. `false` if the document
    /// does not match the step (missing layer, wrong kind or raster size).
    fn swap(&mut self, doc: &mut Document) -> bool {
        match &mut self.0 {
            Step::Snapshot(d) => {
                std::mem::swap(doc, d);
                true
            }
            Step::Paint(p) => {
                let Some(LayerKind::Paint(layer)) = doc.layer_mut(p.layer).map(|l| &mut l.kind)
                else {
                    return false;
                };
                if layer.pixels.size() != p.raster {
                    return false;
                }
                let stride = p.raster.width as usize * 4;
                let row = p.rect.width as usize * 4;
                let raw = layer.pixels.as_raw_mut();
                for (i, y) in (p.rect.y..p.rect.y + p.rect.height).enumerate() {
                    let start = y as usize * stride + p.rect.x as usize * 4;
                    raw[start..start + row].swap_with_slice(&mut p.pixels[i * row..(i + 1) * row]);
                }
                true
            }
        }
    }
}

fn crop(img: &RgbaImage, rect: Rect) -> Vec<u8> {
    let stride = img.width() as usize * 4;
    let row = rect.width as usize * 4;
    let mut out = Vec::with_capacity(rect.area() * 4);
    for y in rect.y..rect.y + rect.height {
        let start = y as usize * stride + rect.x as usize * 4;
        out.extend_from_slice(&img.as_raw()[start..start + row]);
    }
    out
}

/// Approximate heap + inline bytes of a document, leaving out the
/// copy-on-write raster and mask buffers (see "Memory" in the module docs).
fn document_own_bytes(d: &Document) -> usize {
    size_of::<Document>()
        + d.swatches.len() * size_of::<rp_palette_dither::Rgb>()
        + d.layers().iter().map(layer_own_bytes).sum::<usize>()
}

fn layer_own_bytes(l: &Layer) -> usize {
    size_of::<Layer>()
        + l.name.len()
        + match &l.kind {
            LayerKind::Paint(_) => 0,
            LayerKind::Image(i) => i.source.as_str().len(),
        }
}

/// A copy-on-write buffer held by the stacks: how many step references
/// point at it, and its size.
#[derive(Clone, Copy, Debug)]
struct Held {
    refs: usize,
    bytes: usize,
}

/// An open `begin_*` awaiting [`History::commit`].
#[derive(Clone, Debug)]
enum Pending {
    Snapshot(Box<Document>),
    Paint { layer: LayerId, before: RgbaImage },
}

/// Undo and redo stacks with a memory budget. See the [module docs](self).
#[derive(Clone, Debug)]
pub struct History {
    /// Oldest at the front.
    undo: VecDeque<Edit>,
    /// Next redo at the back.
    redo: Vec<Edit>,
    budget: usize,
    /// Running total, see [`History::memory_used`].
    used: usize,
    /// Buffers referenced by the stacks, keyed by `Arc` pointer.
    held: HashMap<usize, Held>,
    pending: Option<Pending>,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// Empty, with the [`DEFAULT_BUDGET`].
    pub fn new() -> Self {
        Self::with_budget(DEFAULT_BUDGET)
    }

    /// Empty, keeping at most about `bytes` of steps (see the module docs).
    pub fn with_budget(bytes: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            budget: bytes,
            used: 0,
            held: HashMap::new(),
            pending: None,
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Changes the budget, evicting old undo steps if needed.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
        self.evict();
    }

    /// Bytes the undo and redo stacks keep reachable: each step's own bytes
    /// plus each distinct raster/mask buffer they reference, counted once,
    /// including buffers shared with the live document (see "Memory" in
    /// the module docs). The budget is checked against this.
    pub fn memory_used(&self) -> usize {
        self.used
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// The step the next undo would revert.
    pub fn peek_undo(&self) -> Option<&Edit> {
        self.undo.back()
    }

    /// The step the next redo would reapply.
    pub fn peek_redo(&self) -> Option<&Edit> {
        self.redo.last()
    }

    /// Whether a `begin_*` is open.
    pub fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    /// Drops every step and any open `begin_*`. Call on New / Open.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.used = 0;
        self.held.clear();
        self.pending = None;
    }

    /// Records a finished step: clears redo, pushes, evicts to budget.
    pub fn push(&mut self, edit: Edit) {
        for e in std::mem::take(&mut self.redo) {
            self.release(&e);
        }
        self.charge(&edit);
        self.undo.push_back(edit);
        self.evict();
    }

    /// Opens a snapshot step: remembers `doc` as it is now. If a step is
    /// already open it is committed against `doc` first.
    pub fn begin_snapshot(&mut self, doc: &Document) {
        self.commit(doc);
        self.pending = Some(Pending::Snapshot(Box::new(doc.clone())));
    }

    /// Opens a paint step on `layer`: remembers its raster as it is now.
    /// Returns `false` (and opens nothing) if `layer` is not a paint layer
    /// of `doc`. If a step is already open it is committed first.
    pub fn begin_paint(&mut self, doc: &Document, layer: LayerId) -> bool {
        self.commit(doc);
        match doc.layer(layer).map(|l| &l.kind) {
            Some(LayerKind::Paint(p)) => {
                self.pending = Some(Pending::Paint {
                    layer,
                    before: p.pixels.clone(),
                });
                true
            }
            _ => false,
        }
    }

    /// Closes the open step against the document as it is now. Records one
    /// step and returns `true` if anything changed; otherwise records
    /// nothing. Returns `false` if no step is open, or (paint) if the layer
    /// is gone or was resized meanwhile.
    pub fn commit(&mut self, doc: &Document) -> bool {
        self.commit_within(doc, None)
    }

    /// Like [`commit`](Self::commit), but a paint step only looks for
    /// changes inside `hint` (e.g. the stroke's bounding box). Changes
    /// outside it are not recorded. Ignored for snapshot steps.
    pub fn commit_within(&mut self, doc: &Document, hint: Option<Rect>) -> bool {
        let edit = match self.pending.take() {
            None => return false,
            Some(Pending::Snapshot(before)) => {
                if *before == *doc {
                    return false;
                }
                Edit(Step::Snapshot(before))
            }
            Some(Pending::Paint { layer, before }) => {
                let Some(LayerKind::Paint(p)) = doc.layer(layer).map(|l| &l.kind) else {
                    return false;
                };
                if p.pixels.size() != before.size() {
                    return false;
                }
                if p.pixels.shares_buffer(&before) {
                    // Never written since `begin_paint`: nothing changed.
                    return false;
                }
                let Some(rect) = dirty_rect(&before, &p.pixels, hint) else {
                    return false;
                };
                Edit::paint_rect(layer, &before, rect)
            }
        };
        self.push(edit);
        true
    }

    /// Drops the open step without recording it. The caller restores the
    /// document if needed.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Reverts the newest step. `false` if there is nothing to undo, or if
    /// the history was out of sync with `doc` (it is then cleared and `doc`
    /// is left unchanged).
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        self.pending = None;
        let Some(mut edit) = self.undo.pop_back() else {
            return false;
        };
        self.release(&edit);
        if !edit.swap(doc) {
            self.clear();
            return false;
        }
        // The step now holds the other side's state, maybe other buffers.
        self.charge(&edit);
        self.redo.push(edit);
        self.evict();
        true
    }

    /// Reapplies the newest undone step. Same failure rules as
    /// [`undo`](Self::undo).
    pub fn redo(&mut self, doc: &mut Document) -> bool {
        self.pending = None;
        let Some(mut edit) = self.redo.pop() else {
            return false;
        };
        self.release(&edit);
        if !edit.swap(doc) {
            self.clear();
            return false;
        }
        // The step now holds the other side's state, maybe other buffers.
        self.charge(&edit);
        self.undo.push_back(edit);
        self.evict();
        true
    }

    /// Evicts the oldest undo steps until within budget, keeping the newest.
    fn evict(&mut self) {
        while self.used > self.budget && self.undo.len() > 1 {
            let e = self.undo.pop_front().expect("len > 1");
            self.release(&e);
        }
    }

    /// Adds a step entering a stack to the running total; its buffers are
    /// charged only if no other step holds them yet.
    fn charge(&mut self, e: &Edit) {
        self.used += e.own_bytes();
        let (held, used) = (&mut self.held, &mut self.used);
        e.for_each_buffer(|id, bytes| {
            let h = held.entry(id).or_insert(Held { refs: 0, bytes });
            if h.refs == 0 {
                *used += bytes;
            }
            h.refs += 1;
        });
    }

    /// Removes a step leaving the stacks (call before it is dropped or
    /// swapped); a buffer is released with its last reference.
    fn release(&mut self, e: &Edit) {
        self.used -= e.own_bytes();
        let (held, used) = (&mut self.held, &mut self.used);
        e.for_each_buffer(|id, _| {
            let h = held.get_mut(&id).expect("released buffer was charged");
            h.refs -= 1;
            if h.refs == 0 {
                *used -= h.bytes;
                held.remove(&id);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_clip() {
        let s = Size::new(10, 5);
        assert_eq!(Rect::new(8, 3, 5, 5).clip_to(s), Rect::new(8, 3, 2, 2));
        assert!(Rect::new(20, 0, 5, 5).clip_to(s).is_empty());
        assert_eq!(
            Rect::new(0, 0, u32::MAX, u32::MAX).clip_to(s),
            Rect::new(0, 0, 10, 5)
        );
    }

    #[test]
    fn crop_takes_rows() {
        let mut img = RgbaImage::new(Size::new(3, 3));
        img.put(1, 1, [1, 2, 3, 4]);
        img.put(2, 2, [5, 6, 7, 8]);
        assert_eq!(
            crop(&img, Rect::new(1, 1, 2, 2)),
            [1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 5, 6, 7, 8]
        );
    }

    use crate::raster::GrayImage;
    use rp_palette_dither::Rgb;

    const S: Size = Size::new(32, 16);
    const RASTER: usize = 32 * 16 * 4;

    /// The accounting rebuilt from scratch: own bytes of every stacked
    /// step plus each distinct buffer once. Also checks the refcounts.
    fn recount(h: &History) -> usize {
        let mut refs: HashMap<usize, Held> = HashMap::new();
        let mut used = 0;
        for e in h.undo.iter().chain(h.redo.iter()) {
            used += e.own_bytes();
            e.for_each_buffer(|id, bytes| {
                let r = refs.entry(id).or_insert(Held { refs: 0, bytes });
                if r.refs == 0 {
                    used += bytes;
                }
                r.refs += 1;
            });
        }
        let mut want: Vec<_> = refs.iter().map(|(k, v)| (*k, v.refs, v.bytes)).collect();
        let mut got: Vec<_> = h.held.iter().map(|(k, v)| (*k, v.refs, v.bytes)).collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "refcount table out of sync");
        used
    }

    fn check(h: &History) {
        assert_eq!(h.memory_used(), recount(h));
    }

    fn doc() -> (Document, LayerId, LayerId) {
        let mut d = Document::new(S);
        let a = d.push_layer(Layer::paint("a", S));
        let b = d.push_layer(Layer::paint("b", S));
        d.layer_mut(b).unwrap().mask = Some(GrayImage::filled(S, 128));
        (d, a, b)
    }

    fn fill(d: &mut Document, id: LayerId, v: u8) {
        let px = d.layer_mut(id).unwrap().pixels_mut().unwrap();
        px.as_raw_mut().fill(v);
    }

    fn snapshot(h: &mut History, d: &mut Document, f: impl FnOnce(&mut Document)) {
        h.begin_snapshot(d);
        f(d);
        assert!(h.commit(d));
        check(h);
    }

    #[test]
    fn settings_snapshots_charge_shared_rasters_once() {
        let (mut d, _, _) = doc();
        let mut h = History::new();
        for i in 0..50u8 {
            snapshot(&mut h, &mut d, |d| d.swatches.push(Rgb::new(i, i, i)));
        }
        // Two rasters and one mask, once, however many steps hold them.
        let rasters = 2 * RASTER + S.area();
        let own: usize = h.undo.iter().map(Edit::own_bytes).sum();
        assert_eq!(h.memory_used(), own + rasters);
        assert!(
            h.undo
                .iter()
                .all(|e| e.memory_bytes() == e.own_bytes() + rasters)
        );
        assert_eq!(h.held.len(), 3);
        assert!(h.held.values().all(|v| v.refs == 50));
    }

    #[test]
    fn shared_buffer_released_only_with_its_last_holder() {
        let (mut d, a, _) = doc();
        let mut h = History::new();
        let id_a = |d: &Document| match &d.layer(a).unwrap().kind {
            LayerKind::Paint(p) => p.pixels.buffer_id(),
            _ => unreachable!(),
        };
        let r0 = id_a(&d);
        snapshot(&mut h, &mut d, |d| d.swatches.push(Rgb::new(1, 1, 1)));
        snapshot(&mut h, &mut d, |d| d.swatches.push(Rgb::new(2, 2, 2)));
        // Fill as a snapshot: the step keeps r0, the document gets a copy.
        snapshot(&mut h, &mut d, |d| fill(d, a, 9));
        let r1 = id_a(&d);
        assert_ne!(r0, r1);
        assert_eq!(h.held[&r0].refs, 3);
        assert!(!h.held.contains_key(&r1));

        // Evict the oldest step: r0 stays charged, held by the other two.
        let before = h.memory_used();
        let oldest = h.undo[0].own_bytes();
        h.set_budget(before - 1);
        assert_eq!(h.undo_len(), 2);
        assert_eq!(h.memory_used(), before - oldest);
        assert_eq!(h.held[&r0].refs, 2);
        check(&h);

        // Down to the newest step: still holds r0.
        h.set_budget(0);
        assert_eq!(h.undo_len(), 1);
        assert_eq!(h.held[&r0].refs, 1);
        check(&h);

        // Undo hands r0 back to the document; the redo step holds r1.
        assert!(h.undo(&mut d));
        assert_eq!(id_a(&d), r0);
        assert!(!h.held.contains_key(&r0));
        assert_eq!(h.held[&r1].refs, 1);
        check(&h);
        assert!(h.redo(&mut d));
        assert_eq!(id_a(&d), r1);
        assert_eq!(h.held[&r0].refs, 1);
        check(&h);

        // Undo, then a new edit drops the redo step and its r1.
        assert!(h.undo(&mut d));
        snapshot(&mut h, &mut d, |d| d.swatches.push(Rgb::new(3, 3, 3)));
        assert!(!h.held.contains_key(&r1));
        assert_eq!(h.undo_len(), 1);
        assert_eq!(h.held[&r0].refs, 1);

        h.clear();
        assert_eq!(h.memory_used(), 0);
        assert!(h.held.is_empty());
    }

    #[test]
    fn accounting_stays_exact_through_mixed_edits() {
        let (mut d, a, b) = doc();
        let mut h = History::with_budget(6 * RASTER);
        for i in 0..60u32 {
            let v = i as u8;
            match i % 6 {
                0 => snapshot(&mut h, &mut d, |d| d.swatches.push(Rgb::new(v, 0, 0))),
                1 => snapshot(&mut h, &mut d, |d| fill(d, a, v)),
                2 => {
                    h.begin_paint(&d, b);
                    let px = d.layer_mut(b).unwrap().pixels_mut().unwrap();
                    px.put(i % 32, i % 16, [v, 1, 2, 255]);
                    assert!(h.commit(&d));
                    check(&h);
                }
                3 => snapshot(&mut h, &mut d, |d| {
                    d.layer_mut(b).unwrap().mask.as_mut().unwrap().put(0, 0, v);
                }),
                4 => {
                    h.undo(&mut d);
                    check(&h);
                    h.undo(&mut d);
                    check(&h);
                    h.redo(&mut d);
                    check(&h);
                }
                _ => snapshot(&mut h, &mut d, |d| {
                    let dup = d.layer(a).unwrap().clone();
                    d.push_layer(dup);
                }),
            }
            assert!(h.memory_used() <= h.budget() || h.undo_len() == 1);
        }
        while h.undo(&mut d) {
            check(&h);
        }
        while h.redo(&mut d) {
            check(&h);
        }
    }
}
