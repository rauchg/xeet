//! Glyph outlines to anti-aliased coverage masks, with a bounded cache.
//!
//! Chrome rasterizes text with hinting off (`--font-render-hinting=none` in
//! the oracle) and grayscale anti-aliasing. Glyphs sit at fractional device
//! positions, so the subpixel offset is baked into the mask: a mask is cached
//! per (face, glyph, size, subpixel bucket) and blitted at an integer offset.
//! Skia does the same thing with a 1/4 px subpixel grid; we keep 1/16 px, which
//! is finer than the oracle can tell apart and keeps the cache small.
//!
//! The cache holds only public glyph coverage: no tenant data, bounded by
//! bytes, cleared wholesale when it overflows. Its contents never change the
//! output, so results stay deterministic.

use crate::fonts::{db, Face};

use super::coverage;
use crate::paint::fonts;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::MetadataProvider as _;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tiny_skia::PathBuilder;

/// A rasterized glyph: coverage plus its integer position relative to the
/// glyph origin (device pixels, y down).
#[derive(Debug)]
pub(crate) struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub left: i32,
    pub top: i32,
    pub data: Vec<u8>,
}

impl Bitmap {
    fn bytes(&self) -> usize {
        self.data.len() + std::mem::size_of::<Bitmap>()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    generation: u32,
    face: u16,
    glyph: u32,
    /// Font size in device pixels × 64.
    size: u32,
    /// Subpixel offsets, in 1/[`SUBPIXEL`] px.
    fx: u8,
    fy: u8,
}

/// Subpixel buckets per pixel. Skia places each glyph mask on a 1/4 px grid in
/// device space (`SkPackedGlyphID`'s two subpixel bits), and 1/4 measured
/// better than 1/2, 1/8, 1/16 or 1/64 against Chrome in
/// `tools/oracle/raster-text.mjs`, so that is the grid we use: the mask bakes
/// in the quantized offset and is blitted at an integer position.
const SUBPIXEL: f32 = 4.0;

/// A device position snapped to the 1/4 px grid, as an exact value.
///
/// Quantizing the *position* (rather than only the mask's phase) keeps the
/// integer part consistent with the phase: a position of 10.9 rounds to 11.0,
/// not to "10 plus a 0.75 phase", which would put the glyph a quarter of a pixel
/// to the left of where Skia puts it.
pub(crate) fn quantize(v: f32) -> f32 {
    if v.is_finite() {
        (v * SUBPIXEL).round() / SUBPIXEL
    } else {
        v
    }
}

/// The phase of an already-quantized position, in 1/[`SUBPIXEL`] px.
fn subpixel(v: f32) -> u8 {
    let f = v - v.floor();
    (f * SUBPIXEL).round() as u8 % SUBPIXEL as u8
}

const CACHE_BYTES: usize = 12 << 20;

struct Cache {
    map: HashMap<Key, Arc<Bitmap>>,
    bytes: usize,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(Cache {
            map: HashMap::new(),
            bytes: 0,
        })
    })
}

/// Coverage for one glyph, positioned at device origin `(x, y)` (baseline).
pub(crate) fn bitmap(face_id: u16, glyph: u32, size: f32, x: f32, y: f32) -> Option<Arc<Bitmap>> {
    if !size.is_finite() || size <= 0.0 || !x.is_finite() || !y.is_finite() {
        return None;
    }
    let (x, y) = (quantize(x), quantize(y));
    let key = Key {
        generation: crate::fonts::generation(),
        face: face_id,
        glyph,
        size: (size * 64.0).round().clamp(1.0, u32::MAX as f32) as u32,
        fx: subpixel(x),
        fy: subpixel(y),
    };
    {
        let cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = cache.map.get(&key) {
            return Some(b.clone());
        }
    }
    let fonts = db();
    let face = fonts.faces.get(face_id as usize)?;
    let size = key.size as f32 / 64.0;
    let bmp = Arc::new(rasterize(
        face,
        glyph,
        size,
        key.fx as f32 / SUBPIXEL,
        key.fy as f32 / SUBPIXEL,
    )?);
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.bytes + bmp.bytes() > CACHE_BYTES {
        cache.map.clear();
        cache.bytes = 0;
    }
    cache.bytes += bmp.bytes();
    cache.map.insert(key, bmp.clone());
    Some(bmp)
}

