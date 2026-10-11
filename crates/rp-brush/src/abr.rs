//! Photoshop `.abr` sampled brushes (versions 1, 2, 6 and 10), read the way
//! GIMP reads them.
//!
//! Loading is split in two so a brush panel can open a 75 MB file cheaply:
//! [`index_abr`] walks the file and returns one [`AbrEntry`] per sampled brush
//! (name, spacing, size and where its pixels are) without decoding any
//! pixels, and [`decode_abr`] decodes one entry into a [`Brush`] on demand.
//! [`parse_abr`] does both for every brush.
//!
//! # Layout (all integers big-endian)
//!
//! Header `u16 version, u16 count_or_subversion`.
//!
//! - **v1 / v2**: `count` records of `u16 type, u32 size`. Type 1 (computed)
//!   is skipped; type 2 (sampled) is `u32 misc, u16 spacing`, then for v2 a
//!   UCS-2 name (`u32` length in code units, then UTF-16BE), `u8 antialias,
//!   4 x i16 bounds, 4 x i32 bounds (top, left, bottom, right), u16 depth,
//!   u8 compression`, then the pixels.
//! - **v6 / v10** (subversion 1 or 2): a sequence of `8BIM <tag> u32 length`
//!   sections. `samp` holds the samples, each `u32 length` (records padded
//!   to 4 bytes) starting with a 37-byte Pascal-string uuid, then 10 (sub 1)
//!   or 264 (sub 2) unknown bytes, then the i32 bounds, depth and
//!   compression as above. `desc` is an action descriptor holding the
//!   presets: each `brushPreset` has a `Nm  ` name and a `Brsh` object whose
//!   `sampledData` is the sample's uuid and whose `Spcn` is the spacing in
//!   percent. Other sections (`patt`, `phry`) are skipped.
//!
//! Pixels are `height` rows of `width * depth / 8` bytes, raw or PackBits
//! RLE (a `u16` compressed length per row, then the rows). 16-bit samples
//! are reduced to 8 bits. Sample values are coverage (255 = paint), which is
//! already the [`Mask`] convention: on the dark-on-white brush preview
//! Photoshop shows they are inverted, but the stored samples are not, and
//! GIMP copies them straight into the mask.
//!
//! Names and spacing come from `desc` when it parses and names the sample's
//! uuid, and entries are then listed in preset order (as Photoshop lists
//! them), followed by any samples no preset names. Otherwise a brush is
//! called `"<file stem> N"` (N is its 1-based position in the index) with 25%
//! spacing. A name that repeats within one file gets `" #2"`, `" #3"`, ...
//! (some packs give every preset the same name). Computed (non-sampled)
//! presets have no sample, so they never appear; neither do samples of a
//! depth other than 8 or 16.

use std::collections::HashMap;
use std::ops::Range;

use crate::{Brush, BrushError, Mask};

/// Largest sample side accepted.
const MAX_DIM: u32 = 10_000;
/// Spacing used when the file does not give one.
const DEFAULT_SPACING: f32 = 25.0;

/// The brushes found in an `.abr` file, without their pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct AbrIndex {
    pub version: u16,
    /// Subversion for v6/v10; the brush count for v1/v2.
    pub subversion: u16,
    /// Sampled brushes in file order.
    pub entries: Vec<AbrEntry>,
}

/// One sampled brush in an `.abr` file; decode it with [`decode_abr`].
#[derive(Debug, Clone, PartialEq)]
pub struct AbrEntry {
    pub name: String,
    /// Percent of the brush size.
    pub spacing: f32,
    pub width: u32,
    pub height: u32,
    /// Bits per sample in the file, 8 or 16 (decoded masks are always 8-bit).
    pub depth: u16,
    /// PackBits RLE rather than raw rows.
    pub compressed: bool,
    /// The sample's uuid (v6/v10 only).
    pub uuid: Option<String>,
    /// Byte range of the pixel data in the file: the RLE row lengths and
    /// rows, or the raw rows. A caller can read just this range from disk and
    /// pass it to [`decode_abr_data`].
    pub data: Range<usize>,
}

struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(d: &'a [u8], pos: usize) -> Self {
        Self { d, pos }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], BrushError> {
        let end = self.pos.checked_add(n).ok_or(BrushError::Truncated)?;
        let s = self.d.get(self.pos..end).ok_or(BrushError::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn skip(&mut self, n: usize) -> Result<(), BrushError> {
        self.bytes(n).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8, BrushError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, BrushError> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, BrushError> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, BrushError> {
        Ok(self.u32()? as i32)
    }

    fn f64(&mut self) -> Result<f64, BrushError> {
        let b = self.bytes(8)?;
        Ok(f64::from_be_bytes(b.try_into().unwrap()))
    }

    /// `u32` length in UTF-16 code units, then UTF-16BE; trailing NULs dropped.
    fn ucs2(&mut self) -> Result<String, BrushError> {
        let n = self.u32()? as usize;
        let b = self.bytes(n.checked_mul(2).ok_or(BrushError::Truncated)?)?;
        let units: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        let s = String::from_utf16_lossy(&units);
        Ok(s.trim_end_matches('\0').to_string())
    }
}

struct SampleHeader {
    width: u32,
    height: u32,
    depth: u16,
    compressed: bool,
    data: Range<usize>,
}

/// The part of a sampled brush shared by all versions: bounds, depth,
/// compression. Leaves the reader at the pixel data.
fn sample_header(r: &mut Reader, end: usize) -> Result<Option<SampleHeader>, BrushError> {
    let top = r.i32()?;
    let left = r.i32()?;
    let bottom = r.i32()?;
    let right = r.i32()?;
    let depth = r.u16()?;
    let compression = r.u8()?;
    let width = i64::from(right) - i64::from(left);
    let height = i64::from(bottom) - i64::from(top);
    if width <= 0 || height <= 0 || width > MAX_DIM as i64 || height > MAX_DIM as i64 {
        return Err(BrushError::Invalid(format!("sample size {width}x{height}")));
    }
    if compression > 1 {
        return Err(BrushError::Unsupported(format!(
            "abr compression {compression}"
        )));
    }
    if r.pos > end || end > r.d.len() {
        return Err(BrushError::Truncated);
    }
    if depth != 8 && depth != 16 {
        // Not a format we decode; skip the brush like a computed one.
        return Ok(None);
    }
    let (w, h) = (width as u32, height as u32);
    let data = r.pos..end;
    let need = if compression == 1 {
        2 * h as usize
    } else {
        w as usize * h as usize * (depth as usize / 8)
    };
    if data.len() < need {
        return Err(BrushError::Truncated);
    }
    Ok(Some(SampleHeader {
        width: w,
        height: h,
        depth,
        compressed: compression == 1,
        data,
    }))
}

/// Index every sampled brush in an `.abr` file without decoding pixels.
/// `stem` names brushes the file leaves unnamed (`"<stem> N"`).
pub fn index_abr(bytes: &[u8], stem: &str) -> Result<AbrIndex, BrushError> {
    let mut r = Reader::new(bytes, 0);
    let version = r.u16()?;
    let sub = r.u16()?;
    let mut entries = match version {
        1 | 2 => index_v12(&mut r, version, sub)?,
        6 | 10 => {
            if sub != 1 && sub != 2 {
                return Err(BrushError::Unsupported(format!(
                    "abr version {version}.{sub}"
                )));
            }
            index_v6(&mut r, sub)?
        }
        v => return Err(BrushError::Unsupported(format!("abr version {v}"))),
    };
    // Unnamed brushes are "<stem> N"; repeated names get " #2", " #3", ...
    let mut seen: HashMap<String, u32> = HashMap::new();
    for (i, e) in entries.iter_mut().enumerate() {
        if e.name.is_empty() {
            e.name = format!("{stem} {}", i + 1);
        }
        let n = seen.entry(e.name.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            e.name = format!("{} #{n}", e.name);
        }
    }
    Ok(AbrIndex {
        version,
        subversion: sub,
        entries,
    })
}

fn index_v12(r: &mut Reader, version: u16, count: u16) -> Result<Vec<AbrEntry>, BrushError> {
    let mut out = Vec::new();
    for _ in 0..count {
        let kind = r.u16()?;
        let size = r.u32()? as usize;
        let start = r.pos;
        let end = start.checked_add(size).ok_or(BrushError::Truncated)?;
        if end > r.d.len() {
            return Err(BrushError::Truncated);
        }
        if kind == 2 {
            r.skip(4)?; // misc
            let spacing = r.u16()? as f32;
            let name = if version == 2 {
                r.ucs2()?
            } else {
                String::new()
            };
            r.skip(1 + 8)?; // antialiasing, short bounds
            if let Some(SampleHeader {
                width,
                height,
                depth,
                compressed,
                data,
            }) = sample_header(r, end)?
            {
                out.push(AbrEntry {
                    name,
                    spacing,
                    width,
                    height,
                    depth,
                    compressed,
                    uuid: None,
                    data,
                });
            }
        } else if kind != 1 {
            return Err(BrushError::Invalid(format!("abr brush type {kind}")));
        }
        r.pos = end;
    }
    Ok(out)
}

fn index_v6(r: &mut Reader, sub: u16) -> Result<Vec<AbrEntry>, BrushError> {
    let mut out = Vec::new();
    let mut desc: Option<(usize, usize)> = None;
    let mut saw_samp = false;
    while r.d.len() - r.pos >= 12 {
        let sig = r.bytes(4)?;
        if sig != b"8BIM" {
            if saw_samp {
                break; // trailing data after the sections we need
            }
            return Err(BrushError::Invalid("missing 8BIM section".into()));
        }
        let tag: [u8; 4] = r.bytes(4)?.try_into().unwrap();
        let len = r.u32()? as usize;
        let start = r.pos;
        let end = start.checked_add(len).ok_or(BrushError::Truncated)?;
        if end > r.d.len() {
            return Err(BrushError::Truncated);
        }
        match &tag {
            b"samp" => {
                saw_samp = true;
                index_samples(r, sub, end, &mut out)?;
            }
            b"desc" => desc = Some((start, end)),
            _ => {}
        }
        r.pos = end;
        // Sections are normally 4-aligned already; tolerate padding.
        if r.d.get(r.pos..r.pos + 4) != Some(b"8BIM") {
            let aligned = r.pos.next_multiple_of(4);
            if r.d.get(aligned..aligned + 4) == Some(b"8BIM") {
                r.pos = aligned;
            }
        }
    }
    if !saw_samp {
        return Err(BrushError::Invalid("no samp section".into()));
    }
    if let Some((s, e)) = desc {
        // A descriptor we cannot read only costs the names and order.
        if let Ok(presets) = parse_presets(&r.d[s..e]) {
            out = apply_presets(out, &presets);
        }
    }
    Ok(out)
}

/// Name and space samples from their presets and put them in preset order;
/// samples no preset names keep file order after them.
fn apply_presets(samples: Vec<AbrEntry>, presets: &[Preset]) -> Vec<AbrEntry> {
    let mut slots: Vec<Option<AbrEntry>> = samples.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(slots.len());
    for p in presets {
        let found = slots.iter_mut().find(|s| {
            s.as_ref()
                .is_some_and(|e| e.uuid.as_deref() == Some(p.uuid.as_str()))
        });
        if let Some(mut e) = found.and_then(Option::take) {
            e.name = p.name.clone();
            if let Some(sp) = p.spacing {
                e.spacing = sp;
            }
            out.push(e);
        }
    }
    out.extend(slots.into_iter().flatten());
    out
}

fn index_samples(
    r: &mut Reader,
    sub: u16,
    end: usize,
    out: &mut Vec<AbrEntry>,
) -> Result<(), BrushError> {
    while r.pos < end {
        let len = r.u32()? as usize;
        let start = r.pos;
        let rec_end = start.checked_add(len).ok_or(BrushError::Truncated)?;
        if rec_end > end {
            return Err(BrushError::Truncated);
        }
        let key_len = r.u8()? as usize;
        let key = r.bytes(36)?;
        let uuid = String::from_utf8_lossy(&key[..key_len.min(36)]).into_owned();
        r.skip(if sub == 1 { 10 } else { 264 })?;
        if r.pos > rec_end {
            return Err(BrushError::Truncated);
        }
        if let Some(SampleHeader {
            width,
            height,
            depth,
            compressed,
            data,
        }) = sample_header(r, rec_end)?
        {
            out.push(AbrEntry {
                name: String::new(),
                spacing: DEFAULT_SPACING,
                width,
                height,
                depth,
                compressed,
                uuid: Some(uuid),
                data,
            });
        }
        r.pos = rec_end + (4 - len % 4) % 4;
    }
    Ok(())
}

/// Decode one indexed brush; `bytes` is the whole file the index came from.
pub fn decode_abr(bytes: &[u8], entry: &AbrEntry) -> Result<Brush, BrushError> {
    let data = bytes.get(entry.data.clone()).ok_or(BrushError::Truncated)?;
    decode_abr_data(data, entry)
}

/// Decode one brush from just its pixel data (`file[entry.data]`).
pub fn decode_abr_data(data: &[u8], entry: &AbrEntry) -> Result<Brush, BrushError> {
    let (w, h) = (entry.width as usize, entry.height as usize);
    if w == 0 || h == 0 || entry.width > MAX_DIM || entry.height > MAX_DIM {
        return Err(BrushError::Invalid(format!("sample size {w}x{h}")));
    }
    let bpp = match entry.depth {
        8 => 1,
        16 => 2,
        d => return Err(BrushError::Unsupported(format!("abr depth {d}"))),
    };
    let row = w * bpp;
    let raw = if entry.compressed {
        unpack_rows(data, row, h)?
    } else {
        data.get(..row * h).ok_or(BrushError::Truncated)?.to_vec()
    };
    let mask = if bpp == 1 {
        raw
    } else {
        raw.as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                let v = u16::from_be_bytes(*c) as u32;
                ((v * 255 + 32767) / 65535) as u8
            })
            .collect()
    };
    Ok(Brush::bitmap(
        entry.name.clone(),
        entry.spacing,
        Mask::new(entry.width, entry.height, mask),
        None,
    ))
}

