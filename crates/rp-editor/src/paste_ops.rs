//! Edit > Paste (Ctrl+V): turn what is on the Wayland clipboard into image
//! layers through the Insert Image path ([`crate::image_ops`]).
//!
//! Decisions:
//! - The clipboard is read with GPUI's own clipboard support first
//!   (`App::read_from_clipboard`; at the pinned rev the Wayland backend
//!   returns image entries for `image/png`, `image/jpeg`, `image/webp`, …).
//!   GPUI prefers text over images when both are offered, and never offers
//!   `text/uri-list`, so when it yields no image and no file list the
//!   editor falls back to `wl-paste --list-types` and `wl-paste --type
//!   <mime>` on the background executor ([`read_with`]).
//! - Type choice ([`choose_clipboard_type`]): `image/png` first, then JPEG,
//!   WebP, then any other `image/*` except SVG; then `text/uri-list`; then
//!   plain text (which file managers fill with paths).
//! - Text pastes insert files only when every non-comment line is a
//!   `file://` URI or an absolute path to an existing file
//!   ([`parse_file_list`]); any other text is "Clipboard has no image".
//! - Pasted image bytes become a layer named [`PASTED_LAYER_NAME`]; pasted
//!   files are named after their file stems, as with Insert Image. Hashing,
//!   dedupe, fit, selection and the single undo step are the insert path's.
//! - Paste does nothing while a modal dialog is open, and like undo it
//!   finishes a stroke or drag and cancels an inline rename first.

use std::path::{Path, PathBuf};

use rp_doc::LayerId;

use crate::image_ops::{InsertError, LoadedImage};
use crate::state::EditorState;

/// Layer name for an image pasted as pixels.
pub const PASTED_LAYER_NAME: &str = "Pasted image";

/// Status when the clipboard holds neither an image nor image files.
pub const NO_IMAGE_STATUS: &str = "Clipboard has no image";

/// Clipboard contents, independent of where they were read from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClipboardContent {
    /// Encoded image bytes (PNG, JPEG, …).
    Image(Vec<u8>),
    /// File paths the platform already resolved.
    Files(Vec<PathBuf>),
    /// Text: a `text/uri-list` or plain text.
    Text(String),
    Nothing,
}

/// What a paste does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PastePlan {
    /// Decode these bytes and insert them as "Pasted image".
    Image(Vec<u8>),
    /// Insert these files, in order.
    Files(Vec<PathBuf>),
    /// Nothing usable (yet: the GPUI read falls back to `wl-paste`).
    NoImage,
}

/// Decides what to paste. `is_file` filters text paths (injected for tests).
pub fn plan_paste(content: ClipboardContent, is_file: impl Fn(&Path) -> bool) -> PastePlan {
    match content {
        ClipboardContent::Image(bytes) if !bytes.is_empty() => PastePlan::Image(bytes),
        ClipboardContent::Files(paths) if !paths.is_empty() => PastePlan::Files(paths),
        ClipboardContent::Text(text) => match parse_file_list(&text) {
            Some(paths) if paths.iter().all(|p| is_file(p)) => PastePlan::Files(paths),
            _ => PastePlan::NoImage,
        },
        _ => PastePlan::NoImage,
    }
}

/// Which clipboard type `wl-paste` should read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClipboardChoice {
    /// An image MIME type.
    Image(String),
    /// A text type that may hold file paths or URIs.
    Text(String),
}