/// The glyph's outline in device pixels, origin at the pen position.
///
/// When `glyph == 0` (`.notdef`) has an empty outline in the font — as in X's
/// Chirp subsets — synthesizes a visible stroked tofu box inside `.notdef`'s
/// advance so unsupported scripts never vanish silently.
pub(crate) fn outline_path(face: &Face, glyph: u32, size: f32) -> Option<tiny_skia::Path> {
    let font = fonts::font(face.id)?;
    let glyphs = skrifa::outline::OutlineGlyphCollection::new(&font);
    let drawn = glyphs.get(skrifa::GlyphId::new(glyph)).and_then(|g| {
        // `DrawSettings::unhinted(Size)` already scales font units to pixel
        // units, so the pen only packs the path.
        let mut pen = Pen::new(1.0);
        // FreeType's point order: Chrome rasterizes through FreeType, so the
        // contour start conventions have to match it, not HarfBuzz's.
        g.draw(
            DrawSettings::unhinted(Size::new(size), LocationRef::default()),
            &mut pen,
        )
        .ok()?;
        pen.finish()
    });
    if drawn.is_some() {
        return drawn;
    }
    if glyph == 0 {
        return synthetic_notdef(&font, size);
    }
    None
}

fn synthetic_notdef(font: &skrifa::FontRef<'_>, size: f32) -> Option<tiny_skia::Path> {
    let adv = font
        .glyph_metrics(Size::new(size), LocationRef::default())
        .advance_width(skrifa::GlyphId::new(0))
        .unwrap_or(0.55 * size)
        .max(0.45 * size);
    let pad_x = (0.09 * adv).max(0.75);
    let x0 = pad_x;
    let x1 = (adv - pad_x).max(x0 + 2.0);
    let y0 = -0.71 * size;
    let y1 = -0.02 * size;
    let stroke = (0.065 * size).clamp(1.0, ((x1 - x0).min(y1 - y0) * 0.35).max(1.0));

    let mut pb = PathBuilder::new();
    // Outer rectangle (CW).
    pb.move_to(x0, y0);
    pb.line_to(x1, y0);
    pb.line_to(x1, y1);
    pb.line_to(x0, y1);
    pb.close();
    // Inner cutout (CCW) so NonZero fill leaves a crisp hollow `.notdef` frame.
    let (ix0, iy0, ix1, iy1) = (x0 + stroke, y0 + stroke, x1 - stroke, y1 - stroke);
    if ix1 > ix0 && iy1 > iy0 {
        pb.move_to(ix0, iy0);
        pb.line_to(ix0, iy1);
        pb.line_to(ix1, iy1);
        pb.line_to(ix1, iy0);
        pb.close();
    }
    pb.finish()
}

/// Builds a path from font units, scaled by a fixed factor (font size ÷ upem).
///
/// Font outlines are y-up; the canvas is y-down, so y is negated here. The
/// result is in device pixels with the origin on the baseline.
pub(crate) struct Pen {
    p: PathBuilder,
    scale: f32,
    open: bool,
    any: bool,
}

impl Pen {
    pub(crate) fn new(scale: f32) -> Pen {
        Pen {
            p: PathBuilder::new(),
            scale,
            open: false,
            any: false,
        }
    }

    /// The finished path, or `None` if the glyph had no contours (a space).
    pub(crate) fn finish(mut self) -> Option<tiny_skia::Path> {
        if self.open {
            self.p.close();
        }
        if !self.any {
            return None;
        }
        self.p.finish()
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.open {
            self.p.close();
        }
        self.p.move_to(x * self.scale, -y * self.scale);
        self.open = true;
        self.any = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.p.line_to(x * self.scale, -y * self.scale);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.p.quad_to(
            cx * self.scale,
            -cy * self.scale,
            x * self.scale,
            -y * self.scale,
        );
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.p.cubic_to(
            cx0 * self.scale,
            -cy0 * self.scale,
            cx1 * self.scale,
            -cy1 * self.scale,
            x * self.scale,
            -y * self.scale,
        );
    }
    fn close(&mut self) {
        self.p.close();
        self.open = false;
    }
}

fn rasterize(face: &Face, glyph: u32, size: f32, fx: f32, fy: f32) -> Option<Bitmap> {
    let path = outline_path(face, glyph, size)?;
    let bounds = path.bounds();
    // Subpixel offset, then the integer cell the mask covers.
    let min_x = bounds.left() + fx;
    let min_y = bounds.top() + fy;
    let left = min_x.floor();
    let top = min_y.floor();
    let w = ((bounds.right() + fx).ceil() - left).max(0.0) as u32;
    let h = ((bounds.bottom() + fy).ceil() - top).max(0.0) as u32;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    // Exact-area coverage in the mask's own pixel grid (see paint::coverage):
    // tiny-skia's scanline AA is not accurate enough to match Chrome.
    let data = coverage::mask_transformed(
        &path,
        w,
        h,
        tiny_skia::Transform::from_translate(fx - left, fy - top),
    )?;
    Some(Bitmap {
        w,
        h,
        left: left as i32,
        top: top as i32,
        data,
    })
}
