//! Font handling for Tuzminal: discovery, metrics, shaping and rasterization.
//!
//! # The pipeline
//!
//! ```text
//!   config.font ──► fontdb ──► loaded faces (regular/bold/italic + fallbacks)
//!                                   │
//!            text run ──► rustybuzz shaping ──► glyph ids + advances
//!                                   │
//!                     swash rasterization ──► RGBA bitmap ──► atlas
//! ```
//!
//! # Why shaping at all
//!
//! A terminal is a fixed grid, so it is tempting to map each `char` to a glyph and
//! stop. That breaks on:
//!
//! - **ligatures** (`=>` in Fira Code) which need `calt`/`liga` applied to a run;
//! - **combining marks**, where `e` + U+0301 must render as a single é;
//! - **fonts with no direct cmap entry** for a character, needing a fallback face.
//!
//! Shaping is therefore done per run, with a cache keyed on the run's text and
//! style, because re-shaping unchanged text every frame would dominate frame time.

pub mod atlas;

use atlas::{Atlas, AtlasError, AtlasRect, BYTES_PER_PIXEL};
use std::collections::HashMap;
use std::sync::Arc;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Format, Vector};
use swash::{FontRef, GlyphId};
use tuz_config::Font as FontConfig;

/// Which of the four style slots a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Style {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl Style {
    pub fn new(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Style::Regular,
            (true, false) => Style::Bold,
            (false, true) => Style::Italic,
            (true, true) => Style::BoldItalic,
        }
    }

    fn is_bold(self) -> bool {
        matches!(self, Style::Bold | Style::BoldItalic)
    }
    fn is_italic(self) -> bool {
        matches!(self, Style::Italic | Style::BoldItalic)
    }
}

/// Index into the font system's loaded faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontId(pub u16);

/// Cell geometry and decoration positions derived from the primary font.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    /// Advance width of one cell, in physical pixels.
    pub width: u32,
    /// Line height of one cell, in physical pixels.
    pub height: u32,
    /// Distance from the cell top to the text baseline.
    pub ascent: f32,
    pub descent: f32,
    /// Offset below the baseline where an underline is drawn.
    pub underline_offset: f32,
    pub underline_thickness: f32,
    /// Offset above the baseline for a strikethrough.
    pub strikeout_offset: f32,
    pub strikeout_thickness: f32,
}

impl CellMetrics {
    /// The baseline's y offset within the cell.
    pub fn baseline(&self) -> f32 {
        self.ascent
    }
}

/// A glyph placed by the shaper, in cell-relative pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    pub font: FontId,
    pub glyph: GlyphId,
    /// Byte offset into the run that produced this glyph, for mapping glyphs back
    /// to cells when a ligature spans several columns.
    pub cluster: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

/// A rasterized glyph living in the atlas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterizedGlyph {
    /// Where it sits in the atlas. Zero-sized for glyphs that draw nothing.
    pub rect: AtlasRect,
    pub uv: [f32; 4],
    /// Offset from the pen position to the bitmap's top-left corner.
    pub left: f32,
    pub top: f32,
    /// True for color glyphs (emoji): the shader must use the texture's own color
    /// instead of tinting it with the cell's foreground.
    pub color: bool,
}

impl RasterizedGlyph {
    /// True when there is nothing to draw, e.g. a space.
    pub fn is_blank(&self) -> bool {
        self.rect.width == 0 || self.rect.height == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: FontId,
    glyph: GlyphId,
    /// Size in 1/64ths of a pixel, so the key stays hashable and two visually
    /// identical sizes share a cache entry.
    size: u32,
    /// Synthetic emboldening was applied, which changes the bitmap.
    synthetic_bold: bool,
}

/// One loaded font face. The bytes are owned so `FontRef` and `rustybuzz::Face`
/// can borrow them for as long as the system lives.
/// Outcome of searching the already-loaded faces for a character.
enum Loaded {
    /// A face draws it.
    Drawn((FontId, GlyphId)),
    /// A face maps it, but to an empty glyph. Kept only as a last resort.
    Blank((FontId, GlyphId)),
    /// No loaded face maps it at all.
    Missing,
}

struct Face {
    data: Arc<Vec<u8>>,
    index: u32,
    family: String,
}

impl Face {
    fn font_ref(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.data, self.index as usize)
    }

    fn rustybuzz_face(&self) -> Option<rustybuzz::Face<'_>> {
        rustybuzz::Face::from_slice(&self.data, self.index)
    }
}

/// Owns loaded faces, the shaping caches and the glyph atlas.
pub struct FontSystem {
    faces: Vec<Face>,
    /// The four style slots, resolved at load time.
    styles: [FontId; 4],
    /// Extra faces searched when the styled face lacks a glyph.
    fallbacks: Vec<FontId>,

    size_px: f32,
    metrics: CellMetrics,
    ligatures: bool,
    features: Vec<rustybuzz::Feature>,
    synthetic_bold: bool,
    synthetic_italic: bool,

    atlas: Atlas,
    glyphs: HashMap<GlyphKey, RasterizedGlyph>,
    shape_cache: HashMap<(Style, String), Arc<Vec<ShapedGlyph>>>,
    scale: ScaleContext,

    /// Kept alive after construction so a character none of the configured fonts
    /// covers can still be found by scanning every installed face.
    db: fontdb::Database,
    /// Results of those scans, misses included. Without caching the misses, a
    /// single unmappable character would rescan the whole font database on every
    /// frame it is visible.
    system_cache: HashMap<char, Option<FontId>>,
}

/// Initial atlas edge length in pixels. Large enough for a full Latin set at
/// typical sizes without a reset, small enough to be a trivial allocation.
const ATLAS_SIZE: u32 = 1024;