/// PackBits rows: `h` u16 compressed lengths, then each row.
fn unpack_rows(data: &[u8], row: usize, h: usize) -> Result<Vec<u8>, BrushError> {
    let mut r = Reader::new(data, 0);
    let mut lens = Vec::with_capacity(h);
    for _ in 0..h {
        lens.push(r.u16()? as usize);
    }
    // PackBits expands at most 64x, so a corrupt header cannot make us
    // reserve far more than the data could fill.
    let mut out = Vec::with_capacity((row * h).min(data.len().saturating_mul(64)));
    for len in lens {
        let src = r.bytes(len)?;
        let start = out.len();
        let mut i = 0;
        while i < src.len() {
            let n = src[i] as i8;
            i += 1;
            if n >= 0 {
                let n = n as usize + 1;
                let lit = src.get(i..i + n).ok_or(BrushError::Truncated)?;
                out.extend_from_slice(lit);
                i += n;
            } else if n != -128 {
                let v = *src.get(i).ok_or(BrushError::Truncated)?;
                i += 1;
                out.extend(std::iter::repeat_n(v, (1 - n as isize) as usize));
            }
            if out.len() - start > row {
                return Err(BrushError::Invalid("rle row overflows".into()));
            }
        }
        if out.len() - start != row {
            return Err(BrushError::Invalid("rle row is short".into()));
        }
    }
    Ok(out)
}

