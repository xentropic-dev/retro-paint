//! `format_version` handling: step-by-step manifest migrations.
//!
//! Each step takes the raw manifest JSON of version `n` to version `n + 1`
//! and works on [`serde_json::Value`], so old schemas never need Rust types.
//! [`migrate`] runs every step from the file's version up to
//! [`FORMAT_VERSION`]; only then is the manifest parsed into the current
//! schema.
//!
//! Versions:
//! - **0**: the pre-release draft. Never written by a release; kept as the
//!   first step so the chain is exercised by tests. Sizes were flat fields
//!   (`canvas_width`, `canvas_height`, `output_width`, `output_height`).
//! - **1**: current. Sizes are `{"width", "height"}` objects (`canvas`,
//!   `output`).

use serde_json::{Map, Value};

use crate::FormatError;

/// The manifest version this build writes and reads natively.
pub const FORMAT_VERSION: u32 = 1;

type Step = fn(Map<String, Value>) -> Result<Map<String, Value>, FormatError>;

/// `STEPS[n]` migrates version `n` to `n + 1`.
const STEPS: [Step; FORMAT_VERSION as usize] = [v0_to_v1];

/// Reads `format_version` from a raw manifest.
pub fn format_version(manifest: &Value) -> Result<u64, FormatError> {
    manifest
        .get("format_version")
        .ok_or_else(|| FormatError::InvalidManifest("missing `format_version`".into()))?
        .as_u64()
        .ok_or_else(|| {
            FormatError::InvalidManifest("`format_version` is not a non-negative integer".into())
        })
}

/// Migrates a raw manifest from version `from` to [`FORMAT_VERSION`], one
/// step at a time. The result has `format_version` set to the current
/// version. A `from` newer than this build is refused with
/// [`FormatError::NewerVersion`].
pub fn migrate(value: Value, from: u64) -> Result<Value, FormatError> {
    if from > u64::from(FORMAT_VERSION) {
        return Err(FormatError::NewerVersion {
            found: from,
            supported: FORMAT_VERSION,
        });
    }
    let Value::Object(mut map) = value else {
        return Err(FormatError::InvalidManifest(
            "manifest is not a JSON object".into(),
        ));
    };
    for (version, step) in STEPS.iter().enumerate().skip(from as usize) {
        map = step(map)?;
        map.insert("format_version".into(), Value::from(version + 1));
    }
    Ok(Value::Object(map))
}

/// v0 → v1: flat size fields become `canvas` / `output` objects.
fn v0_to_v1(mut m: Map<String, Value>) -> Result<Map<String, Value>, FormatError> {
    for (key, w, h) in [
        ("canvas", "canvas_width", "canvas_height"),
        ("output", "output_width", "output_height"),
    ] {
        let mut take = |k: &str| {
            m.remove(k)
                .ok_or_else(|| FormatError::InvalidManifest(format!("v0 manifest missing `{k}`")))
        };
        let (width, height) = (take(w)?, take(h)?);
        let mut size = Map::new();
        size.insert("width".into(), width);
        size.insert("height".into(), height);
        m.insert(key.into(), Value::Object(size));
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn current_version_is_unchanged() {
        let v = json!({"format_version": 1, "canvas": {"width": 1, "height": 2}});
        assert_eq!(migrate(v.clone(), 1).unwrap(), v);
    }

    #[test]
    fn v0_migrates_to_v1() {
        let v0 = json!({
            "format_version": 0,
            "canvas_width": 8, "canvas_height": 4,
            "output_width": 2, "output_height": 1,
            "layout": "mobile",
        });
        assert_eq!(
            migrate(v0, 0).unwrap(),
            json!({
                "format_version": 1,
                "canvas": {"width": 8, "height": 4},
                "output": {"width": 2, "height": 1},
                "layout": "mobile",
            })
        );
    }

    #[test]
    fn broken_v0_is_an_error() {
        let v0 = json!({"format_version": 0, "canvas_width": 8});
        assert!(matches!(
            migrate(v0, 0),
            Err(FormatError::InvalidManifest(_))
        ));
        assert!(matches!(
            migrate(json!([1]), 0),
            Err(FormatError::InvalidManifest(_))
        ));
    }

    #[test]
    fn newer_is_refused_naming_both_versions() {
        let err = migrate(json!({"format_version": 7}), 7).unwrap_err();
        assert!(matches!(
            err,
            FormatError::NewerVersion {
                found: 7,
                supported: 1
            }
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("version 7") && msg.contains("version 1"),
            "{msg}"
        );
    }

    #[test]
    fn reads_format_version() {
        assert_eq!(format_version(&json!({"format_version": 3})).unwrap(), 3);
        assert!(format_version(&json!({})).is_err());
        assert!(format_version(&json!({"format_version": "1"})).is_err());
        assert!(format_version(&json!({"format_version": -1})).is_err());
    }
}