impl FontSystem {
    /// Load the configured fonts at `scale_factor` (HiDPI multiplier).
    pub fn new(cfg: &FontConfig, scale_factor: f64) -> Result<Self, FontError> {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();

        let size_px = (cfg.size * scale_factor as f32).max(1.0);

        let mut faces: Vec<Face> = Vec::new();
        let mut styles = [FontId(0); 4];

        for (slot, style) in [
            Style::Regular,
            Style::Bold,
            Style::Italic,
            Style::BoldItalic,
        ]
        .into_iter()
        .enumerate()
        {
            // A per-style family override wins; otherwise ask for the primary
            // family in the requested weight and let fontconfig pick the face.
            let family = match style {
                Style::Regular => &cfg.family,
                Style::Bold => cfg.bold_family.as_ref().unwrap_or(&cfg.family),
                Style::Italic => cfg.italic_family.as_ref().unwrap_or(&cfg.family),
                Style::BoldItalic => cfg
                    .bold_italic_family
                    .as_ref()
                    .or(cfg.bold_family.as_ref())
                    .unwrap_or(&cfg.family),
            };

            let id = load_family(&mut faces, &db, family, style)
                // Falling back to the regular slot keeps the terminal usable with
                // a font that ships only one weight, which is common for
                // hand-made bitmap-style fonts.
                .or_else(|| if slot == 0 { None } else { Some(styles[0]) })
                .ok_or_else(|| FontError::FamilyNotFound(family.clone()))?;
            styles[slot] = id;
        }

        // Fallbacks are best-effort: a user's list naming a font they do not have
        // installed should be skipped, not fatal.
        let mut fallbacks = Vec::new();
        for family in &cfg.fallback {
            // Exact match only. Substituting here would make a missing fallback a
            // second copy of the primary font, which cannot supply any glyph the
            // primary lacks — the whole point of a fallback.
            match load_fallback(&mut faces, &db, family) {
                Some(id) => fallbacks.push(id),
                None => log::debug!("fallback font `{family}` is not installed; skipping"),
            }
        }

        let metrics = compute_metrics(&faces[styles[0].0 as usize], size_px, cfg)
            .ok_or_else(|| FontError::NoMetrics(cfg.family.clone()))?;

        log::info!(
            "font: {} at {:.1}px, cell {}x{}, {} fallback(s)",
            faces[styles[0].0 as usize].family,
            size_px,
            metrics.width,
            metrics.height,
            fallbacks.len()
        );

        Ok(Self {
            faces,
            styles,
            fallbacks,
            size_px,
            metrics,
            ligatures: cfg.ligatures,
            features: build_features(cfg),
            synthetic_bold: cfg.synthetic_bold,
            synthetic_italic: cfg.synthetic_italic,
            atlas: Atlas::new(ATLAS_SIZE, ATLAS_SIZE),
            glyphs: HashMap::new(),
            shape_cache: HashMap::new(),
            scale: ScaleContext::new(),
            db,
            system_cache: HashMap::new(),
        })
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }
    pub fn atlas_mut(&mut self) -> &mut Atlas {
        &mut self.atlas
    }
    pub fn size_px(&self) -> f32 {
        self.size_px
    }

    /// Installed families that look monospaced, sorted and deduplicated.
    ///
    /// For the settings font picker. Filtered by the face's own `is_monospace` flag
    /// where available and by name otherwise, because a picker listing every
    /// proportional font on the system would be useless for a terminal.
    pub fn monospace_families(&self) -> Vec<String> {
        let mut names = std::collections::BTreeSet::new();

        for info in self.db.faces() {
            let Some((family, _)) = info.families.first() else {
                continue;
            };
            // The flag is authoritative when a font sets it; the name check catches
            // the ones that do not.
            let looks_mono = info.monospaced || family.to_lowercase().contains("mono");
            if looks_mono {
                names.insert(family.clone());
            }
        }
        names.into_iter().collect()
    }

    fn face(&self, id: FontId) -> &Face {
        &self.faces[id.0 as usize]
    }

    fn style_font(&self, style: Style) -> FontId {
        self.styles[match style {
            Style::Regular => 0,
            Style::Bold => 1,
            Style::Italic => 2,
            Style::BoldItalic => 3,
        }]
    }

    /// Find a face that has a glyph for `c`.
    ///
    /// Four tiers: the styled face, the user's configured fallbacks, the regular
    /// face, and finally **every installed font**. That last tier is what makes
    /// powerline and Nerd Font glyphs appear: they live in fonts nobody thinks to
    /// list, and a terminal that only searches a configured list silently drops
    /// them while every other terminal on the system renders them fine.
    ///
    /// Takes `&mut self` because the system tier may need to load a face.
    pub fn font_for_char(&mut self, c: char, style: Style) -> Option<(FontId, GlyphId)> {
        // A face that maps the character but draws nothing is worse than one that
        // does not claim it at all: it ends the search with a hit and the character
        // silently disappears. So a candidate that rasterizes blank is remembered but
        // not accepted, and the search carries on looking for one that draws.
        let mut blank: Option<(FontId, GlyphId)> = None;

        match self.lookup_loaded(c, style) {
            Loaded::Drawn(hit) => return Some(hit),
            Loaded::Blank(hit) => blank = Some(hit),
            Loaded::Missing => {}
        }

        // A previous scan already answered for this character.
        let found = match self.system_cache.get(&c) {
            Some(cached) => *cached,
            None => {
                let found = self.load_from_system(c);
                self.system_cache.insert(c, found);
                found
            }
        };

        if let Some(id) = found {
            if let Some(glyph) = self.glyph_in(id, c) {
                if self.renders(id, glyph, style) {
                    return Some((id, glyph));
                }
                blank = blank.or(Some((id, glyph)));
            }
        }

        // Nothing anywhere draws it. Return the blank hit rather than `None` so a
        // genuinely empty character — a space, U+00A0 — still measures and advances
        // like the character it is.
        blank
    }

    /// The family name a face was loaded from. For diagnostics.
    pub fn family_of(&self, id: FontId) -> &str {
        self.faces
            .get(id.0 as usize)
            .map(|f| f.family.as_str())
            .unwrap_or("?")
    }

    /// Whether this glyph actually puts pixels on the screen.
    ///
    /// Rasterizing to answer looks expensive, but the result is cached in
    /// `self.glyphs` and the caller is about to rasterize it anyway.
    fn renders(&mut self, font: FontId, glyph: GlyphId, style: Style) -> bool {
        self.rasterize(font, glyph, style)
            .map(|g| !g.is_blank())
            .unwrap_or(false)
    }

