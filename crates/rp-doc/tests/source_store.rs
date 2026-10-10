//! `SourceStore` keeps sources behind `Arc` so clones share pixel buffers.

use std::sync::Arc;
use std::time::Instant;

use rp_doc::{RgbaImage, Size, SourceHash, SourceStore};

fn store_with(n: usize, size: Size) -> SourceStore {
    let mut store = SourceStore::new();
    for i in 0..n {
        store.insert(
            SourceHash::new(format!("{i:064x}")),
            RgbaImage::filled(size, [i as u8, 0, 0, 255]),
        );
    }
    store
}

#[test]
fn clone_shares_pixel_buffers() {
    let store = store_with(2, Size::new(8, 4));
    let copy = store.clone();
    for h in store.hashes() {
        let a = store.get_arc(h).unwrap();
        let b = copy.get_arc(h).unwrap();
        assert!(Arc::ptr_eq(a, b));
        assert_eq!(
            store.get(h).unwrap().as_raw().as_ptr(),
            copy.get(h).unwrap().as_raw().as_ptr()
        );
    }
}

#[test]
fn insert_into_clone_leaves_original_and_shares_existing() {
    let store = store_with(1, Size::new(2, 2));
    let mut copy = store.clone();
    copy.insert(SourceHash::new("beef"), RgbaImage::new(Size::new(1, 1)));
    assert_eq!(store.len(), 1);
    assert_eq!(copy.len(), 2);
    let h = store.hashes().next().unwrap();
    assert!(Arc::ptr_eq(
        store.get_arc(h).unwrap(),
        copy.get_arc(h).unwrap()
    ));
}

#[test]
fn insert_arc_shares_callers_buffer() {
    let img = Arc::new(RgbaImage::filled(Size::new(3, 3), [1, 2, 3, 4]));
    let mut store = SourceStore::new();
    let h = SourceHash::new("abcd");
    store.insert_arc(h.clone(), img.clone());
    assert!(Arc::ptr_eq(store.get_arc(&h).unwrap(), &img));
    assert_eq!(store.get(&h), Some(&*img));
}

#[test]
fn remove_returns_image_even_when_shared() {
    let mut store = store_with(1, Size::new(2, 2));
    let copy = store.clone();
    let h = store.hashes().next().unwrap().clone();
    let removed = store.remove(&h).unwrap();
    assert_eq!(Some(&removed), copy.get(&h));
    assert!(!store.contains(&h));
    assert!(copy.contains(&h));
}

/// Cost of cloning a store holding two 4000×3000 sources.
/// Run with `cargo test -p rp-doc --release -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_clone_two_large_sources() {
    let store = store_with(2, Size::new(4000, 3000));
    let deep = Instant::now();
    let copied: Vec<RgbaImage> = store
        .hashes()
        .map(|h| store.get(h).unwrap().clone())
        .collect();
    let deep = deep.elapsed();
    let shallow = Instant::now();
    let copy = store.clone();
    let shallow = shallow.elapsed();
    println!("deep copy of pixels (old clone cost): {deep:?}");
    println!("SourceStore::clone (Arc): {shallow:?}");
    assert_eq!(copy.len(), copied.len());
}
