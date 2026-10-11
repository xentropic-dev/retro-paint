//! The brush library: every brush the Brushes section shows, which one is
//! selected, and the paths the user added. Pure; the file work runs off the
//! UI thread in [`crate::brush_loader`] and arrives here as [`LoadEvent`]s.
//!
//! Decisions:
//! - Order: the built-in round Hard and Soft first, then each brush file in
//!   the order it finished indexing (a folder's files sorted by path), each
//!   file's brushes in file order.
//! - Dedupe: a brush file (by canonical path) is loaded once; adding it
//!   again, or a folder that holds it, adds nothing. Added roots (files or
//!   folders, as the user picked them) are deduped too and persisted by
//!   [`crate::brush_config`].
//! - Entries appear as soon as a file is indexed; thumbnails fill in as the
//!   loader makes them. Selecting a sampled brush decodes it lazily, off the
//!   UI thread, into a working base (see [`crate::paint_ops::working_base`]);
//!   the last few working bases are cached so switching back is instant.
//! - The selection is editor state: not in the document, not an undo step.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rp_brush::{AbrEntry, Brush, Mask, thumbnail};

use crate::paint_ops::{BrushTip, default_sampled_size};
use crate::state::EditorState;

/// Longest side of a library thumbnail, in pixels (shown at half that in
/// logical pixels, so it stays sharp on HiDPI).
pub const BRUSH_THUMB_SIDE: u32 = 64;
/// Brush file extensions the library loads (any case). Zips are skipped.
pub const BRUSH_EXTENSIONS: [&str; 4] = ["abr", "gbr", "gih", "vbr"];
/// How many decoded working bases are kept for quick reselection.
const PREPARED_CACHE: usize = 8;

/// Identifies one library brush.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum BrushKey {
    Builtin(BrushTip),
    /// Brush `index` (file order) of the canonical brush file `file`.
    File {
        file: PathBuf,
        index: usize,
    },
}

/// Where a library brush's pixels come from.
#[derive(Clone, Debug)]
pub enum BrushData {
    Builtin(BrushTip),
    /// A small brush (`.gbr`, `.vbr`, `.gih`) kept decoded.
    Decoded(Arc<Brush>),
    /// An `.abr` sample, decoded on demand from `file[entry.data]`.
    Abr {
        file: PathBuf,
        entry: Arc<AbrEntry>,
    },
}

/// One brush as the indexer found it, before any pixels are decoded.
#[derive(Clone, Debug)]
pub struct IndexedBrush {
    pub name: String,
    /// Native size in pixels (for parametric brushes, the diameter).
    pub width: u32,
    pub height: u32,
    pub data: BrushData,
}

impl IndexedBrush {
    /// Native longest side, the size a sampled brush starts at.
    pub fn long_side(&self) -> u32 {
        self.width.max(self.height).max(1)
    }

    /// A built-in round brush as a library entry.
    pub fn builtin(tip: BrushTip) -> Self {
        Self {
            name: tip.name().into(),
            width: BRUSH_THUMB_SIDE,
            height: BRUSH_THUMB_SIDE,
            data: BrushData::Builtin(tip),
        }
    }
}

/// One brush in the library.
#[derive(Clone, Debug)]
pub struct LibraryEntry {
    pub key: BrushKey,
    pub brush: IndexedBrush,
    /// Coverage thumbnail, at most [`BRUSH_THUMB_SIDE`] px; `None` until the
    /// loader has made it.
    pub thumb: Option<Arc<Mask>>,
}

/// What the off-thread loader reports, in order, for one batch of roots.
#[derive(Clone, Debug)]
pub enum LoadEvent {
    /// A brush file was indexed: its brushes, in file order.
    Indexed {
        file: PathBuf,
        brushes: Vec<IndexedBrush>,
    },
    /// Thumbnails for brushes of an indexed file, by index.
    Thumbs {
        file: PathBuf,
        thumbs: Vec<(usize, Mask)>,
    },
    /// A brush file could not be read or parsed.
    Failed { file: PathBuf, error: String },
    /// The batch is done. `missing` are roots that no longer exist.
    Finished {
        files: usize,
        brushes: usize,
        missing: Vec<PathBuf>,
    },
}

