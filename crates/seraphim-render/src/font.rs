//! Font discovery, fallback and rasterization.
//!
//! Faces are located with `fontdb` (system fonts + fontconfig aliases) and
//! rasterized with `swash`. A terminal needs per-character fallback rather
//! than paragraph shaping, so lookup is: primary face for the requested
//! style → primary regular face → first system face that maps the char,
//! cached per `(char, style)`.
//!
//! Not yet: ligatures (need shaping over same-attribute runs), variable
//! font axes, and procedural box-drawing glyphs.

use std::collections::HashMap;
use std::sync::Arc;

use fontdb::{Database, Family, Query, Style, Weight, ID};
use swash::scale::{image::Content, Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Format, Vector};
use swash::{CacheKey, FontRef};

/// Family names tried, in order, when no family is configured.
pub const DEFAULT_FAMILIES: &[&str] =
    &["JetBrains Mono", "Fira Code", "DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono"];

#[derive(Clone, Debug)]
pub struct FontConfig {
    /// Preferred family; `None` uses [`DEFAULT_FAMILIES`] then generic monospace.
    pub family: Option<String>,
    /// Font size in physical pixels (already multiplied by the scale factor).
    pub size_px: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        FontConfig { family: None, size_px: 15.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontStyle {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl FontStyle {
    pub fn new(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => FontStyle::Regular,
            (true, false) => FontStyle::Bold,
            (false, true) => FontStyle::Italic,
            (true, true) => FontStyle::BoldItalic,
        }
    }
    fn is_bold(self) -> bool {
        matches!(self, FontStyle::Bold | FontStyle::BoldItalic)
    }
}

/// Pixel metrics of one terminal cell, all integers so glyphs land on
/// whole pixels (crisp text, spec §4.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from cell top to the baseline.
    pub baseline: f32,
    /// Underline top, measured from cell top.
    pub underline_y: f32,
    pub stroke: f32,
    /// Strikethrough top, measured from cell top.
    pub strike_y: f32,
}

struct Face {
    data: Arc<Vec<u8>>,
    offset: u32,
    key: CacheKey,
    /// The face has no real bold; embolden synthetically.
    synthetic_bold: bool,
}

impl Face {
    fn font_ref(&self) -> FontRef<'_> {
        FontRef { data: &self.data, offset: self.offset, key: self.key }
    }
}

/// A rasterized glyph, positioned relative to the pen at the baseline.
#[derive(Clone, Debug)]
pub struct RasterGlyph {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// True for color (emoji) bitmaps; false for coverage masks.
    pub color: bool,
    /// RGBA8; masks are stored as white with coverage in alpha.
    pub rgba: Vec<u8>,
}

/// Identifies a glyph in a specific face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub face: u16,
    pub glyph: u16,
    pub bold: bool,
}

pub struct Fonts {
    db: Database,
    faces: Vec<Face>,
    face_ids: HashMap<ID, u16>,
    /// Index into `faces` for each [`FontStyle`].
    primary: [u16; 4],
    fallback: HashMap<(char, FontStyle), Option<GlyphKey>>,
    scale: ScaleContext,
    size_px: f32,
    metrics: CellMetrics,
    pub family_name: String,
}

#[derive(Debug)]
pub struct FontError(pub String);

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for FontError {}

impl Fonts {
    pub fn new(config: &FontConfig) -> Result<Fonts, FontError> {
        let mut db = Database::new();
        db.load_system_fonts();
        Self::with_database(db, config)
    }