    /// Search only the faces already loaded, which is the hot path.
    fn lookup_loaded(&mut self, c: char, style: Style) -> Loaded {
        let primary = self.style_font(style);
        let candidates: Vec<FontId> = std::iter::once(primary)
            .chain(self.fallbacks.iter().copied())
            .chain(std::iter::once(self.styles[0]))
            .collect();

        let mut blank = None;
        for id in candidates {
            let Some(glyph) = self.glyph_in(id, c) else {
                continue;
            };
            if self.renders(id, glyph, style) {
                return Loaded::Drawn((id, glyph));
            }
            blank = blank.or(Some((id, glyph)));
        }
        match blank {
            Some(hit) => Loaded::Blank(hit),
            None => Loaded::Missing,
        }
    }

    /// The glyph for `c` in an already-loaded face, if it has a real one.
    fn glyph_in(&self, id: FontId, c: char) -> Option<GlyphId> {
        let font = self.faces.get(id.0 as usize)?.font_ref()?;
        let glyph = font.charmap().map(c);
        // Glyph 0 is `.notdef`; treating it as a hit renders boxes instead of
        // trying the next candidate.
        (glyph != 0).then_some(glyph)
    }

    /// Scan every installed face for one covering `c`, loading it if found.
    ///
    /// Linear over the font database, which is why the result is cached. Runs at
    /// most once per distinct character.
    fn load_from_system(&mut self, c: char) -> Option<FontId> {
        let mut found: Option<(fontdb::ID, String)> = None;

        for info in self.db.faces() {
            let covered = self
                .db
                .with_face_data(info.id, |data, index| {
                    FontRef::from_index(data, index as usize)
                        .map(|font| font.charmap().map(c) != 0)
                        .unwrap_or(false)
                })
                .unwrap_or(false);

            if covered {
                let family = info
                    .families
                    .first()
                    .map(|(n, _)| n.clone())
                    .unwrap_or_default();
                found = Some((info.id, family));
                break;
            }

        }

        let (id, family) = found?;
        log::debug!("system fallback: U+{:04X} found in `{family}`", c as u32);

        // `push_face` needs `&mut self.faces` while `self.db` is only read, so the
        // scan above has to finish before this.
        let mut faces = std::mem::take(&mut self.faces);
        let result = push_face(&mut faces, &self.db, id, &family);
        self.faces = faces;
        result
    }

    /// Shape a run of text into positioned glyphs.
    ///
    /// Results are cached on `(style, text)`. With ligatures disabled the run is
    /// mapped per character, which is both faster and avoids a font's default
    /// `calt` rules changing glyphs the user did not ask to change.
    pub fn shape(&mut self, text: &str, style: Style) -> Arc<Vec<ShapedGlyph>> {
        let key = (style, text.to_owned());
        if let Some(cached) = self.shape_cache.get(&key) {
            return cached.clone();
        }

        let shaped = if self.ligatures {
            self.shape_with_harfbuzz(text, style)
        } else {
            self.shape_per_char(text, style)
        };

        let shaped = Arc::new(shaped);
        // An unbounded cache would grow without limit on output that never
        // repeats, e.g. a log stream of unique lines.
        if self.shape_cache.len() > 8192 {
            self.shape_cache.clear();
        }
        self.shape_cache.insert(key, shaped.clone());
        shaped
    }

    fn shape_per_char(&mut self, text: &str, style: Style) -> Vec<ShapedGlyph> {
        let advance = self.metrics.width as f32;
        let mut out = Vec::with_capacity(text.len());
        for (offset, c) in text.char_indices() {
            if let Some((font, glyph)) = self.font_for_char(c, style) {
                out.push(ShapedGlyph {
                    font,
                    glyph,
                    cluster: offset as u32,
                    x_advance: advance,
                    x_offset: 0.0,
                    y_offset: 0.0,
                });
            }
        }
        out
    }

    fn shape_with_harfbuzz(&mut self, text: &str, style: Style) -> Vec<ShapedGlyph> {
        let font_id = self.style_font(style);
        let face = self.face(font_id);
        let Some(rb_face) = face.rustybuzz_face() else {
            return self.shape_per_char(text, style);
        };

        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();

        let output = rustybuzz::shape(&rb_face, &self.features, buffer);
        let upem = rb_face.units_per_em() as f32;
        let scale = self.size_px / upem;

        let infos = output.glyph_infos();
        let positions = output.glyph_positions();
        let advance = self.metrics.width as f32;

        let mut out = Vec::with_capacity(infos.len());
        // Clusters this face could not render. Resolved after the loop, because
        // `rb_face` borrows the face data and fallback lookup needs `&mut self`.
        let mut needs_fallback: Vec<usize> = Vec::new();

        for (info, pos) in infos.iter().zip(positions.iter()) {
            if info.glyph_id == 0 {
                needs_fallback.push(out.len());
            }
            out.push(ShapedGlyph {
                font: font_id,
                glyph: info.glyph_id as GlyphId,
                cluster: info.cluster,
                x_advance: pos.x_advance as f32 * scale,
                x_offset: pos.x_offset as f32 * scale,
                y_offset: pos.y_offset as f32 * scale,
            });
        }
        drop(output);
        drop(rb_face);

        // Replace each .notdef with a fallback glyph, or drop it: drawing .notdef
        // shows a box where another font has the real character.
        for index in needs_fallback.into_iter().rev() {
            let cluster = out[index].cluster as usize;
            let resolved = text
                .get(cluster..)
                .and_then(|rest| rest.chars().next())
                .and_then(|c| self.font_for_char(c, style));

            match resolved {
                Some((font, glyph)) => {
                    out[index].font = font;
                    out[index].glyph = glyph;
                    out[index].x_advance = advance;
                    out[index].x_offset = 0.0;
                    out[index].y_offset = 0.0;
                }
                None => {
                    out.remove(index);
                }
            }
        }
        out
    }