/// A sampled brush to decode and prepare off the UI thread.
#[derive(Clone, Debug)]
pub struct PrepareJob {
    pub key: BrushKey,
    pub data: BrushData,
}

/// Roots to scan and load off the UI thread.
#[derive(Clone, Debug)]
pub struct AddJob {
    /// The new roots, to scan.
    pub roots: Vec<PathBuf>,
    /// Every root, to persist.
    pub save: Vec<PathBuf>,
    /// Brush files already loaded (skipped by the scan).
    pub known: Vec<PathBuf>,
}

/// The library model; see the module docs.
#[derive(Clone, Debug)]
pub struct BrushLibrary {
    entries: Vec<LibraryEntry>,
    /// Canonical brush files that were indexed, in order.
    files: Vec<PathBuf>,
    /// Paths the user added (files or folders), persisted.
    roots: Vec<PathBuf>,
    selected: BrushKey,
    /// The sampled brush being decoded for selection.
    pending: Option<BrushKey>,
    /// Recently prepared working bases, most recent last.
    prepared: Vec<(BrushKey, Arc<Brush>)>,
    /// Bumps whenever entries or thumbnails change (view caches key on it).
    version: u64,
    /// Notes (failed files, dropped saved paths) repeated in the next
    /// "Loaded …" status, so they are not lost behind it.
    notes: Vec<String>,
}

impl Default for BrushLibrary {
    fn default() -> Self {
        Self::new()
    }
}

/// A coverage thumbnail of a built-in tip.
fn builtin_thumb(tip: BrushTip) -> Mask {
    thumbnail(&tip.make(BRUSH_THUMB_SIDE).mask, BRUSH_THUMB_SIDE)
}

impl BrushLibrary {
    /// The built-in round Hard and Soft brushes, Hard selected.
    pub fn new() -> Self {
        let entries = [BrushTip::Hard, BrushTip::Soft]
            .into_iter()
            .map(|tip| LibraryEntry {
                key: BrushKey::Builtin(tip),
                brush: IndexedBrush::builtin(tip),
                thumb: Some(Arc::new(builtin_thumb(tip))),
            })
            .collect();
        Self {
            entries,
            files: Vec::new(),
            roots: Vec::new(),
            selected: BrushKey::Builtin(BrushTip::Hard),
            pending: None,
            prepared: Vec::new(),
            version: 0,
            notes: Vec::new(),
        }
    }

    pub fn entries(&self) -> &[LibraryEntry] {
        &self.entries
    }

    pub fn entry(&self, key: &BrushKey) -> Option<&LibraryEntry> {
        self.entries.iter().find(|e| &e.key == key)
    }

    pub fn selected(&self) -> &BrushKey {
        &self.selected
    }

    pub fn select(&mut self, key: BrushKey) {
        self.pending = None;
        self.selected = key;
    }