    pub fn with_database(db: Database, config: &FontConfig) -> Result<Fonts, FontError> {
        let mut fonts = Fonts {
            db,
            faces: Vec::new(),
            face_ids: HashMap::new(),
            primary: [0; 4],
            fallback: HashMap::new(),
            scale: ScaleContext::new(),
            size_px: config.size_px.max(4.0),
            metrics: CellMetrics {
                width: 1.0,
                height: 1.0,
                baseline: 1.0,
                underline_y: 1.0,
                stroke: 1.0,
                strike_y: 1.0,
            },
            family_name: String::new(),
        };

        let names: Vec<String> = match &config.family {
            Some(f) => std::iter::once(f.clone())
                .chain(DEFAULT_FAMILIES.iter().map(|s| s.to_string()))
                .collect(),
            None => DEFAULT_FAMILIES.iter().map(|s| s.to_string()).collect(),
        };
        let mut families: Vec<Family> = names.iter().map(|n| Family::Name(n)).collect();
        families.push(Family::Monospace);

        let find = |db: &Database, weight: Weight, style: Style| {
            db.query(&Query { families: &families, weight, style, ..Query::default() })
        };
        let regular_id = find(&fonts.db, Weight::NORMAL, Style::Normal)
            .ok_or_else(|| FontError("no monospace font found on this system".into()))?;
        fonts.family_name = fonts
            .db
            .face(regular_id)
            .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
            .unwrap_or_default();

        // Only accept styled variants from the same family as the regular
        // face; mixing families looks broken.
        let family_name = fonts.family_name.clone();
        let same_family = |db: &Database, id: ID| {
            db.face(id).is_some_and(|f| f.families.iter().any(|(n, _)| *n == family_name))
        };
        let regular = fonts.load_face(regular_id, false)?;
        let pick = |weight, style, fonts: &mut Fonts| -> Result<u16, FontError> {
            match find(&fonts.db, weight, style) {
                Some(id) if id != regular_id && same_family(&fonts.db, id) => {
                    let wanted_bold = weight == Weight::BOLD;
                    let actual_bold =
                        fonts.db.face(id).is_some_and(|f| f.weight.0 >= Weight::SEMIBOLD.0);
                    fonts.load_face(id, wanted_bold && !actual_bold)
                }
                _ => Ok(regular),
            }
        };
        let bold = pick(Weight::BOLD, Style::Normal, &mut fonts)?;
        let italic = pick(Weight::NORMAL, Style::Italic, &mut fonts)?;
        let bold_italic = pick(Weight::BOLD, Style::Italic, &mut fonts)?;
        if bold == regular {
            // Same face, but bold text must still look bold.
            fonts.faces.push(Face {
                data: fonts.faces[regular as usize].data.clone(),
                offset: fonts.faces[regular as usize].offset,
                key: CacheKey::new(),
                synthetic_bold: true,
            });
            fonts.primary = [regular, fonts.faces.len() as u16 - 1, italic, bold_italic];
        } else {
            fonts.primary = [regular, bold, italic, bold_italic];
        }
        fonts.compute_metrics();
        Ok(fonts)
    }

    fn load_face(&mut self, id: ID, synthetic_bold: bool) -> Result<u16, FontError> {
        if !synthetic_bold {
            if let Some(&i) = self.face_ids.get(&id) {
                return Ok(i);
            }
        }
        let (data, index) = self
            .db
            .with_face_data(id, |data, index| (data.to_vec(), index))
            .ok_or_else(|| FontError("could not read font data".into()))?;
        let font = FontRef::from_index(&data, index as usize)
            .ok_or_else(|| FontError("unsupported font file".into()))?;
        let (offset, key) = (font.offset, font.key);
        self.faces.push(Face { data: Arc::new(data), offset, key, synthetic_bold });
        let i = (self.faces.len() - 1) as u16;
        if !synthetic_bold {
            self.face_ids.insert(id, i);
        }
        Ok(i)
    }