/// Index and decode every brush in an `.abr` file.
pub fn parse_abr(bytes: &[u8], stem: &str) -> Result<Vec<Brush>, BrushError> {
    index_abr(bytes, stem)?
        .entries
        .iter()
        .map(|e| decode_abr(bytes, e))
        .collect()
}

// ---- `desc` action descriptor -------------------------------------------

struct Preset {
    name: String,
    uuid: String,
    spacing: Option<f32>,
}

/// A descriptor value, keeping only what preset lookup needs.
enum Value {
    Obj(Vec<(String, Value)>),
    List(Vec<Value>),
    Text(String),
    Unit(f64),
    Other,
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
}

const MAX_NESTING: u32 = 32;

fn parse_presets(d: &[u8]) -> Result<Vec<Preset>, BrushError> {
    let mut r = Reader::new(d, 0);
    let version = r.u32()?;
    if version != 16 {
        return Err(BrushError::Unsupported(format!("descriptor {version}")));
    }
    let root = descriptor(&mut r, 0)?;
    let Some(Value::List(list)) = root.get("Brsh") else {
        return Ok(Vec::new());
    };
    Ok(list
        .iter()
        .filter_map(|p| {
            let brush = p.get("Brsh")?;
            let uuid = brush.get("sampledData")?.text()?.to_string();
            let name = p
                .get("Nm  ")
                .or_else(|| brush.get("Nm  "))
                .and_then(Value::text)
                .unwrap_or("")
                .to_string();
            let spacing = match brush.get("Spcn") {
                Some(Value::Unit(v)) if v.is_finite() && *v > 0.0 => Some(*v as f32),
                _ => None,
            };
            Some(Preset {
                name,
                uuid,
                spacing,
            })
        })
        .collect())
}