    /// Rasterize a glyph and place it in the atlas, or return the cached entry.
    ///
    /// Returns `None` only if the glyph cannot be rendered at all.
    pub fn rasterize(
        &mut self,
        font: FontId,
        glyph: GlyphId,
        style: Style,
    ) -> Option<RasterizedGlyph> {
        // Synthetic bold applies when the requested style is bold but the face we
        // resolved to is the regular one — i.e. no real bold face existed.
        let needs_synthetic_bold = self.synthetic_bold && style.is_bold() && font == self.styles[0];

        let key = GlyphKey {
            font,
            glyph,
            size: (self.size_px * 64.0) as u32,
            synthetic_bold: needs_synthetic_bold,
        };
        if let Some(cached) = self.glyphs.get(&key) {
            return Some(*cached);
        }

        let entry = self.render_glyph(font, glyph, style, needs_synthetic_bold)?;
        self.glyphs.insert(key, entry);
        Some(entry)
    }

    fn render_glyph(
        &mut self,
        font_id: FontId,
        glyph: GlyphId,
        style: Style,
        synthetic_bold: bool,
    ) -> Option<RasterizedGlyph> {
        let face = &self.faces[font_id.0 as usize];
        let data = face.data.clone();
        let index = face.index;
        let font = FontRef::from_index(&data, index as usize)?;

        let synthetic_italic =
            self.synthetic_italic && style.is_italic() && font_id == self.styles[0];

        let mut builder = self.scale.builder(font);
        builder = builder.size(self.size_px).hint(true);
        let mut scaler = builder.build();

        // Source order matters: try color outlines and bitmaps first so emoji come
        // out in color, and only fall back to a monochrome outline.
        let image = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ])
        // Always grayscale coverage. Subpixel (LCD) antialiasing would need
        // three-channel coverage in the atlas plus a dual-source blend in the
        // shader; until that exists, `grayscale_antialiasing` has nothing to
        // switch off, and branching here would only imply otherwise.
        .format(Format::Alpha)
        .offset(Vector::new(0.0, 0.0))
        .render(&mut scaler, glyph)?;

        let w = image.placement.width;
        let h = image.placement.height;

        // Convert whatever swash produced into RGBA so one atlas serves both
        // monochrome and color glyphs.
        let is_color = matches!(image.content, swash::scale::image::Content::Color);
        let mut rgba = vec![0u8; (w * h) as usize * BYTES_PER_PIXEL];

        match image.content {
            swash::scale::image::Content::Mask => {
                // Store white with the coverage in alpha; the shader tints it with
                // the cell's foreground color.
                for (i, &a) in image.data.iter().enumerate() {
                    let o = i * 4;
                    if o + 3 < rgba.len() {
                        rgba[o] = 0xff;
                        rgba[o + 1] = 0xff;
                        rgba[o + 2] = 0xff;
                        rgba[o + 3] = a;
                    }
                }
            }
            swash::scale::image::Content::Color | swash::scale::image::Content::SubpixelMask => {
                let n = rgba.len().min(image.data.len());
                rgba[..n].copy_from_slice(&image.data[..n]);
            }
        }

        if synthetic_bold {
            embolden(&mut rgba, w, h);
        }
        if synthetic_italic {
            // Shearing the bitmap is crude next to a real italic face, but it is
            // what the config promises when none exists.
            shear(&mut rgba, w, h);
        }

        let rect = match self.atlas.insert(w, h, &rgba) {
            Ok(r) => r,
            Err(AtlasError::Full) => {
                // Start over: every cached coordinate is now stale, so the glyph
                // cache must go too.
                log::debug!("glyph atlas full; resetting");
                self.atlas.reset();
                self.glyphs.clear();
                self.atlas.insert(w, h, &rgba).ok()?
            }
            Err(AtlasError::TooLarge) => {
                log::warn!("glyph {glyph} is too large for the atlas; skipping");
                return None;
            }
        };

        Some(RasterizedGlyph {
            rect,
            uv: self.atlas.uv(rect),
            left: image.placement.left as f32,
            top: image.placement.top as f32,
            color: is_color,
        })
    }

    /// Discard every cached glyph and shaped run.
    ///
    /// Called when the font configuration changes; the atlas is reset too, so
    /// nothing can reference stale texture coordinates.
    pub fn clear_caches(&mut self) {
        self.glyphs.clear();
        self.shape_cache.clear();
        self.atlas.reset();
    }
}

/// Thicken a glyph bitmap by smearing it one pixel right and down.
///
/// Cheap and good enough at terminal sizes; a real bold face is always preferred
/// and this only runs when none exists.
fn embolden(rgba: &mut [u8], w: u32, h: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let original = rgba.to_vec();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            // Take the maximum alpha of this pixel and its left neighbour.
            let left = if x > 0 {
                original[(y * w as usize + x - 1) * 4 + 3]
            } else {
                0
            };
            rgba[i + 3] = rgba[i + 3].max(left);
        }
    }
}

/// Shear a bitmap horizontally to fake an italic.
fn shear(rgba: &mut [u8], w: u32, h: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let original = rgba.to_vec();
    rgba.fill(0);
    // About 12 degrees, the usual synthetic-oblique angle.
    let slant = 0.21_f32;
    for y in 0..h as usize {
        // Shift more at the top, which is where an italic leans.
        let shift = ((h as usize - 1 - y) as f32 * slant).round() as usize;
        for x in 0..w as usize {
            let sx = x + shift;
            if sx >= w as usize {
                continue;
            }
            let src = (y * w as usize + x) * 4;
            let dst = (y * w as usize + sx) * 4;
            rgba[dst..dst + 4].copy_from_slice(&original[src..src + 4]);
        }
    }
}

