//! `manifest.json` schema (version [`FORMAT_VERSION`]) as serde mirror
//! types, and conversions to and from the rp-doc / rp-palette-dither types.
//!
//! The mirrors keep serde out of the model crates and pin the on-disk names:
//! renaming a Rust variant never changes the file format.

use rp_doc::{BlendMode, Document, Layer, LayerKind, Layout, Lock, Size, Transform};
use rp_palette_dither::{
    CgaColor, DitherMethod, DitherSettings, FourColor, Intensity, Palette, PaletteChoice,
    PreAdjust, Preset, Rgb,
};
use serde::{Deserialize, Serialize};

use crate::FormatError;
use crate::migrate::FORMAT_VERSION;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Manifest {
    pub format_version: u32,
    pub canvas: SizeDto,
    pub output: SizeDto,
    pub layout: LayoutDto,
    pub dither: DitherDto,
    /// `#rrggbb`.
    pub fg: String,
    pub bg: String,
    pub swatches: Vec<String>,
    /// Bottom to top.
    pub layers: Vec<LayerDto>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
pub struct SizeDto {
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum LayoutDto {
    Mobile,
    Desktop,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct DitherDto {
    pub method: MethodDto,
    pub palette: PaletteDto,
    pub adjust: AdjustDto,
    pub bayer_spread: f32,
    pub strength: f32,
    pub serpentine: bool,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum MethodDto {
    None,
    Bayer4,
    FloydSteinberg,
    Atkinson,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaletteDto {
    Full,
    FourColor {
        preset: PresetDto,
        intensity: IntensityDto,
        background: CgaDto,
    },
    /// Colors in palette order.
    Custom { colors: Vec<CgaDto> },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PresetDto {
    Palette0,
    Palette1,
    Mode5,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum IntensityDto {
    Low,
    High,
}

/// The 16 CGA colors by name, in doc order.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CgaDto {
    Black,
    DarkGray,
    LightGray,
    White,
    Blue,
    LightBlue,
    Green,
    LightGreen,
    Cyan,
    LightCyan,
    Red,
    LightRed,
    Magenta,
    LightMagenta,
    Brown,
    Yellow,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
pub struct AdjustDto {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct LayerDto {
    pub name: String,
    pub visible: bool,
    /// Percent, `0..=100`.
    pub opacity: u8,
    /// [`BlendMode::name`].
    pub blend: String,
    pub lock: LockDto,
    /// Zip entry of the grayscale mask PNG, if the layer has a mask.
    pub mask: Option<String>,
    #[serde(flatten)]
    pub content: ContentDto,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
pub struct LockDto {
    pub pixels: bool,
    pub position: bool,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentDto {
    Paint {
        /// Zip entry of the RGBA PNG.
        pixels: String,
        offset_x: i32,
        offset_y: i32,
    },
    Image {
        /// [`SourceHash`](rp_doc::SourceHash); the pixels are
        /// `sources/<source>.png`.
        source: String,
        transform: TransformDto,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
pub struct TransformDto {
    pub translate_x: f32,
    pub translate_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation: f32,
}

// ---- model → manifest -------------------------------------------------------

/// Entry name of layer `index`'s (0-based, bottom first) pixel PNG.
pub fn layer_entry(index: usize) -> String {
    format!("layers/{:04}.png", index + 1)
}

/// Entry name of layer `index`'s mask PNG.
pub fn mask_entry(index: usize) -> String {
    format!("layers/{:04}-mask.png", index + 1)
}

/// Entry name of a source image.
pub fn source_entry(hash: &str) -> String {
    format!("sources/{hash}.png")
}

impl Manifest {
    /// The manifest for `doc`. Pixel data is referenced by entry name, not
    /// included. Refuses non-finite floats, which JSON cannot store.
    pub fn from_document(doc: &Document) -> Result<Self, FormatError> {
        let d = &doc.dither;
        finite("dither.bayer_spread", d.bayer_spread)?;
        finite("dither.strength", d.strength)?;
        finite("dither.adjust.brightness", d.adjust.brightness)?;
        finite("dither.adjust.contrast", d.adjust.contrast)?;
        finite("dither.adjust.saturation", d.adjust.saturation)?;
        let layers = doc
            .layers()
            .iter()
            .enumerate()
            .map(|(i, l)| LayerDto::from_layer(i, l))
            .collect::<Result<_, _>>()?;
        Ok(Manifest {
            format_version: FORMAT_VERSION,
            canvas: doc.canvas_size.into(),
            output: doc.output_size.into(),
            layout: match doc.layout {
                Layout::Mobile => LayoutDto::Mobile,
                Layout::Desktop => LayoutDto::Desktop,
            },
            dither: DitherDto {
                method: d.method.into(),
                palette: d.palette.into(),
                adjust: AdjustDto {
                    brightness: d.adjust.brightness,
                    contrast: d.adjust.contrast,
                    saturation: d.adjust.saturation,
                },
                bayer_spread: d.bayer_spread,
                strength: d.strength,
                serpentine: d.serpentine,
            },
            fg: hex(doc.fg),
            bg: hex(doc.bg),
            swatches: doc.swatches.iter().map(|&c| hex(c)).collect(),
            layers,
        })
    }

    /// A document with this manifest's settings and no layers. The caller
    /// rebuilds the layers (it has to read their pixels).
    pub fn to_empty_document(&self) -> Result<Document, FormatError> {
        let mut doc = Document::new(self.canvas.into());
        doc.output_size = self.output.into();
        doc.layout = match self.layout {
            LayoutDto::Mobile => Layout::Mobile,
            LayoutDto::Desktop => Layout::Desktop,
        };
        let d = &self.dither;
        doc.dither = DitherSettings {
            method: d.method.into(),
            palette: d.palette.to_choice()?,
            adjust: PreAdjust {
                brightness: d.adjust.brightness,
                contrast: d.adjust.contrast,
                saturation: d.adjust.saturation,
            },
            bayer_spread: d.bayer_spread,
            strength: d.strength,
            serpentine: d.serpentine,
        };
        doc.fg = parse_hex(&self.fg)?;
        doc.bg = parse_hex(&self.bg)?;
        doc.swatches = self
            .swatches
            .iter()
            .map(|s| parse_hex(s))
            .collect::<Result<_, _>>()?;
        Ok(doc)
    }
}

impl LayerDto {
    fn from_layer(index: usize, l: &Layer) -> Result<Self, FormatError> {
        let content = match &l.kind {
            LayerKind::Paint(p) => ContentDto::Paint {
                pixels: layer_entry(index),
                offset_x: p.offset_x,
                offset_y: p.offset_y,
            },
            LayerKind::Image(img) => {
                let t = img.transform;
                let field = |f: &str| format!("layers[{index}].transform.{f}");
                finite(&field("translate_x"), t.translate_x)?;
                finite(&field("translate_y"), t.translate_y)?;
                finite(&field("scale_x"), t.scale_x)?;
                finite(&field("scale_y"), t.scale_y)?;
                finite(&field("rotation"), t.rotation)?;
                ContentDto::Image {
                    source: img.source.as_str().to_owned(),
                    transform: t.into(),
                }
            }
        };
        Ok(LayerDto {
            name: l.name.clone(),
            visible: l.visible,
            opacity: l.opacity.min(100),
            blend: l.blend.name().to_owned(),
            lock: LockDto {
                pixels: l.lock.pixels,
                position: l.lock.position,
            },
            mask: l.mask.as_ref().map(|_| mask_entry(index)),
            content,
        })
    }

    /// The layer's shared properties, applied to a layer of the right kind
    /// built by the caller.
    pub fn apply_common(&self, layer: &mut Layer) -> Result<(), FormatError> {
        if self.opacity > 100 {
            return Err(FormatError::InvalidManifest(format!(
                "layer `{}` opacity {} is over 100",
                self.name, self.opacity
            )));
        }
        layer.visible = self.visible;
        layer.opacity = self.opacity;
        layer.blend = BlendMode::from_name(&self.blend).ok_or_else(|| {
            FormatError::InvalidManifest(format!("unknown blend mode `{}`", self.blend))
        })?;
        layer.lock = Lock {
            pixels: self.lock.pixels,
            position: self.lock.position,
        };
        Ok(())
    }
}

fn finite(field: &str, v: f32) -> Result<(), FormatError> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(FormatError::NonFinite {
            field: field.to_owned(),
        })
    }
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn parse_hex(s: &str) -> Result<Rgb, FormatError> {
    let bad = || FormatError::InvalidManifest(format!("bad color `{s}`, expected #rrggbb"));
    let digits = s.strip_prefix('#').ok_or_else(bad)?;
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|_| bad());
    Ok(Rgb::new(byte(0)?, byte(2)?, byte(4)?))
}

// ---- small conversions ------------------------------------------------------

impl From<Size> for SizeDto {
    fn from(s: Size) -> Self {
        SizeDto {
            width: s.width,
            height: s.height,
        }
    }
}

impl From<SizeDto> for Size {
    fn from(s: SizeDto) -> Self {
        Size::new(s.width, s.height)
    }
}

impl From<Transform> for TransformDto {
    fn from(t: Transform) -> Self {
        TransformDto {
            translate_x: t.translate_x,
            translate_y: t.translate_y,
            scale_x: t.scale_x,
            scale_y: t.scale_y,
            rotation: t.rotation,
        }
    }
}

impl From<TransformDto> for Transform {
    fn from(t: TransformDto) -> Self {
        Transform {
            translate_x: t.translate_x,
            translate_y: t.translate_y,
            scale_x: t.scale_x,
            scale_y: t.scale_y,
            rotation: t.rotation,
        }
    }
}

impl From<DitherMethod> for MethodDto {
    fn from(m: DitherMethod) -> Self {
        match m {
            DitherMethod::None => MethodDto::None,
            DitherMethod::Bayer4 => MethodDto::Bayer4,
            DitherMethod::FloydSteinberg => MethodDto::FloydSteinberg,
            DitherMethod::Atkinson => MethodDto::Atkinson,
        }
    }
}

impl From<MethodDto> for DitherMethod {
    fn from(m: MethodDto) -> Self {
        match m {
            MethodDto::None => DitherMethod::None,
            MethodDto::Bayer4 => DitherMethod::Bayer4,
            MethodDto::FloydSteinberg => DitherMethod::FloydSteinberg,
            MethodDto::Atkinson => DitherMethod::Atkinson,
        }
    }
}

impl From<PaletteChoice> for PaletteDto {
    fn from(p: PaletteChoice) -> Self {
        match p {
            PaletteChoice::Full => PaletteDto::Full,
            PaletteChoice::FourColor(m) => PaletteDto::FourColor {
                preset: match m.preset {
                    Preset::Palette0 => PresetDto::Palette0,
                    Preset::Palette1 => PresetDto::Palette1,
                    Preset::Mode5 => PresetDto::Mode5,
                },
                intensity: match m.intensity {
                    Intensity::Low => IntensityDto::Low,
                    Intensity::High => IntensityDto::High,
                },
                background: m.background.into(),
            },
            PaletteChoice::Custom(p) => PaletteDto::Custom {
                colors: p.colors().iter().map(|&c| c.into()).collect(),
            },
        }
    }
}

impl PaletteDto {
    fn to_choice(&self) -> Result<PaletteChoice, FormatError> {
        Ok(match self {
            PaletteDto::Full => PaletteChoice::Full,
            PaletteDto::FourColor {
                preset,
                intensity,
                background,
            } => PaletteChoice::FourColor(FourColor {
                preset: match preset {
                    PresetDto::Palette0 => Preset::Palette0,
                    PresetDto::Palette1 => Preset::Palette1,
                    PresetDto::Mode5 => Preset::Mode5,
                },
                intensity: match intensity {
                    IntensityDto::Low => Intensity::Low,
                    IntensityDto::High => Intensity::High,
                },
                background: (*background).into(),
            }),
            PaletteDto::Custom { colors } => {
                let palette = Palette::new(colors.iter().map(|&c| CgaColor::from(c)))
                    .map_err(|e| FormatError::InvalidManifest(format!("custom palette: {e}")))?;
                PaletteChoice::Custom(palette)
            }
        })
    }
}

const CGA_DTO: [CgaDto; 16] = [
    CgaDto::Black,
    CgaDto::DarkGray,
    CgaDto::LightGray,
    CgaDto::White,
    CgaDto::Blue,
    CgaDto::LightBlue,
    CgaDto::Green,
    CgaDto::LightGreen,
    CgaDto::Cyan,
    CgaDto::LightCyan,
    CgaDto::Red,
    CgaDto::LightRed,
    CgaDto::Magenta,
    CgaDto::LightMagenta,
    CgaDto::Brown,
    CgaDto::Yellow,
];

impl From<CgaColor> for CgaDto {
    fn from(c: CgaColor) -> Self {
        CGA_DTO[c.index() as usize]
    }
}

impl From<CgaDto> for CgaColor {
    fn from(c: CgaDto) -> Self {
        CgaColor::ALL[c as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cga_names_follow_doc_order() {
        for c in CgaColor::ALL {
            let dto = CgaDto::from(c);
            assert_eq!(CgaColor::from(dto), c);
            // Names on disk are snake_case of the doc-order color names.
            let name = serde_json::to_string(&dto).unwrap();
            let debug = format!("{c:?}");
            let snake: String = debug
                .chars()
                .enumerate()
                .flat_map(|(i, ch)| {
                    let lower = ch.to_ascii_lowercase();
                    if ch.is_uppercase() && i > 0 {
                        vec!['_', lower]
                    } else {
                        vec![lower]
                    }
                })
                .collect();
            assert_eq!(name, format!("\"{snake}\""));
        }
    }

    #[test]
    fn hex_colors() {
        assert_eq!(hex(Rgb::new(0xab, 0x01, 0xff)), "#ab01ff");
        assert_eq!(parse_hex("#AB01ff").unwrap(), Rgb::new(0xab, 0x01, 0xff));
        for bad in ["ab01ff", "#ab01f", "#ab01fff", "#gg0000", "#+10000"] {
            assert!(parse_hex(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn f32_survives_json() {
        // Shortest-repr f32 → JSON → f64 → f32 is exact; check awkward values.
        let mut x: u32 = 0x1234_5678;
        for _ in 0..10_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let f = f32::from_bits(x);
            if !f.is_finite() {
                continue;
            }
            let s = serde_json::to_string(&f).unwrap();
            let back: f32 = serde_json::from_str(&s).unwrap();
            assert_eq!(back.to_bits(), f.to_bits(), "{f} via {s}");
        }
    }
}