    /// The sampled brush being decoded, if any.
    pub fn pending(&self) -> Option<&BrushKey> {
        self.pending.as_ref()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// The added roots, in the order they were added.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Indexed brush files, in library order.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Records `paths` as roots, skipping ones already there. Returns the
    /// newly added roots.
    pub fn add_roots(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
        let mut added = Vec::new();
        for p in paths {
            if !self.roots.contains(&p) {
                self.roots.push(p.clone());
                added.push(p);
            }
        }
        added
    }

    /// Applies one loader event. Returns a status message, if any.
    pub fn apply(&mut self, event: LoadEvent) -> Option<String> {
        match event {
            LoadEvent::Indexed { file, brushes } => {
                if self.files.contains(&file) {
                    return None;
                }
                self.entries
                    .extend(
                        brushes
                            .into_iter()
                            .enumerate()
                            .map(|(index, brush)| LibraryEntry {
                                key: BrushKey::File {
                                    file: file.clone(),
                                    index,
                                },
                                brush,
                                thumb: None,
                            }),
                    );
                self.files.push(file);
                self.version += 1;
                None
            }
            LoadEvent::Thumbs { file, thumbs } => {
                for (index, mask) in thumbs {
                    let key = BrushKey::File {
                        file: file.clone(),
                        index,
                    };
                    if let Some(e) = self.entries.iter_mut().find(|e| e.key == key) {
                        e.thumb = Some(Arc::new(mask));
                    }
                }
                self.version += 1;
                None
            }
            LoadEvent::Failed { file, error } => {
                let msg = format!("can't load {}: {error}", display_file(&file));
                self.notes.push(msg.clone());
                Some(format!("Brushes: {msg}"))
            }
            LoadEvent::Finished {
                files,
                brushes,
                missing,
            } => {
                if !missing.is_empty() {
                    self.roots.retain(|r| !missing.contains(r));
                    self.notes.push(format!(
                        "dropped {} missing path{}",
                        missing.len(),
                        plural(missing.len())
                    ));
                }
                let mut msg = format!(
                    "Loaded {brushes} brush{} from {files} file{}",
                    if brushes == 1 { "" } else { "es" },
                    plural(files)
                );
                for n in self.notes.drain(..) {
                    msg.push_str("; ");
                    msg.push_str(&n);
                }
                Some(msg)
            }
        }
    }

    /// A cached working base for `key`, if it was prepared recently.
    pub fn prepared(&self, key: &BrushKey) -> Option<Arc<Brush>> {
        self.prepared
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, b)| b.clone())
    }

    fn remember_prepared(&mut self, key: BrushKey, brush: Arc<Brush>) {
        self.prepared.retain(|(k, _)| k != &key);
        self.prepared.push((key, brush));
        if self.prepared.len() > PREPARED_CACHE {
            self.prepared.remove(0);
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn display_file(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

impl EditorState {
    /// Brushes section click: selects a library brush. Built-in tips apply
    /// at once; a cached sampled brush too. Otherwise the brush is marked
    /// pending and the returned job decodes it off the UI thread (finish
    /// with [`EditorState::finish_brush_prepare`]).
    pub fn select_library_brush(&mut self, key: &BrushKey) -> Option<PrepareJob> {
        let entry = self.brushes.entry(key)?;
        let data = entry.brush.data.clone();
        if let BrushData::Builtin(tip) = data {
            self.select_round_brush(tip);
            return None;
        }
        if let Some(base) = self.brushes.prepared(key) {
            self.apply_sampled(key.clone(), base);
            return None;
        }
        self.status = format!("Loading brush {}…", entry.brush.name);
        self.brushes.pending = Some(key.clone());
        Some(PrepareJob {
            key: key.clone(),
            data,
        })
    }

    /// A prepared working base arrived. Applies it if `key` is still the
    /// brush waiting for selection; an error goes to the status bar.
    pub fn finish_brush_prepare(&mut self, key: BrushKey, result: Result<Brush, String>) {
        if self.brushes.pending.as_ref() != Some(&key) {
            return;
        }
        self.brushes.pending = None;
        match result {
            Ok(base) => self.apply_sampled(key, Arc::new(base)),
            Err(e) => {
                let name = self
                    .brushes
                    .entry(&key)
                    .map(|e| e.brush.name.clone())
                    .unwrap_or_default();
                self.status = format!("Can't load brush {name}: {e}");
            }
        }
    }

    /// Paints with a sampled brush at its default size: native longest
    /// side, clamped to the canvas and to 1–1024.
    fn apply_sampled(&mut self, key: BrushKey, base: Arc<Brush>) {
        let Some(entry) = self.brushes.entry(&key) else {
            return;
        };
        let name = entry.brush.name.clone();
        let size = default_sampled_size(entry.brush.long_side(), self.document.canvas_size);
        self.brush.set_sampled(name.clone(), (*base).clone(), size);
        self.brushes.remember_prepared(key.clone(), base);
        self.brushes.select(key);
        self.status = format!("Brush {name}, {size} px");
    }

    /// Adds brush files or folders (Add brushes…, or a drop). Returns the
    /// job to scan, load and persist them off the UI thread, or `None`
    /// when nothing new was added.
    pub fn add_brush_roots(&mut self, paths: Vec<PathBuf>) -> Option<AddJob> {
        let roots = self.brushes.add_roots(paths);
        if roots.is_empty() {
            self.status = "Those brushes are already in the library".into();
            return None;
        }
        self.status = format!(
            "Loading brushes from {} path{}…",
            roots.len(),
            plural(roots.len())
        );
        Some(AddJob {
            roots,
            save: self.brushes.roots().to_vec(),
            known: self.brushes.files().to_vec(),
        })
    }

    /// Startup: the saved roots that still exist, and the note about the
    /// ones that were dropped (shown now and repeated in the "Loaded …"
    /// status). Returns the load job (nothing to save: the config file is
    /// already up to date).
    pub fn restore_brush_roots(
        &mut self,
        kept: Vec<PathBuf>,
        note: Option<String>,
    ) -> Option<AddJob> {
        let job = if kept.is_empty() {
            None
        } else {
            self.add_brush_roots(kept)
        };
        if let Some(note) = note {
            self.status = note.clone();
            if job.is_some() {
                self.brushes.notes.push(note);
            }
        }
        job
    }

    /// Applies one event from the off-thread brush loader.
    pub fn brush_load_event(&mut self, event: LoadEvent) {
        if let Some(msg) = self.brushes.apply(event) {
            self.status = msg;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_doc::Size;

    fn abr_brush(name: &str, w: u32, h: u32) -> IndexedBrush {
        IndexedBrush {
            name: name.into(),
            width: w,
            height: h,
            data: BrushData::Decoded(Arc::new(Brush::bitmap(
                name,
                25.0,
                Mask::new(w, h, vec![255; (w * h) as usize]),
                None,
            ))),
        }
    }

    fn indexed(file: &str, names: &[&str]) -> LoadEvent {
        LoadEvent::Indexed {
            file: PathBuf::from(file),
            brushes: names.iter().map(|n| abr_brush(n, 8, 4)).collect(),
        }
    }

    fn names(lib: &BrushLibrary) -> Vec<String> {
        lib.entries().iter().map(|e| e.brush.name.clone()).collect()
    }

    #[test]
    fn builtins_come_first_with_thumbnails_and_hard_selected() {
        let lib = BrushLibrary::new();
        assert_eq!(names(&lib), ["Round Hard", "Round Soft"]);
        assert!(lib.entries().iter().all(|e| e.thumb.is_some()));
        assert_eq!(lib.selected(), &BrushKey::Builtin(BrushTip::Hard));
        let t = lib.entries()[0].thumb.as_ref().unwrap();
        assert!(t.width <= BRUSH_THUMB_SIDE && t.height <= BRUSH_THUMB_SIDE);
    }

    #[test]
    fn files_append_in_order_and_dedupe() {
        let mut lib = BrushLibrary::new();
        let v = lib.version();
        lib.apply(indexed("/b/one.abr", &["a", "b"]));
        lib.apply(indexed("/b/two.gbr", &["c"]));
        assert!(lib.version() > v);
        // The same file again (another root or a second add) adds nothing.
        lib.apply(indexed("/b/one.abr", &["a", "b"]));
        assert_eq!(names(&lib), ["Round Hard", "Round Soft", "a", "b", "c"]);
        assert_eq!(
            lib.files(),
            [PathBuf::from("/b/one.abr"), PathBuf::from("/b/two.gbr")]
        );
        assert_eq!(
            lib.entries()[3].key,
            BrushKey::File {
                file: "/b/one.abr".into(),
                index: 1
            }
        );
        // Entries are there before their thumbnails.
        assert!(lib.entries()[2].thumb.is_none());
        let v = lib.version();
        lib.apply(LoadEvent::Thumbs {
            file: "/b/one.abr".into(),
            thumbs: vec![(1, Mask::new(2, 1, vec![9, 9]))],
        });
        assert!(lib.version() > v);
        assert!(lib.entries()[2].thumb.is_none());
        assert_eq!(lib.entries()[3].thumb.as_ref().unwrap().width, 2);
    }

    #[test]
    fn roots_dedupe_and_missing_ones_are_dropped_with_a_note() {
        let mut lib = BrushLibrary::new();
        let added = lib.add_roots(["/x".into(), "/y".into(), "/x".into()]);
        assert_eq!(added, [PathBuf::from("/x"), PathBuf::from("/y")]);
        assert!(lib.add_roots(["/y".into()]).is_empty());
        let msg = lib
            .apply(LoadEvent::Finished {
                files: 1,
                brushes: 3,
                missing: vec!["/y".into()],
            })
            .unwrap();
        assert_eq!(lib.roots(), [PathBuf::from("/x")]);
        assert!(msg.contains("3 brushes from 1 file"), "{msg}");
        assert!(msg.contains("dropped 1 missing path"), "{msg}");
    }

    #[test]
    fn notes_survive_into_the_loaded_status() {
        let mut st = EditorState::new();
        let job = st.restore_brush_roots(vec!["/kept".into()], Some("Brush path /gone".into()));
        assert!(job.is_some());
        assert_eq!(st.status, "Brush path /gone");
        st.brush_load_event(LoadEvent::Failed {
            file: "/kept/bad.abr".into(),
            error: "truncated".into(),
        });
        assert!(st.status.contains("bad.abr"), "{}", st.status);
        st.brush_load_event(LoadEvent::Finished {
            files: 0,
            brushes: 0,
            missing: vec![],
        });
        assert!(
            st.status.contains("bad.abr") && st.status.contains("/gone"),
            "{}",
            st.status
        );
        // Nothing kept: just the note, no job.
        let mut st = EditorState::new();
        assert!(
            st.restore_brush_roots(vec![], Some("gone".into()))
                .is_none()
        );
        assert_eq!(st.status, "gone");
    }

    #[test]
    fn add_job_carries_new_roots_all_roots_and_known_files() {
        let mut st = EditorState::new();
        st.brushes.apply(indexed("/k/known.abr", &["a"]));
        let job = st.add_brush_roots(vec!["/r1".into()]).unwrap();
        assert_eq!(job.roots, [PathBuf::from("/r1")]);
        assert_eq!(job.known, [PathBuf::from("/k/known.abr")]);
        let job = st
            .add_brush_roots(vec!["/r1".into(), "/r2".into()])
            .unwrap();
        assert_eq!(job.roots, [PathBuf::from("/r2")]);
        assert_eq!(job.save, [PathBuf::from("/r1"), PathBuf::from("/r2")]);
        assert!(st.add_brush_roots(vec!["/r2".into()]).is_none());
        assert!(st.status.contains("already"), "{}", st.status);
    }

    #[test]
    fn selecting_a_sampled_brush_waits_for_the_decode_then_uses_native_size() {
        let mut st = EditorState::new(); // 800 x 1280 canvas
        st.brushes.apply(LoadEvent::Indexed {
            file: "/f.abr".into(),
            brushes: vec![abr_brush("big", 1500, 600), abr_brush("small", 40, 90)],
        });
        let big = BrushKey::File {
            file: "/f.abr".into(),
            index: 0,
        };
        let small = BrushKey::File {
            file: "/f.abr".into(),
            index: 1,
        };
        let job = st.select_library_brush(&big).expect("decode job");
        assert_eq!(st.brushes.pending(), Some(&big));
        assert!(st.brush.is_round(), "nothing changes until it is decoded");
        // A stale result for another brush is ignored.
        st.finish_brush_prepare(small.clone(), Ok(Brush::round_hard(3.0)));
        assert!(st.brush.is_round());
        let BrushData::Decoded(b) = job.data else {
            panic!()
        };
        st.finish_brush_prepare(
            big.clone(),
            Ok(crate::paint_ops::working_base((*b).clone())),
        );
        assert!(!st.brush.is_round());
        assert_eq!(st.brush.name(), "big");
        // Native 1500 clamped to the canvas's shorter side (800).
        assert_eq!(st.brush.size(), 800);
        assert_eq!(st.brush.brush().diameter(), 800.0);
        assert_eq!(st.brushes.selected(), &big);
        assert!(st.brushes.pending().is_none());

        // Small: native size. Then the cached big one comes back at once.
        let job = st.select_library_brush(&small).unwrap();
        let BrushData::Decoded(b) = job.data else {
            panic!()
        };
        st.finish_brush_prepare(small.clone(), Ok((*b).clone()));
        assert_eq!(st.brush.size(), 90);
        assert!(st.select_library_brush(&big).is_none(), "cached");
        assert_eq!(st.brush.size(), 800);

        // Built-ins apply at once; size is clamped back to 1–256.
        let hard = BrushKey::Builtin(BrushTip::Soft);
        assert!(st.select_library_brush(&hard).is_none());
        assert!(st.brush.is_round());
        assert_eq!(st.brush.tip(), BrushTip::Soft);
        assert_eq!(st.brush.size(), 256);
        assert_eq!(st.brushes.selected(), &hard);
    }

    #[test]
    fn decode_errors_go_to_the_status_bar() {
        let mut st = EditorState::new();
        st.brushes.apply(indexed("/e.abr", &["broken"]));
        let key = BrushKey::File {
            file: "/e.abr".into(),
            index: 0,
        };
        st.select_library_brush(&key).unwrap();
        st.finish_brush_prepare(key, Err("truncated".into()));
        assert!(st.brush.is_round());
        assert!(st.status.contains("broken") && st.status.contains("truncated"));
    }

    #[test]
    fn hard_soft_only_applies_to_round_brushes() {
        let mut st = EditorState::new();
        st.set_brush_tip(BrushTip::Soft);
        assert_eq!(st.brushes.selected(), &BrushKey::Builtin(BrushTip::Soft));
        st.brush.set_sampled("s", Brush::round_hard(9.0), 300);
        st.set_brush_tip(BrushTip::Hard);
        assert!(!st.brush.is_round());
        assert!(st.status.contains("round"), "{}", st.status);
    }

    #[test]
    fn sampled_size_range_is_1_to_1024_and_brackets_step_past_256() {
        let mut st = EditorState::new();
        let base = Brush::bitmap("b", 25.0, Mask::new(1024, 512, vec![200; 1024 * 512]), None);
        st.brush.set_sampled("b", base, 256);
        st.brush_size_up();
        assert_eq!(st.brush.size(), 300);
        assert_eq!(st.brush.brush().mask.width, 300);
        st.set_brush_size(5000);
        assert_eq!(st.brush.size(), 1024);
        st.brush_size_up();
        assert_eq!(st.brush.size(), 1024);
        st.set_brush_size(0);
        assert_eq!(st.brush.size(), 1);
        assert_eq!(
            default_sampled_size(4971, Size::new(800, 1280)),
            800,
            "clamped to the canvas"
        );
        assert_eq!(default_sampled_size(4971, Size::new(4000, 4000)), 1024);
        assert_eq!(default_sampled_size(316, Size::new(800, 1280)), 316);
    }
}