/// Turn the config's feature settings into rustybuzz features.
fn build_features(cfg: &FontConfig) -> Vec<rustybuzz::Feature> {
    let mut features = Vec::new();

    // Ligatures are opt-in, so when disabled they are switched *off* explicitly:
    // many programming fonts enable `calt` by default.
    let liga_value = u32::from(cfg.ligatures);
    for tag in [b"liga", b"calt", b"clig"] {
        features.push(rustybuzz::Feature::new(
            rustybuzz::ttf_parser::Tag::from_bytes(tag),
            liga_value,
            ..,
        ));
    }

    for (tag, value) in &cfg.features {
        let bytes = tag.as_bytes();
        if bytes.len() == 4 {
            let tag =
                rustybuzz::ttf_parser::Tag::from_bytes(&[bytes[0], bytes[1], bytes[2], bytes[3]]);
            features.push(rustybuzz::Feature::new(tag, *value, ..));
        }
    }
    features
}

/// Generic family names that mean "whatever the system monospace font is".
///
/// `fontdb` does not implement fontconfig's alias resolution, so a literal query
/// for `monospace` finds nothing on a typical Linux system even though
/// `fc-match monospace` resolves fine. These names are therefore expanded here.
const GENERIC_MONOSPACE: &[&str] = &["monospace", "mono", "fixed", "courier"];

/// Monospace families to try, in order, when the requested one is unavailable.
///
/// Ordered by how likely a user is to want them for a terminal, with the common
/// distro defaults last so they act as a floor rather than a preference.
const PREFERRED_MONOSPACE: &[&str] = &[
    "JetBrains Mono",
    "Fira Code",
    "Cascadia Code",
    "Cascadia Mono",
    "Source Code Pro",
    "Iosevka",
    "Hack",
    "Menlo",
    "Consolas",
    "SF Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Adwaita Mono",
    "Nimbus Mono PS",
    "Courier New",
];

/// Query the database for an exact family name in the requested style.
fn query_exact(db: &fontdb::Database, family: &str, style: Style) -> Option<fontdb::ID> {
    db.query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight: if style.is_bold() {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        },
        style: if style.is_italic() {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        },
        stretch: fontdb::Stretch::Normal,
    })
}

/// Resolve a family name to a concrete face, degrading gracefully.
///
/// Four tiers, in order: the exact name; a known-good monospace family; any
/// installed family whose name contains "mono"; any face at all. The last tier can
/// pick a proportional font, which looks wrong but still renders text — much
/// better than a terminal that shows nothing because of a typo'd family name.
pub fn resolve_family(db: &fontdb::Database, requested: &str, style: Style) -> Option<fontdb::ID> {
    let generic = GENERIC_MONOSPACE.contains(&requested.to_lowercase().as_str());

    if !generic {
        if let Some(id) = query_exact(db, requested, style) {
            return Some(id);
        }
        log::warn!("font family `{requested}` is not installed; looking for a substitute");
    }

    for candidate in PREFERRED_MONOSPACE {
        if let Some(id) = query_exact(db, candidate, style) {
            if !generic {
                log::info!("substituting `{candidate}` for `{requested}`");
            }
            return Some(id);
        }
    }

    // Any family that calls itself monospaced.
    let mono_family = db.faces().find_map(|face| {
        face.families
            .first()
            .map(|(name, _)| name.clone())
            .filter(|name| name.to_lowercase().contains("mono"))
    });
    if let Some(name) = mono_family {
        if let Some(id) = query_exact(db, &name, style) {
            log::info!("falling back to monospace family `{name}`");
            return Some(id);
        }
    }

    // Last resort. Warn loudly: a proportional font in a character grid looks
    // broken, and the user should know why.
    let any = db.faces().next()?;
    log::warn!(
        "no monospace font found; falling back to `{}`, which will not align correctly",
        any.families
            .first()
            .map(|(n, _)| n.as_str())
            .unwrap_or("unknown")
    );
    Some(any.id)
}

/// Load a fallback family by exact name, or nothing.
fn load_fallback(faces: &mut Vec<Face>, db: &fontdb::Database, family: &str) -> Option<FontId> {
    let id = query_exact(db, family, Style::Regular)?;
    push_face(faces, db, id, family)
}

/// Look up a family and push its face, returning the existing id if already loaded.
fn load_family(
    faces: &mut Vec<Face>,
    db: &fontdb::Database,
    family: &str,
    style: Style,
) -> Option<FontId> {
    let id = resolve_family(db, family, style)?;
    push_face(faces, db, id, family)
}

/// Read a face's bytes and add it to the list, deduplicating identical faces.
fn push_face(
    faces: &mut Vec<Face>,
    db: &fontdb::Database,
    id: fontdb::ID,
    requested: &str,
) -> Option<FontId> {
    let (source, index) = db.face_source(id)?;

    let data = match source {
        fontdb::Source::Binary(data) => Arc::new(data.as_ref().as_ref().to_vec()),
        fontdb::Source::File(path) => Arc::new(std::fs::read(&path).ok()?),
        fontdb::Source::SharedFile(_, data) => Arc::new(data.as_ref().as_ref().to_vec()),
    };

    let family_name = db
        .face(id)
        .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
        .unwrap_or_else(|| requested.to_owned());

    // Reuse an already-loaded identical face rather than holding the bytes twice.
    if let Some(existing) = faces
        .iter()
        .position(|f| f.index == index && f.data.len() == data.len() && f.family == family_name)
    {
        return Some(FontId(existing as u16));
    }

    faces.push(Face {
        data,
        index,
        family: family_name,
    });
    Some(FontId((faces.len() - 1) as u16))
}

