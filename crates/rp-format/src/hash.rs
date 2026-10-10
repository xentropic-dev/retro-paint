//! [`SourceHash`] for inserted images.

use rp_doc::{RgbaImage, SourceHash};
use sha2::{Digest, Sha256};

/// The content hash of a decoded source image: SHA-256 of `width` and
/// `height` (each a big-endian `u32`) followed by the straight-alpha RGBA8
/// bytes, as 64 lowercase hex digits.
///
/// It hashes pixels, not file bytes, so the same image gets the same hash
/// whichever file it came from and however its PNG is encoded. Code that
/// inserts an image layer should name the source with this function; saving
/// and loading check it.
pub fn source_hash(image: &RgbaImage) -> SourceHash {
    let mut h = Sha256::new();
    h.update(image.width().to_be_bytes());
    h.update(image.height().to_be_bytes());
    h.update(image.as_raw());
    let digest = h.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    SourceHash::new(hex)
}

/// Whether `hash` has the shape [`source_hash`] produces: 64 lowercase hex
/// digits. Anything else is refused before it is used in an entry name.
pub fn is_well_formed(hash: &SourceHash) -> bool {
    let s = hash.as_str();
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rp_doc::Size;

    #[test]
    fn hash_is_sha256_of_size_and_pixels() {
        let img = RgbaImage::new(Size::new(0, 0));
        // SHA-256 of eight zero bytes.
        assert_eq!(
            source_hash(&img).as_str(),
            "af5570f5a1810b7af78caf4bc70a660f0df51e42baf91d4de5b2328de0e83dfc"
        );
        assert!(is_well_formed(&source_hash(&img)));
    }

    #[test]
    fn hash_depends_on_shape_and_pixels() {
        let a = RgbaImage::new(Size::new(2, 1));
        let b = RgbaImage::new(Size::new(1, 2));
        let mut c = a.clone();
        c.put(1, 0, [0, 0, 0, 1]);
        assert_ne!(source_hash(&a), source_hash(&b));
        assert_ne!(source_hash(&a), source_hash(&c));
        assert_eq!(source_hash(&a), source_hash(&a.clone()));
    }

    #[test]
    fn well_formed_rejects_paths_and_case() {
        assert!(!is_well_formed(&SourceHash::new("cafe")));
        assert!(!is_well_formed(&SourceHash::new("../".repeat(21) + "a")));
        assert!(!is_well_formed(&SourceHash::new("A".repeat(64))));
        assert!(is_well_formed(&SourceHash::new("0".repeat(64))));
    }
}
