//! The brush paths the user added, remembered across runs in
//! `$XDG_CONFIG_HOME/retro-paint/brushes.json` (`~/.config/...` when
//! `XDG_CONFIG_HOME` is unset or not absolute): a JSON array of path
//! strings, in the order they were added. Missing paths are dropped on
//! startup (and the file rewritten) with a status note. An unreadable or
//! malformed file counts as empty. Editor config, never part of a document.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The config file name inside `retro-paint/`.
pub const BRUSHES_FILE: &str = "brushes.json";

/// Where the brush list lives, from `$XDG_CONFIG_HOME` and `$HOME`.
pub fn config_file(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match xdg_config_home.map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => PathBuf::from(home?).join(".config"),
    };
    Some(base.join("retro-paint").join(BRUSHES_FILE))
}

/// [`config_file`] from the process environment.
pub fn config_file_from_env() -> Option<PathBuf> {
    config_file(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// The paths in a brushes.json text; malformed text gives an error.
pub fn parse_paths(text: &str) -> Result<Vec<PathBuf>, String> {
    let list: Vec<String> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(list.into_iter().map(PathBuf::from).collect())
}

/// brushes.json text for `paths`.
pub fn paths_json(paths: &[PathBuf]) -> String {
    let list: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut s = serde_json::to_string_pretty(&list).expect("strings serialize");
    s.push('\n');
    s
}

/// The saved paths; a missing, unreadable or malformed file is empty.
pub fn read_paths(file: &Path) -> Vec<PathBuf> {
    fs::read_to_string(file)
        .ok()
        .and_then(|t| parse_paths(&t).ok())
        .unwrap_or_default()
}

/// Writes `paths` to `file`, creating its folder; replaces the file
/// atomically (temp file + rename).
pub fn write_paths(file: &Path, paths: &[PathBuf]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, paths_json(paths))?;
    fs::rename(&tmp, file)
}

/// The startup read: saved paths split into those that still exist and
/// those that do not.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Restored {
    pub kept: Vec<PathBuf>,
    pub dropped: Vec<PathBuf>,
}

impl Restored {
    /// Status note for dropped paths, if any.
    pub fn note(&self) -> Option<String> {
        match self.dropped.len() {
            0 => None,
            1 => Some(format!(
                "Brush path {} is missing; removed it from the library",
                self.dropped[0].display()
            )),
            n => Some(format!(
                "{n} brush paths are missing; removed them from the library"
            )),
        }
    }
}

/// Reads `file`, drops paths that no longer exist and rewrites the file
/// when any were dropped. File I/O: run it off the UI thread.
pub fn restore(file: &Path) -> Restored {
    let (kept, dropped): (Vec<_>, Vec<_>) = read_paths(file).into_iter().partition(|p| p.exists());
    if !dropped.is_empty() {
        // Best effort: a read-only config just keeps the stale entries.
        let _ = write_paths(file, &kept);
    }
    Restored { kept, dropped }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rp-brush-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn config_file_follows_xdg_then_home() {
        assert_eq!(
            config_file(Some("/x/cfg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/cfg/retro-paint/brushes.json"))
        );
        // Relative or empty XDG_CONFIG_HOME is ignored, per the spec.
        for xdg in [None, Some("".into()), Some("rel".into())] {
            assert_eq!(
                config_file(xdg, Some("/home/u".into())),
                Some(PathBuf::from("/home/u/.config/retro-paint/brushes.json"))
            );
        }
        assert_eq!(config_file(None, None), None);
    }

    #[test]
    fn paths_round_trip_and_bad_text_is_an_error() {
        let paths = vec![PathBuf::from("/a/b.abr"), PathBuf::from("/c d/\"q\"")];
        assert_eq!(parse_paths(&paths_json(&paths)).unwrap(), paths);
        assert!(parse_paths("{\"paths\": 3}").is_err());
        assert!(parse_paths("").is_err());
    }

    #[test]
    fn restore_drops_missing_paths_and_rewrites_the_file() {
        let d = temp_dir("restore");
        let file = d.join("retro-paint").join(BRUSHES_FILE);
        assert_eq!(restore(&file), Restored::default(), "no file yet");
        let here = d.join("brushes");
        fs::create_dir_all(&here).unwrap();
        let gone = d.join("gone.abr");
        write_paths(&file, &[here.clone(), gone.clone()]).unwrap();
        let r = restore(&file);
        assert_eq!(r.kept, std::slice::from_ref(&here));
        assert_eq!(r.dropped, [gone]);
        assert!(r.note().unwrap().contains("gone.abr"));
        assert_eq!(read_paths(&file), std::slice::from_ref(&here));
        let again = restore(&file);
        assert_eq!(again.kept, [here]);
        assert!(again.note().is_none());
        fs::write(&file, "garbage").unwrap();
        assert!(read_paths(&file).is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