/// Derive cell geometry from a face's metrics.
fn compute_metrics(face: &Face, size_px: f32, cfg: &FontConfig) -> Option<CellMetrics> {
    let font = face.font_ref()?;
    // `scale(ppem)` divides by units-per-em; `linear_scale` does not. Using the
    // wrong one yields metrics ~1000x too large, which then collapses the grid to
    // 1x1 and panics deep inside the VT library.
    let m = font.metrics(&[]).scale(size_px);

    // Cell width comes from a real advance rather than `average_width`, which is
    // meaningless for a monospace font and often zero.
    let advance = {
        let glyph = font.charmap().map('M');
        let gm = font.glyph_metrics(&[]).scale(size_px);
        let a = gm.advance_width(glyph);
        if a > 0.0 {
            a
        } else if m.average_width > 0.0 {
            m.average_width
        } else {
            // Last resort for a font with no usable metrics at all.
            size_px * 0.6
        }
    };

    let width = (advance * cfg.cell_width).round().max(1.0) as u32;

    // Include leading: a font that specifies line gap looks cramped without it.
    let natural_height = m.ascent + m.descent + m.leading;
    let height = (natural_height * cfg.line_height).round().max(1.0) as u32;

    // Extra line_height is distributed above the baseline so text sits optically
    // centered rather than hugging the top of the cell.
    let extra = height as f32 - natural_height;
    let ascent = m.ascent + extra / 2.0;

    Some(CellMetrics {
        width,
        height,
        ascent,
        descent: m.descent,
        underline_offset: if m.underline_offset != 0.0 {
            -m.underline_offset
        } else {
            m.descent * 0.5
        },
        underline_thickness: m.stroke_size.max(1.0),
        strikeout_offset: if m.strikeout_offset != 0.0 {
            m.strikeout_offset
        } else {
            m.ascent * 0.3
        },
        strikeout_thickness: m.stroke_size.max(1.0),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("no font found for family `{0}`; is any monospace font installed?")]
    FamilyNotFound(String),
    #[error("font `{0}` has unusable metrics")]
    NoMetrics(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generic `monospace` family, which fontconfig resolves on any system
    /// with fonts installed. Tests that need a real face use this.
    fn config() -> FontConfig {
        FontConfig {
            family: "monospace".to_owned(),
            size: 14.0,
            ..FontConfig::default()
        }
    }

    /// A font system over the generic monospace family.
    ///
    /// Panics rather than skipping. An earlier version returned `Option` and every
    /// test began `let Some(sys) = system() else { return }`, so when family
    /// resolution broke the whole suite reported "ok" while testing nothing. A
    /// machine with no fonts installed is a real failure worth seeing.
    fn system() -> FontSystem {
        FontSystem::new(&config(), 1.0).expect(
            "no usable system font: install a monospace font (e.g. DejaVu Sans Mono) to run these tests",
        )
    }

    #[test]
    fn style_maps_bold_and_italic_flags() {
        assert_eq!(Style::new(false, false), Style::Regular);
        assert_eq!(Style::new(true, false), Style::Bold);
        assert_eq!(Style::new(false, true), Style::Italic);
        assert_eq!(Style::new(true, true), Style::BoldItalic);
        assert!(Style::BoldItalic.is_bold() && Style::BoldItalic.is_italic());
        assert!(!Style::Regular.is_bold());
    }

    #[test]
    fn ligature_features_are_explicitly_disabled_when_off() {
        // Many programming fonts enable `calt` by default, so "off" has to be
        // stated rather than merely omitted.
        let mut cfg = config();
        cfg.ligatures = false;
        let features = build_features(&cfg);

        let calt = features
            .iter()
            .find(|f| f.tag == rustybuzz::ttf_parser::Tag::from_bytes(b"calt"))
            .expect("calt should be listed");
        assert_eq!(calt.value, 0);

        cfg.ligatures = true;
        let features = build_features(&cfg);
        let calt = features
            .iter()
            .find(|f| f.tag == rustybuzz::ttf_parser::Tag::from_bytes(b"calt"))
            .unwrap();
        assert_eq!(calt.value, 1);
    }

    #[test]
    fn custom_features_are_forwarded_and_bad_tags_ignored() {
        let mut cfg = config();
        cfg.features.insert("ss01".to_owned(), 1);
        cfg.features.insert("toolong".to_owned(), 1);

        let features = build_features(&cfg);
        assert!(features
            .iter()
            .any(|f| f.tag == rustybuzz::ttf_parser::Tag::from_bytes(b"ss01")));
        // The malformed tag must not panic or produce a bogus 4-byte tag.
        assert_eq!(features.len(), 3 + 1);
    }

    #[test]
    fn a_missing_family_falls_back_rather_than_failing() {
        // fontconfig resolves the generic Monospace fallback, so a typo in the
        // family name still yields a working terminal.
        let cfg = FontConfig {
            family: "ThisFontDoesNotExistAnywhere12345".to_owned(),
            ..config()
        };
        let sys =
            FontSystem::new(&cfg, 1.0).expect("a nonexistent family must fall back, not fail");
        assert!(sys.metrics().width > 0);
    }

    #[test]
    fn metrics_are_positive_and_plausible() {
        let sys = system();
        let m = sys.metrics();
        let px = sys.size_px();

        assert!(m.width > 0 && m.height > 0);
        // A cell taller than it is wide is the norm for monospace text.
        assert!(m.height > m.width, "cell {}x{}", m.width, m.height);
        assert!(m.ascent > 0.0);
        assert!(m.baseline() <= m.height as f32);
        assert!(m.underline_thickness >= 1.0);

        // Absolute bounds against the pixel size. These exist because an earlier
        // version scaled by font units instead of dividing by units-per-em and
        // produced a 9600x20112 cell — which still satisfied `height > width`, so
        // the weaker assertion above passed while the terminal was unusable.
        assert!(
            (0.3 * px..=1.2 * px).contains(&(m.width as f32)),
            "cell width {} is implausible for {px}px text",
            m.width
        );
        assert!(
            (0.8 * px..=2.5 * px).contains(&(m.height as f32)),
            "cell height {} is implausible for {px}px text",
            m.height
        );
    }

    #[test]
    fn scale_factor_multiplies_the_pixel_size() {
        let one = system();
        let two = FontSystem::new(&config(), 2.0).expect("should load at 2x");

        assert!((two.size_px() - one.size_px() * 2.0).abs() < 0.01);
        // A HiDPI cell must actually be bigger, or text is tiny on a scaled display.
        assert!(two.metrics().width > one.metrics().width);
    }

    #[test]
    fn line_height_and_cell_width_multipliers_apply() {
        let base = system();

        let mut cfg = config();
        cfg.line_height = 2.0;
        cfg.cell_width = 2.0;
        let stretched = FontSystem::new(&cfg, 1.0).expect("should load stretched");

        assert!(stretched.metrics().height > base.metrics().height);
        assert!(stretched.metrics().width > base.metrics().width);
    }

    #[test]
    fn ascii_characters_resolve_to_real_glyphs() {
        let mut sys = system();
        for c in ['a', 'Z', '0', '#', '~'] {
            let (_, glyph) = sys
                .font_for_char(c, Style::Regular)
                .unwrap_or_else(|| panic!("no glyph for {c:?}"));
            assert_ne!(glyph, 0, "{c:?} resolved to .notdef");
        }
    }

    #[test]
    fn an_unmappable_character_reports_no_glyph() {
        let mut sys = system();
        // A private-use codepoint no ordinary font covers.
        assert!(sys.font_for_char('\u{10FFFD}', Style::Regular).is_none());
    }

    #[test]
    fn shaping_produces_one_glyph_per_character_without_ligatures() {
        let mut sys = system();
        let shaped = sys.shape("abc", Style::Regular);
        assert_eq!(shaped.len(), 3);
        assert_eq!(shaped[0].cluster, 0);
        assert_eq!(shaped[2].cluster, 2);
    }

    #[test]
    fn shaped_runs_are_cached() {
        let mut sys = system();
        let first = sys.shape("hello", Style::Regular);
        let second = sys.shape("hello", Style::Regular);
        // Same allocation means the cache was hit, not just an equal result.
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn different_styles_cache_separately() {
        let mut sys = system();
        let regular = sys.shape("x", Style::Regular);
        let bold = sys.shape("x", Style::Bold);
        assert!(!Arc::ptr_eq(&regular, &bold));
    }

    #[test]
    fn rasterizing_puts_a_glyph_in_the_atlas() {
        let mut sys = system();

        // Measured across the resolve, not after it: `font_for_char` rasterizes each
        // candidate to check it is not blank, so by the time it returns the glyph is
        // already cached and a second `rasterize` adds nothing.
        let before = sys.atlas().len();
        let (font, glyph) = sys.font_for_char('M', Style::Regular).unwrap();
        let g = sys
            .rasterize(font, glyph, Style::Regular)
            .expect("should rasterize");

        assert!(!g.is_blank(), "'M' should have pixels");
        assert!(sys.atlas().len() > before);
        // UV coordinates must be inside the texture.
        assert!(g.uv.iter().all(|&v| (0.0..=1.0).contains(&v)));
        assert!(g.uv[2] > g.uv[0] && g.uv[3] > g.uv[1]);
    }

    #[test]
    fn rasterizing_the_same_glyph_twice_reuses_the_cache() {
        let mut sys = system();
        let (font, glyph) = sys.font_for_char('W', Style::Regular).unwrap();

        let first = sys.rasterize(font, glyph, Style::Regular).unwrap();
        let count = sys.atlas().len();
        let second = sys.rasterize(font, glyph, Style::Regular).unwrap();

        assert_eq!(first.rect, second.rect);
        assert_eq!(sys.atlas().len(), count, "must not re-pack a cached glyph");
    }

    #[test]
    fn a_space_rasterizes_to_nothing_drawable() {
        let mut sys = system();
        if let Some((font, glyph)) = sys.font_for_char(' ', Style::Regular) {
            let g = sys.rasterize(font, glyph, Style::Regular).unwrap();
            assert!(g.is_blank(), "a space should have no pixels");
        }
    }

    #[test]
    fn clearing_caches_empties_the_atlas_too() {
        // Keeping glyph entries after an atlas reset would leave them pointing at
        // pixels that no longer exist.
        let mut sys = system();
        let (font, glyph) = sys.font_for_char('A', Style::Regular).unwrap();
        sys.rasterize(font, glyph, Style::Regular);
        sys.shape("cached", Style::Regular);
        assert!(!sys.atlas().is_empty());

        sys.clear_caches();
        assert!(sys.atlas().is_empty());
        assert!(sys.glyphs.is_empty());
        assert!(sys.shape_cache.is_empty());
    }

    #[test]
    fn embolden_only_adds_coverage() {
        // Smearing must never erase existing coverage, or bold text develops holes.
        let w = 4;
        let h = 2;
        let mut rgba = vec![0u8; (w * h) as usize * 4];
        // One opaque pixel at (1,0).
        let row0 = 0usize;
        rgba[(row0 * w as usize + 1) * 4 + 3] = 0xff;
        let before = rgba.clone();

        embolden(&mut rgba, w, h);

        for i in (3..rgba.len()).step_by(4) {
            assert!(rgba[i] >= before[i], "alpha decreased at {i}");
        }
        // And the pixel to the right of the original gained coverage.
        assert_eq!(rgba[(row0 * w as usize + 2) * 4 + 3], 0xff);
    }

    #[test]
    fn shear_moves_pixels_right_at_the_top() {
        let w = 8;
        let h = 4;
        let mut rgba = vec![0u8; (w * h) as usize * 4];
        // A vertical line at x=0.
        for y in 0..h as usize {
            rgba[(y * w as usize) * 4 + 3] = 0xff;
        }

        shear(&mut rgba, w, h);

        let alpha = |x: usize, y: usize| rgba[(y * w as usize + x) * 4 + 3];
        // Bottom row stays put, top row leans right.
        assert_eq!(alpha(0, h as usize - 1), 0xff);
        assert_eq!(alpha(0, 0), 0, "the top of the stem should have moved");
        assert!(
            (1..w as usize).any(|x| alpha(x, 0) == 0xff),
            "top row pixel should exist further right"
        );
    }

    #[test]
    fn empty_bitmaps_survive_the_synthetic_transforms() {
        // Zero-sized glyphs are common (spaces); these must not panic or index
        // out of bounds.
        let mut empty: Vec<u8> = Vec::new();
        embolden(&mut empty, 0, 0);
        shear(&mut empty, 0, 0);
        assert!(empty.is_empty());
    }
}

#[cfg(test)]
mod system_fallback_tests {
    use super::*;

    fn system() -> FontSystem {
        FontSystem::new(
            &FontConfig {
                family: "monospace".to_owned(),
                size: 14.0,
                // Deliberately empty: the point is that system fallback works even
                // when the user has configured nothing.
                fallback: Vec::new(),
                ..FontConfig::default()
            },
            1.0,
        )
        .expect("a monospace font is required for these tests")
    }

    #[test]
    fn powerline_glyphs_resolve_even_with_no_configured_fallbacks() {
        // The bug this covers: prompts built with Starship/powerline draw a branch
        // symbol and separators from the Private Use Area. Searching only the
        // configured fallback list left them blank, while every other terminal on
        // the machine rendered them via full fontconfig fallback.
        let mut sys = system();

        // U+E0A0 BRANCH, U+E0B0 SEPARATOR — the two most common powerline glyphs.
        for c in ['\u{e0a0}', '\u{e0b0}'] {
            let hit = sys.font_for_char(c, Style::Regular);
            assert!(
                hit.is_some(),
                "U+{:04X} should resolve through system fallback",
                c as u32
            );
            let (_, glyph) = hit.unwrap();
            assert_ne!(glyph, 0, "U+{:04X} resolved to .notdef", c as u32);
        }
    }

    #[test]
    fn a_symbol_outside_the_primary_font_still_resolves() {
        let mut sys = system();
        // Geometric shapes and box drawing, common in TUI output and unlikely to be
        // in a plain programming font.
        for c in ['◆', '█', '▄', '⑂'] {
            assert!(
                sys.font_for_char(c, Style::Regular).is_some(),
                "{c:?} (U+{:04X}) should resolve",
                c as u32
            );
        }
    }

    /// Characters used by the shell prompt theme that exposed this bug.
    ///
    /// `cyberzsh` draws its branch marker, brackets and battery gauge from the
    /// Miscellaneous Symbols and Geometric Shapes blocks — ordinary BMP codepoints
    /// that a programming font has no reason to carry. Searching only the configured
    /// fallback list left them blank while GNOME Terminal rendered them fine.
    const PROMPT_SYMBOLS: &[char] = &[
        '⑂',  // U+2442 branch
        '◈',  // U+25C8 diamond
        '▰',  // U+25B0 filled gauge segment
        '▱',  // U+25B1 empty gauge segment
        '⟦',  // U+27E6 bracket
        '⟧',  // U+27E7 bracket
        '❯',  // U+276F prompt arrow
        '─',  // U+2500 box drawing
        '▐',  // U+2590 half block
        '⚙',  // U+2699 gear
        '⚠',  // U+26A0 warning
        '⚡', // U+26A1 high voltage
        '✖',  // U+2716 cross
        '…',  // U+2026 ellipsis
    ];

    #[test]
    fn every_prompt_symbol_resolves_to_a_real_glyph() {
        let mut sys = system();
        for &c in PROMPT_SYMBOLS {
            let hit = sys.font_for_char(c, Style::Regular);
            assert!(
                hit.is_some(),
                "{c:?} (U+{:04X}) did not resolve; it would render as a blank cell",
                c as u32
            );
            assert_ne!(hit.unwrap().1, 0, "{c:?} resolved to .notdef");
        }
    }

    #[test]
    fn prompt_symbols_actually_rasterize_to_pixels() {
        // Resolving is only half the job: a glyph that resolves but rasterizes blank
        // still leaves a hole in the prompt.
        let mut sys = system();
        for &c in PROMPT_SYMBOLS {
            let Some((font, glyph)) = sys.font_for_char(c, Style::Regular) else {
                panic!("{c:?} should resolve");
            };
            let raster = sys
                .rasterize(font, glyph, Style::Regular)
                .unwrap_or_else(|| panic!("{c:?} should rasterize"));
            assert!(
                !raster.is_blank(),
                "{c:?} (U+{:04X}) rasterized to nothing",
                c as u32
            );
        }
    }

    #[test]
    fn a_fallback_result_is_cached_rather_than_rescanned() {
        // Scanning the whole font database is linear; doing it per frame for every
        // visible symbol would be ruinous.
        let mut sys = system();

        // Find a symbol that genuinely needed the system scan on this machine. Which
        // one that is depends on the installed fonts, so it is discovered rather
        // than hardcoded — an earlier version of this test asserted U+E0A0 and
        // failed because the primary font happened to carry it.
        let scanned = PROMPT_SYMBOLS.iter().copied().find(|&c| {
            sys.font_for_char(c, Style::Regular);
            sys.system_cache.contains_key(&c)
        });

        let Some(c) = scanned else {
            // Every symbol was already covered, so there is nothing to assert about
            // caching. Say so rather than passing silently.
            eprintln!("skipping: every prompt symbol is in an already-loaded font");
            return;
        };

        let first = sys.font_for_char(c, Style::Regular);
        assert!(first.is_some());
        assert_eq!(
            sys.font_for_char(c, Style::Regular),
            first,
            "a cached lookup must agree with the original"
        );
    }

    #[test]
    fn a_miss_is_cached_too() {
        // Otherwise one unmappable character rescans every installed font on every
        // frame it is on screen.
        let mut sys = system();
        let unmappable = '\u{10FFFD}';
        assert!(sys.font_for_char(unmappable, Style::Regular).is_none());
        assert_eq!(
            sys.system_cache.get(&unmappable),
            Some(&None),
            "the miss must be remembered"
        );
    }

    #[test]
    fn ascii_never_reaches_the_system_scan() {
        // The hot path must stay in the already-loaded faces.
        let mut sys = system();
        for c in ['a', 'Z', '0', '#'] {
            assert!(sys.font_for_char(c, Style::Regular).is_some());
        }
        assert!(
            sys.system_cache.is_empty(),
            "ASCII should be served by the primary font, not by a database scan"
        );
    }

    #[test]
    fn a_system_fallback_glyph_can_be_rasterized() {
        // Resolving is only half the job; it has to reach the atlas as pixels.
        let mut sys = system();
        let Some((font, glyph)) = sys.font_for_char('\u{e0a0}', Style::Regular) else {
            panic!("U+E0A0 should resolve");
        };
        let raster = sys
            .rasterize(font, glyph, Style::Regular)
            .expect("should rasterize");
        assert!(!raster.is_blank(), "the branch glyph should have pixels");
    }
}