/// Image types in order of preference; other `image/*` types follow.
const PREFERRED_IMAGE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];
/// Image types that never decode.
const SKIPPED_IMAGE_TYPES: [&str; 1] = ["image/svg+xml"];
/// Text types that may hold file paths, in order of preference.
const TEXT_TYPES: [&str; 6] = [
    "text/uri-list",
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

/// Picks the best type from `wl-paste --list-types` output.
pub fn choose_clipboard_type(types: &[&str]) -> Option<ClipboardChoice> {
    let has = |t: &str| types.iter().any(|&o| o.eq_ignore_ascii_case(t));
    if let Some(t) = PREFERRED_IMAGE_TYPES.iter().find(|t| has(t)) {
        return Some(ClipboardChoice::Image((*t).to_string()));
    }
    if let Some(t) = types.iter().find(|t| {
        let l = t.to_ascii_lowercase();
        l.starts_with("image/") && !SKIPPED_IMAGE_TYPES.contains(&l.as_str())
    }) {
        return Some(ClipboardChoice::Image((*t).to_string()));
    }
    TEXT_TYPES
        .iter()
        .find(|t| has(t))
        .map(|t| ClipboardChoice::Text((*t).to_string()))
}

/// Reads the clipboard through `run`, which runs `wl-paste` with the given
/// arguments and returns its stdout (or `None` on failure). Blocking.
pub fn read_with(mut run: impl FnMut(&[&str]) -> Option<Vec<u8>>) -> ClipboardContent {
    let Some(listed) = run(&["--list-types"]) else {
        return ClipboardContent::Nothing;
    };
    let listed = String::from_utf8_lossy(&listed);
    let types: Vec<&str> = listed
        .lines()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    match choose_clipboard_type(&types) {
        Some(ClipboardChoice::Image(mime)) => run(&["--no-newline", "--type", &mime])
            .filter(|b| !b.is_empty())
            .map_or(ClipboardContent::Nothing, ClipboardContent::Image),
        Some(ClipboardChoice::Text(mime)) => run(&["--no-newline", "--type", &mime])
            .map_or(ClipboardContent::Nothing, |b| {
                ClipboardContent::Text(String::from_utf8_lossy(&b).into_owned())
            }),
        None => ClipboardContent::Nothing,
    }
}

/// Runs `wl-paste` with `args`; its stdout, or `None` if it is missing or
/// fails (an empty clipboard exits non-zero). Blocking.
pub fn run_wl_paste(args: &[&str]) -> Option<Vec<u8>> {
    let out = std::process::Command::new("wl-paste")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// Parses a `text/uri-list` (or a plain list of absolute paths, one per
/// line) into local paths. `None` unless every non-comment line is a local
/// `file://` URI or an absolute path. A leading `copy` / `cut` line
/// (GNOME's copied-files format) is skipped.
pub fn parse_file_list(text: &str) -> Option<Vec<PathBuf>> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .peekable();
    if lines.peek().is_some_and(|&l| l == "copy" || l == "cut") {
        lines.next();
    }
    let paths = lines
        .map(|l| {
            if let Some(rest) = l.strip_prefix("file://") {
                // Host must be empty or localhost.
                let path = rest.strip_prefix("localhost").unwrap_or(rest);
                path.starts_with('/')
                    .then(|| percent_decode(path))
                    .flatten()
                    .map(PathBuf::from)
            } else if l.starts_with('/') {
                Some(PathBuf::from(l))
            } else {
                None
            }
        })
        .collect::<Option<Vec<_>>>()?;
    (!paths.is_empty()).then_some(paths)
}

/// Decodes `%XX` escapes; `None` for a malformed escape or non-UTF-8 result.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl EditorState {
    /// Starts a paste. Returns `false` (and does nothing) while a modal
    /// dialog is open. Closes the menus, finishes gestures, and shows
    /// `Pasting…`.
    pub fn begin_paste(&mut self) -> bool {
        self.close_menus();
        if self.file_modal_open() || self.project_dialog.is_some() {
            return false;
        }
        self.finish_gestures();
        self.status = "Pasting…".into();
        true
    }

    /// The clipboard had nothing to paste.
    pub fn paste_found_nothing(&mut self) {
        self.status = NO_IMAGE_STATUS.into();
    }

    /// Applies a background decode of pasted image bytes: inserts it as a
    /// new image layer (one undo step) or reports why it failed.
    pub fn finish_paste(&mut self, result: Result<LoadedImage, InsertError>) -> Option<LayerId> {
        match result {
            Ok(loaded) => Some(self.insert_loaded_image(loaded)),
            Err(e) => {
                self.status = format!("Can't paste: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_ops::load_image_bytes;
    use crate::state::FileCommand;
    use crate::undo_ops::EditLabel;
    use image::{DynamicImage, ImageFormat};
    use rp_doc::{LayerKind, Size};
    use std::cell::RefCell;
    use std::io::Cursor;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x * 30) as u8, (y * 30) as u8, 90, 255])
        });
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn any_file(_: &Path) -> bool {
        true
    }

    // ---- type decision -----------------------------------------------------

    #[test]
    fn png_wins_over_everything() {
        let types = [
            "text/plain;charset=utf-8",
            "text/uri-list",
            "image/jpeg",
            "image/png",
        ];
        assert_eq!(
            choose_clipboard_type(&types),
            Some(ClipboardChoice::Image("image/png".into()))
        );
    }

    #[test]
    fn jpeg_then_webp_then_other_images() {
        assert_eq!(
            choose_clipboard_type(&["image/webp", "image/jpeg"]),
            Some(ClipboardChoice::Image("image/jpeg".into()))
        );
        assert_eq!(
            choose_clipboard_type(&["image/bmp", "image/webp"]),
            Some(ClipboardChoice::Image("image/webp".into()))
        );
        assert_eq!(
            choose_clipboard_type(&["text/html", "image/bmp"]),
            Some(ClipboardChoice::Image("image/bmp".into()))
        );
    }

    #[test]
    fn svg_is_not_an_image_choice() {
        assert_eq!(
            choose_clipboard_type(&["image/svg+xml", "text/plain"]),
            Some(ClipboardChoice::Text("text/plain".into()))
        );
        assert_eq!(choose_clipboard_type(&["image/svg+xml"]), None);
    }

    #[test]
    fn uri_list_beats_plain_text() {
        let types = [
            "x-special/gnome-copied-files",
            "UTF8_STRING",
            "text/uri-list",
        ];
        assert_eq!(
            choose_clipboard_type(&types),
            Some(ClipboardChoice::Text("text/uri-list".into()))
        );
    }

    #[test]
    fn nothing_usable() {
        assert_eq!(choose_clipboard_type(&[]), None);
        assert_eq!(
            choose_clipboard_type(&["text/html", "application/x-foo"]),
            None
        );
    }

    #[test]
    fn read_with_lists_then_reads_the_chosen_type() {
        let calls = RefCell::new(Vec::new());
        let content = read_with(|args| {
            calls.borrow_mut().push(args.join(" "));
            match args {
                ["--list-types"] => Some(b"text/html\nimage/png\n".to_vec()),
                ["--no-newline", "--type", "image/png"] => Some(vec![1, 2, 3]),
                _ => None,
            }
        });
        assert_eq!(content, ClipboardContent::Image(vec![1, 2, 3]));
        assert_eq!(
            *calls.borrow(),
            ["--list-types", "--no-newline --type image/png"]
        );
    }

    #[test]
    fn read_with_reads_uri_lists_as_text() {
        let content = read_with(|args| match args {
            ["--list-types"] => Some(b"text/uri-list\nUTF8_STRING\n".to_vec()),
            ["--no-newline", "--type", "text/uri-list"] => Some(b"file:///a.png\r\n".to_vec()),
            _ => None,
        });
        assert_eq!(content, ClipboardContent::Text("file:///a.png\r\n".into()));
    }

    #[test]
    fn read_with_reports_nothing_when_wl_paste_fails_or_is_empty() {
        assert_eq!(read_with(|_| None), ClipboardContent::Nothing);
        assert_eq!(
            read_with(|args| match args {
                ["--list-types"] => Some(b"text/html\n".to_vec()),
                _ => Some(b"<b>hi</b>".to_vec()),
            }),
            ClipboardContent::Nothing
        );
        assert_eq!(
            read_with(|args| match args {
                ["--list-types"] => Some(b"image/png\n".to_vec()),
                _ => Some(Vec::new()),
            }),
            ClipboardContent::Nothing
        );
    }

    // ---- file lists --------------------------------------------------------

    #[test]
    fn uri_list_parses_with_comments_crlf_and_escapes() {
        let text = "# from a file manager\r\nfile:///home/me/My%20Pics/cat.png\r\nfile://localhost/tmp/b.jpg\r\n";
        assert_eq!(
            parse_file_list(text),
            Some(vec![
                PathBuf::from("/home/me/My Pics/cat.png"),
                PathBuf::from("/tmp/b.jpg"),
            ])
        );
    }

    #[test]
    fn plain_absolute_paths_and_gnome_copy_lists_parse() {
        assert_eq!(
            parse_file_list("/tmp/a.png\n/tmp/b.png"),
            Some(vec![
                PathBuf::from("/tmp/a.png"),
                PathBuf::from("/tmp/b.png")
            ])
        );
        assert_eq!(
            parse_file_list("copy\nfile:///tmp/a.png"),
            Some(vec![PathBuf::from("/tmp/a.png")])
        );
    }

    #[test]
    fn other_text_is_not_a_file_list() {
        for text in [
            "",
            "hello world",
            "/tmp/a.png\nand some words",
            "https://example.com/cat.png",
            "file://otherhost/tmp/a.png",
            "file:///bad%zzescape.png",
            "relative/path.png",
        ] {
            assert_eq!(parse_file_list(text), None, "{text:?}");
        }
    }

    // ---- plan --------------------------------------------------------------

    #[test]
    fn plan_prefers_what_the_clipboard_holds() {
        assert_eq!(
            plan_paste(ClipboardContent::Image(vec![9]), any_file),
            PastePlan::Image(vec![9])
        );
        let files = vec![PathBuf::from("/x.png")];
        assert_eq!(
            plan_paste(ClipboardContent::Files(files.clone()), any_file),
            PastePlan::Files(files.clone())
        );
        assert_eq!(
            plan_paste(ClipboardContent::Text("file:///x.png".into()), any_file),
            PastePlan::Files(files)
        );
        assert_eq!(
            plan_paste(ClipboardContent::Text("just words".into()), any_file),
            PastePlan::NoImage
        );
        assert_eq!(
            plan_paste(ClipboardContent::Image(Vec::new()), any_file),
            PastePlan::NoImage
        );
        assert_eq!(
            plan_paste(ClipboardContent::Files(Vec::new()), any_file),
            PastePlan::NoImage
        );
        assert_eq!(
            plan_paste(ClipboardContent::Nothing, any_file),
            PastePlan::NoImage
        );
    }

    #[test]
    fn text_paths_must_all_be_existing_files() {
        let text = "/tmp/exists.png\n/tmp/missing.png";
        let plan = plan_paste(ClipboardContent::Text(text.into()), |p| {
            p == Path::new("/tmp/exists.png")
        });
        assert_eq!(plan, PastePlan::NoImage);
    }

    // ---- insert path -------------------------------------------------------

    #[test]
    fn pasted_bytes_insert_as_one_undoable_selected_layer() {
        let mut st = EditorState::new();
        let before = st.document.clone();
        assert!(st.begin_paste());
        assert_eq!(st.status, "Pasting…");
        let loaded = load_image_bytes(PASTED_LAYER_NAME.into(), &png(4, 3));
        let id = st.finish_paste(loaded).unwrap();
        let layer = st.document.layer(id).unwrap();
        assert_eq!(layer.name, "Pasted image");
        assert_eq!(st.selected_layer, Some(id));
        let LayerKind::Image(img) = &layer.kind else {
            panic!("not an image layer");
        };
        assert_eq!(st.sources.get(&img.source).unwrap().size(), Size::new(4, 3));
        assert_eq!(st.status, "Inserted Pasted image (4×3)");
        assert_eq!(st.history.undo_label(), Some(EditLabel::InsertImage));
        assert!(st.undo());
        assert_eq!(st.document, before);
        assert!(!st.undo());
    }

    #[test]
    fn pasting_the_same_picture_twice_stores_one_source() {
        let mut st = EditorState::new();
        let bytes = png(5, 5);
        let a = st
            .finish_paste(load_image_bytes(PASTED_LAYER_NAME.into(), &bytes))
            .unwrap();
        let b = st
            .finish_paste(load_image_bytes(PASTED_LAYER_NAME.into(), &bytes))
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(st.sources.len(), 1);
    }

    #[test]
    fn big_pastes_fit_the_canvas() {
        let mut st = EditorState::new();
        let canvas = st.document.canvas_size;
        let bytes = png(canvas.width * 2, canvas.height);
        let id = st
            .finish_paste(load_image_bytes(PASTED_LAYER_NAME.into(), &bytes))
            .unwrap();
        let LayerKind::Image(img) = &st.document.layer(id).unwrap().kind else {
            panic!("not an image layer");
        };
        assert_eq!(img.transform.scale_x, 0.5);
        assert!(st.status.contains("fit at 50%"), "{}", st.status);
    }

    #[test]
    fn undecodable_paste_reports_and_leaves_the_document() {
        let mut st = EditorState::new();
        let before = st.document.clone();
        let r = st.finish_paste(load_image_bytes(PASTED_LAYER_NAME.into(), b"not an image"));
        assert_eq!(r, None);
        assert_eq!(st.document, before);
        assert!(st.status.starts_with("Can't paste: "), "{}", st.status);
        assert!(!st.history.can_undo());
    }

    #[test]
    fn nothing_on_the_clipboard_says_so() {
        let mut st = EditorState::new();
        st.paste_found_nothing();
        assert_eq!(st.status, "Clipboard has no image");
    }

    #[test]
    fn paste_is_refused_while_a_dialog_is_open_and_closes_menus() {
        let mut st = EditorState::new();
        st.edit_menu_open = true;
        assert!(st.begin_paste());
        assert!(!st.menu_open());
        st.dispatch(FileCommand::ProjectSettings);
        assert!(st.project_dialog.is_some());
        let status = st.status.clone();
        assert!(!st.begin_paste());
        assert_eq!(st.status, status);
    }
}