    fn compute_metrics(&mut self) {
        let face = &self.faces[self.primary[0] as usize];
        let font = face.font_ref();
        let m = font.metrics(&[]).scale(self.size_px);
        let gm = font.glyph_metrics(&[]).scale(self.size_px);
        let advance = gm.advance_width(font.charmap().map('M'));
        let width = advance.round().max(1.0);
        let ascent = m.ascent.round();
        let descent = m.descent.abs().round();
        let height = (ascent + descent + m.leading.max(0.0).round()).max(1.0);
        // Spread any leading evenly above and below.
        let baseline = ((height - ascent - descent) / 2.0).floor() + ascent;
        let stroke = m.stroke_size.round().max(1.0);
        let underline_y = (baseline - m.underline_offset.round()).min(height - stroke);
        let strike_y = (baseline - (m.x_height / 2.0).round()).max(0.0);
        self.metrics = CellMetrics { width, height, baseline, underline_y, stroke, strike_y };
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn size_px(&self) -> f32 {
        self.size_px
    }

    /// Find the face and glyph for `ch` in `style`, falling back across
    /// system fonts. `None` means no installed font covers `ch`.
    pub fn lookup(&mut self, ch: char, style: FontStyle) -> Option<GlyphKey> {
        let mk = |face: u16, glyph: u16, synthetic: bool| GlyphKey { face, glyph, bold: synthetic };
        let primary = self.primary[style as usize];
        let face = &self.faces[primary as usize];
        let g = face.font_ref().charmap().map(ch);
        if g != 0 {
            return Some(mk(primary, g, face.synthetic_bold));
        }
        if let Some(hit) = self.fallback.get(&(ch, style)) {
            return *hit;
        }
        let hit = self.find_fallback(ch, style.is_bold());
        self.fallback.insert((ch, style), hit);
        hit
    }

    fn find_fallback(&mut self, ch: char, bold: bool) -> Option<GlyphKey> {
        let regular = self.primary[0];
        let g = self.faces[regular as usize].font_ref().charmap().map(ch);
        if g != 0 {
            return Some(GlyphKey { face: regular, glyph: g, bold });
        }
        // Prefer monospaced faces, then anything (emoji, CJK, symbols).
        let mut candidates: Vec<(bool, ID)> =
            self.db.faces().map(|f| (!f.monospaced, f.id)).collect();
        candidates.sort_by_key(|c| c.0);
        for (_, id) in candidates {
            let glyph = self
                .db
                .with_face_data(id, |data, index| {
                    FontRef::from_index(data, index as usize).map(|f| f.charmap().map(ch))
                })
                .flatten()
                .unwrap_or(0);
            if glyph != 0 {
                let face = self.load_face(id, false).ok()?;
                return Some(GlyphKey { face, glyph, bold });
            }
        }
        None
    }

    /// Rasterize a glyph at the configured size.
    pub fn rasterize(&mut self, key: GlyphKey) -> Option<RasterGlyph> {
        let face = &self.faces[key.face as usize];
        let font = face.font_ref();
        let mut scaler = self.scale.builder(font).size(self.size_px).hint(true).build();
        let sources =
            [Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline];
        let mut render = Render::new(&sources);
        render.format(Format::Alpha).offset(Vector::new(0.0, 0.0));
        if key.bold || face.synthetic_bold {
            render.embolden((self.size_px / 28.0).max(0.6));
        }
        let image = render.render(&mut scaler, key.glyph)?;
        let (w, h) = (image.placement.width, image.placement.height);
        if w == 0 || h == 0 {
            return Some(RasterGlyph {
                left: 0,
                top: 0,
                width: 0,
                height: 0,
                color: false,
                rgba: Vec::new(),
            });
        }
        let (color, rgba) = match image.content {
            Content::Mask => (false, image.data.iter().flat_map(|&a| [255, 255, 255, a]).collect()),
            Content::SubpixelMask => (
                false,
                image
                    .data
                    .chunks_exact(4)
                    .flat_map(|p| [255, 255, 255, p[0].max(p[1]).max(p[2])])
                    .collect(),
            ),
            Content::Color => (true, image.data),
        };
        Some(RasterGlyph {
            left: image.placement.left,
            top: image.placement.top,
            width: w,
            height: h,
            color,
            rgba,
        })
    }
}