/// A `u32` length then that many bytes, or a 4-byte key when the length is 0.
fn key(r: &mut Reader) -> Result<String, BrushError> {
    let n = r.u32()? as usize;
    let b = r.bytes(if n == 0 { 4 } else { n })?;
    Ok(String::from_utf8_lossy(b).into_owned())
}

fn descriptor(r: &mut Reader, depth: u32) -> Result<Value, BrushError> {
    if depth > MAX_NESTING {
        return Err(BrushError::Invalid("descriptor nests too deep".into()));
    }
    r.ucs2()?; // display name
    key(r)?; // class id
    let count = r.u32()?;
    let mut items = Vec::new();
    for _ in 0..count {
        let k = key(r)?;
        let v = value(r, depth + 1)?;
        items.push((k, v));
    }
    Ok(Value::Obj(items))
}

fn value(r: &mut Reader, depth: u32) -> Result<Value, BrushError> {
    if depth > MAX_NESTING {
        return Err(BrushError::Invalid("descriptor nests too deep".into()));
    }
    let ty: [u8; 4] = r.bytes(4)?.try_into().unwrap();
    Ok(match &ty {
        b"Objc" | b"GlbO" => descriptor(r, depth)?,
        b"VlLs" => {
            let n = r.u32()?;
            let mut v = Vec::new();
            for _ in 0..n {
                v.push(value(r, depth + 1)?);
            }
            Value::List(v)
        }
        b"TEXT" => Value::Text(r.ucs2()?),
        b"UntF" => {
            r.skip(4)?;
            Value::Unit(r.f64()?)
        }
        b"doub" => Value::Unit(r.f64()?),
        b"long" => {
            r.skip(4)?;
            Value::Other
        }
        b"comp" => {
            r.skip(8)?;
            Value::Other
        }
        b"bool" => {
            r.skip(1)?;
            Value::Other
        }
        b"enum" => {
            key(r)?;
            key(r)?;
            Value::Other
        }
        b"type" | b"GlbC" => {
            r.ucs2()?;
            key(r)?;
            Value::Other
        }
        b"alis" | b"tdta" | b"Pth " => {
            let n = r.u32()? as usize;
            r.skip(n)?;
            Value::Other
        }
        b"UnFl" => {
            r.skip(4)?;
            let n = r.u32()? as usize;
            r.skip(n.checked_mul(8).ok_or(BrushError::Truncated)?)?;
            Value::Other
        }
        b"ObAr" => {
            r.u32()?; // item count
            r.ucs2()?;
            key(r)?;
            let n = r.u32()?;
            for _ in 0..n {
                key(r)?;
                value(r, depth + 1)?;
            }
            Value::Other
        }
        b"obj " => {
            let n = r.u32()?;
            for _ in 0..n {
                reference_item(r)?;
            }
            Value::Other
        }
        t => {
            return Err(BrushError::Unsupported(format!(
                "descriptor type {:?}",
                String::from_utf8_lossy(t)
            )));
        }
    })
}

fn reference_item(r: &mut Reader) -> Result<(), BrushError> {
    let ty: [u8; 4] = r.bytes(4)?.try_into().unwrap();
    match &ty {
        b"prop" => {
            r.ucs2()?;
            key(r)?;
            key(r)?;
        }
        b"Clss" => {
            r.ucs2()?;
            key(r)?;
        }
        b"Enmr" => {
            r.ucs2()?;
            key(r)?;
            key(r)?;
            key(r)?;
        }
        b"rele" => {
            r.ucs2()?;
            key(r)?;
            r.skip(4)?;
        }
        b"Idnt" | b"indx" => r.skip(4)?,
        b"name" => {
            r.ucs2()?;
            key(r)?;
            r.ucs2()?;
        }
        t => {
            return Err(BrushError::Unsupported(format!(
                "reference type {:?}",
                String::from_utf8_lossy(t)
            )));
        }
    }
    Ok(())
}
