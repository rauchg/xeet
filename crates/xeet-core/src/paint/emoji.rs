//! Color emoji, drawn from the vendored `Apple Color Emoji` (`sbix`) font, with
//! a `COLRv0` fallback for vector color faces.
//!
//! X's web client renders post text with the native emoji font stack (`Apple
//! Color Emoji, Segoe UI Emoji, Noto Color Emoji, …`) rather than replacing
//! emoji with Twemoji `<img>` sprites. `tools/sync-upstream.mjs` extracts face
//! 0 (`Apple Color Emoji`) from macOS's `Apple Color Emoji.ttc` with its 64 ppem
//! `sbix` strike into `assets/vendor/fonts/AppleColorEmoji.ttf`.
//!
//! ## Placement (measured against Chrome/CoreText on macOS across 13–23 px @ 1–3×)
//!
//! `Apple Color Emoji` has `upem = 800`, `lsb = 0`, `glyf y_min = 0`, and 64×64
//! `sbix` PNG strikes at `ppem = 64` (`originOffset = (0, 0)`). Chrome/CoreText
//! scales the strike by `size_dev / ppem` (`1 em × 1 em`), left-aligns it at
//! the pen origin (`dx = 0`, leaving the trailing `0.25 em` of the `1.25 em`
//! advance on the right), and places the bottom of the em square `0.125 em`
//! (`100 / 800` font units) below the alphabetic baseline (`[-0.875 em .. +0.125 em]`).
//! Directional right-facing emoji (`🏃‍♂️‍➡️`, etc.) use `sbix` `flip` records
//! pointing at the left-facing PNG glyph; we follow `dupe` and `flip` links and
//! mirror horizontally when `flip` toggles parity.

use crate::color::Rgba;
use crate::fonts::Face;
use crate::paint::fonts;
use crate::paint::images::{decode_image, Limits};
use skrifa::color::{
    Brush, ColorGlyphCollection, ColorGlyphFormat, ColorPainter, ColorPalette, ColorPalettes,
    CompositeMode, PaintCachedColorGlyph, Transform as ColorTransform,
};
use skrifa::outline::OutlineGlyphCollection;
use skrifa::raw::{types::Tag, TableProvider};
use skrifa::{GlyphId, MetadataProvider};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap as SkPixmap};

/// A composed emoji, premultiplied RGBA, positioned relative to the glyph
/// origin's integer cell.
#[derive(Debug)]
pub(crate) struct Sprite {
    pub w: u32,
    pub h: u32,
    pub left: i32,
    pub top: i32,
    /// Premultiplied RGBA8.
    pub data: Vec<u8>,
}

impl Sprite {
    fn bytes(&self) -> usize {
        self.data.len() + std::mem::size_of::<Sprite>()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    face: u16,
    glyph: u32,
    size: u32,
    fx: u8,
    fy: u8,
}

const CACHE_BYTES: usize = 8 << 20;

const SBIX_LIMITS: Limits = Limits {
    max_dimension: 512,
    max_pixels: 262_144,
    max_bytes: 1 << 20,
};

/// Bounded cache: sprites plus the bytes they occupy.
type Cache = Mutex<(HashMap<Key, Arc<Sprite>>, usize)>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new((HashMap::new(), 0)))
}

/// The emoji sprite for one glyph at device origin (`x`, `y`).
pub(crate) fn sprite(
    face: &Face,
    glyph: u32,
    size_css: f32,
    dpr: f32,
    x: f32,
    y: f32,
) -> Option<Arc<Sprite>> {
    let size_dev = size_css * dpr;
    if !size_dev.is_finite() || size_dev <= 0.0 || !x.is_finite() || !y.is_finite() {
        return None;
    }
    let key = Key {
        face: face.id,
        glyph,
        size: (size_dev * 64.0).round() as u32,
        // Same 1/4 px grid as text glyphs (see `glyph::SUBPIXEL`).
        fx: (((x - x.floor()) * 4.0).round().clamp(0.0, 3.0)) as u8,
        fy: (((y - y.floor()) * 4.0).round().clamp(0.0, 3.0)) as u8,
    };
    {
        let (map, _) = &*cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = map.get(&key) {
            return Some(s.clone());
        }
    }
    let s = Arc::new(rasterize(
        face,
        glyph,
        size_css,
        dpr,
        key.fx as f32 / 4.0,
        key.fy as f32 / 4.0,
    )?);
    let (map, bytes) = &mut *cache().lock().unwrap_or_else(|e| e.into_inner());
    if *bytes + s.bytes() > CACHE_BYTES {
        map.clear();
        *bytes = 0;
    }
    *bytes += s.bytes();
    map.insert(key, s.clone());
    Some(s)
}

fn rasterize(face: &Face, glyph: u32, size_css: f32, dpr: f32, fx: f32, fy: f32) -> Option<Sprite> {
    rasterize_sbix(face, glyph, size_css, dpr, fx, fy)
        .or_else(|| rasterize_colrv0(face, glyph, size_css, dpr, fx, fy))
}

/// Rasterizes an Apple `sbix` PNG bitmap strike (including `dupe` and `flip`
/// glyph records) at `size_css * dpr` with separable premultiplied tent
/// filtering.
fn rasterize_sbix(
    face: &Face,
    glyph: u32,
    size_css: f32,
    dpr: f32,
    fx: f32,
    fy: f32,
) -> Option<Sprite> {
    let size_dev = size_css * dpr;
    let font = fonts::font(face.id)?;
    let sbix = font.sbix().ok()?;
    let strikes = sbix.strikes();
    if strikes.is_empty() {
        return None;
    }

    // Pick the smallest strike >= size_dev, or else the largest available strike.
    let mut chosen_idx = None;
    let mut best_ppem = 0u16;
    for i in 0..strikes.len() {
        let Ok(st) = strikes.get(i) else { continue };
        let p = st.ppem();
        if p == 0 {
            continue;
        }
        match chosen_idx {
            None => {
                chosen_idx = Some(i);
                best_ppem = p;
            }
            Some(_) => {
                let want = size_dev.ceil() as u16;
                if (p >= want && (best_ppem < want || p < best_ppem))
                    || (best_ppem < want && p > best_ppem)
                {
                    chosen_idx = Some(i);
                    best_ppem = p;
                }
            }
        }
    }
    let strike = strikes.get(chosen_idx?).ok()?;
    let ppem = strike.ppem() as f32;
    if ppem <= 0.0 {
        return None;
    }

    // Follow `dupe` (alias) and `flip` (horizontal mirror) chains to `png `.
    let mut gid = GlyphId::new(glyph);
    let mut flipped = false;
    let mut png_bytes: Option<&[u8]> = None;
    let mut ox = 0i16;
    let mut oy = 0i16;
    for _ in 0..5 {
        let gd = strike.glyph_data(gid).ok()??;
        let tag = gd.graphic_type();
        let data = gd.data();
        if tag == Tag::new(b"png ") {
            if data.is_empty() {
                return None;
            }
            ox = gd.origin_offset_x();
            oy = gd.origin_offset_y();
            png_bytes = Some(data);
            break;
        } else if (tag == Tag::new(b"dupe") || tag == Tag::new(b"flip")) && data.len() >= 2 {
            if tag == Tag::new(b"flip") {
                flipped = !flipped;
            }
            let target = u16::from_be_bytes([data[0], data[1]]) as u32;
            gid = GlyphId::new(target);
        } else {
            return None;
        }
    }
    let decoded = decode_image(png_bytes?, &SBIX_LIMITS).ok()?;
    let src_w = decoded.width as usize;
    let src_h = decoded.height as usize;
    if src_w == 0 || src_h == 0 {
        return None;
    }

    // Premultiply RGBA8 (and mirror horizontally when `flip` was traversed).
    let mut src = vec![[0.0f32; 4]; src_w * src_h];
    for y in 0..src_h {
        for x in 0..src_w {
            let sx = if flipped { src_w - 1 - x } else { x };
            let i = (y * src_w + sx) * 4;
            let a = decoded.rgba[i + 3] as f32 * (1.0 / 255.0);
            src[y * src_w + x] = [
                decoded.rgba[i] as f32 * a,
                decoded.rgba[i + 1] as f32 * a,
                decoded.rgba[i + 2] as f32 * a,
                decoded.rgba[i + 3] as f32,
            ];
        }
    }

    let metrics = font.glyph_metrics(
        skrifa::instance::Size::unscaled(),
        skrifa::instance::LocationRef::default(),
    );
    let (lsb, y_min) = if let Some(b) = metrics.bounds(gid) {
        (metrics.left_side_bearing(gid).unwrap_or(0.0), b.y_min)
    } else {
        (0.0, 0.0)
    };
    let upem = face.upem.max(1.0);
    let dst_w = src_w as f32 * (size_dev / ppem);
    let dst_h = src_h as f32 * (size_dev / ppem);
    if dst_w <= 0.0 || dst_h <= 0.0 {
        return None;
    }

    // CoreText / Skia placement: left-aligned at pen + bearing + origin_offset,
    // with the bottom of the em box sitting 0.125 em (100/800 units) below the
    // alphabetic baseline.
    let x0 = (lsb / upem) * size_dev + (ox as f32 / ppem) * size_dev + fx;
    let y1 = -(y_min / upem) * size_dev - (oy as f32 / ppem) * size_dev + size_dev * 0.125 + fy;
    let y0 = y1 - dst_h;

    let left = x0.floor();
    let top = y0.floor();
    let w = (x0 + dst_w).ceil() - left;
    let h = y1.ceil() - top;
    let w = w.max(0.0) as usize;
    let h = h.max(0.0) as usize;
    if w == 0 || h == 0 || w > 2048 || h > 2048 {
        return None;
    }

    let data = resample_tent_premul(&src, src_w, src_h, w, h, x0 - left, y0 - top, dst_w, dst_h);
    Some(Sprite {
        w: w as u32,
        h: h as u32,
        left: left as i32,
        top: top as i32,
        data,
    })
}

/// Two-pass separable tent (bilinear when upsampling, area-averaged triangle
/// when downsampling) resampler in premultiplied `[0..255]` RGBA space.
#[allow(clippy::too_many_arguments)]
fn resample_tent_premul(
    src: &[[f32; 4]],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    off_x: f32,
    off_y: f32,
    span_w: f32,
    span_h: f32,
) -> Vec<u8> {
    let scale_x = src_w as f32 / span_w;
    let scale_y = src_h as f32 / span_h;
    let rx = scale_x.max(1.0);
    let ry = scale_y.max(1.0);

    // Precompute 1D filter taps for X and Y. Each tap maps into `[0..src_len]`
    // with an implicit transparent border (`-1`) outside `[0, span)` so subpixel
    // overhang fades cleanly instead of clamping edge pixels. Both axes live in
    // flat tables: a resample allocates two vectors, not two per pixel.
    let x_taps = build_taps(dst_w, src_w, off_x, span_w, scale_x, rx);
    let y_taps = build_taps(dst_h, src_h, off_y, span_h, scale_y, ry);

    // Pass 1: horizontal (`src_h × dst_w`).
    let mut tmp = vec![[0.0f32; 4]; src_h * dst_w];
    for sy in 0..src_h {
        let row = &src[sy * src_w..(sy + 1) * src_w];
        let out_row = &mut tmp[sy * dst_w..(sy + 1) * dst_w];
        for (dx, cell) in out_row.iter_mut().enumerate() {
            let mut acc = [0.0f32; 4];
            for &(sx, w) in x_taps.row(dx) {
                let p = row[sx];
                acc[0] += p[0] * w;
                acc[1] += p[1] * w;
                acc[2] += p[2] * w;
                acc[3] += p[3] * w;
            }
            *cell = acc;
        }
    }

    // Pass 2: vertical (`dst_h × dst_w`) → packed premultiplied RGBA8.
    let mut out = vec![0u8; dst_w * dst_h * 4];
    for dy in 0..dst_h {
        let taps = y_taps.row(dy);
        for dx in 0..dst_w {
            let mut acc = [0.0f32; 4];
            for &(sy, w) in taps {
                let p = tmp[sy * dst_w + dx];
                acc[0] += p[0] * w;
                acc[1] += p[1] * w;
                acc[2] += p[2] * w;
                acc[3] += p[3] * w;
            }
            let a = acc[3].round().clamp(0.0, 255.0) as u8;
            // Keep premultiplied channels <= alpha after rounding.
            let r = (acc[0].round().clamp(0.0, 255.0) as u8).min(a);
            let g = (acc[1].round().clamp(0.0, 255.0) as u8).min(a);
            let b = (acc[2].round().clamp(0.0, 255.0) as u8).min(a);
            let o = (dy * dst_w + dx) * 4;
            out[o] = r;
            out[o + 1] = g;
            out[o + 2] = b;
            out[o + 3] = a;
        }
    }
    out
}

/// One axis's filter taps, flat: destination `d` owns `taps[spans[d].0..spans[d].1]`.
struct TapTable {
    spans: Vec<(u32, u32)>,
    taps: Vec<(usize, f32)>,
}

impl TapTable {
    fn row(&self, d: usize) -> &[(usize, f32)] {
        let (lo, hi) = self.spans[d];
        &self.taps[lo as usize..hi as usize]
    }
}

fn build_taps(
    dst_len: usize,
    src_len: usize,
    off: f32,
    span: f32,
    scale: f32,
    radius: f32,
) -> TapTable {
    let mut spans = Vec::with_capacity(dst_len);
    let mut taps: Vec<(usize, f32)> = Vec::with_capacity(dst_len * 2);
    for d in 0..dst_len {
        let start = taps.len();
        let dst_pos = d as f32 + 0.5 - off;
        if dst_pos <= -0.5 || dst_pos >= span + 0.5 {
            spans.push((start as u32, taps.len() as u32));
            continue;
        }
        // Coverage for edge pixels that only partially overlap `[0, span]`.
        let edge_cov = ((dst_pos + 0.5).min(span + 0.5 - dst_pos)).clamp(0.0, 1.0);
        let center = (dst_pos * scale - 0.5).clamp(-0.5, src_len as f32 - 0.5);
        let lo = (center - radius).floor() as isize;
        let hi = (center + radius).ceil() as isize;
        let mut total = 0.0f32;
        for s in lo..=hi {
            let w = (1.0 - ((s as f32 - center).abs() / radius)).max(0.0);
            if w <= 0.0 {
                continue;
            }
            total += w;
            if s >= 0 && (s as usize) < src_len {
                taps.push((s as usize, w));
            }
        }
        if total > 0.0 {
            let norm = edge_cov / total;
            for t in &mut taps[start..] {
                t.1 *= norm;
            }
        }
        spans.push((start as u32, taps.len() as u32));
    }
    TapTable { spans, taps }
}

fn rasterize_colrv0(
    face: &Face,
    glyph: u32,
    size_css: f32,
    dpr: f32,
    fx: f32,
    fy: f32,
) -> Option<Sprite> {
    let size_dev = size_css * dpr;
    let font = fonts::font(face.id)?;
    let color_glyph = ColorGlyphCollection::new(&font)
        .get_with_format(GlyphId::new(glyph), ColorGlyphFormat::ColrV0)?;
    let palettes = ColorPalettes::new(&font);
    let palette = palettes.get(0);
    let mut painter = LayerPainter {
        face,
        size_dev,
        palette,
        transforms: Vec::new(),
        stack: Vec::new(),
        layers: Vec::new(),
    };
    color_glyph
        .paint(skrifa::instance::LocationRef::default(), &mut painter)
        .ok()?;
    let mut layers = painter.layers;
    if layers.is_empty() {
        return None;
    }

    let mut bounds = layers[0].0.bounds();
    for (p, _) in &layers[1..] {
        let b = p.bounds();
        bounds = tiny_skia::Rect::from_ltrb(
            bounds.left().min(b.left()),
            bounds.top().min(b.top()),
            bounds.right().max(b.right()),
            bounds.bottom().max(b.bottom()),
        )?;
    }

    let advance = crate::fonts::emoji_advance(size_css) * dpr;
    let art_w = bounds.right() - bounds.left();
    let dx = (advance - art_w) / 2.0 - bounds.left();

    let min_x = bounds.left() + dx + fx;
    let min_y = bounds.top() + fy;
    let left = min_x.floor();
    let top = min_y.floor();
    let w = ((bounds.right() + dx + fx).ceil() - left).max(0.0) as u32;
    let h = ((bounds.bottom() + fy).ceil() - top).max(0.0) as u32;
    if w == 0 || h == 0 || w > 2048 || h > 2048 {
        return None;
    }
    let mut pm = SkPixmap::new(w, h)?;
    let ts = tiny_skia::Transform::from_translate(dx + fx - left, fy - top);
    for (path, color) in layers.drain(..) {
        let mut paint = Paint::default();
        paint.set_color_rgba8(color.r, color.g, color.b, color.a);
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, FillRule::Winding, ts, None);
    }
    Some(Sprite {
        w,
        h,
        left: left as i32,
        top: top as i32,
        data: pm.take(),
    })
}

/// Collects COLRv0 layers as device-space paths with colors.
struct LayerPainter<'a> {
    face: &'a Face,
    size_dev: f32,
    palette: Option<ColorPalette<'a>>,
    /// Current transform (font units → device pixels), as a 2×3 matrix.
    transforms: Vec<[f32; 6]>,
    /// Clip paths pushed by the color traversal (unused by COLRv0).
    stack: Vec<Option<tiny_skia::Path>>,
    layers: Vec<(tiny_skia::Path, Rgba)>,
}

impl LayerPainter<'_> {
    fn transform(&self) -> [f32; 6] {
        self.transforms.last().copied().unwrap_or(IDENTITY)
    }

    fn color(&self, palette_index: u16, alpha: f32) -> Rgba {
        let c = self
            .palette
            .as_ref()
            .and_then(|p| p.colors().get(palette_index as usize))
            .map(|c| Rgba {
                r: c.red,
                g: c.green,
                b: c.blue,
                a: c.alpha,
            })
            .unwrap_or(Rgba::BLACK);
        let a = (c.a as f32 * alpha).clamp(0.0, 1.0);
        Rgba {
            a: (a * 255.0).round() as u8,
            ..c
        }
    }

    fn glyph_path(&self, glyph: GlyphId) -> Option<tiny_skia::Path> {
        let font = fonts::font(self.face.id)?;
        let g = OutlineGlyphCollection::new(&font).get(glyph)?;
        let mut pen = crate::paint::glyph::Pen::new(1.0);
        g.draw(
            skrifa::outline::DrawSettings::unhinted(
                skrifa::instance::Size::new(self.size_dev),
                skrifa::instance::LocationRef::default(),
            ),
            &mut pen,
        )
        .ok()?;
        let path = pen.finish()?;
        let m = self.transform();
        path.transform(tiny_skia::Transform::from_row(
            m[0], m[1], m[2], m[3], m[4], m[5],
        ))
    }
}

const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

impl ColorPainter for LayerPainter<'_> {
    fn push_transform(&mut self, t: ColorTransform) {
        let a = self.transform();
        let b = [t.xx, t.yx, t.xy, t.yy, t.dx, t.dy];
        self.transforms.push([
            a[0] * b[0] + a[2] * b[1],
            a[1] * b[0] + a[3] * b[1],
            a[0] * b[2] + a[2] * b[3],
            a[1] * b[2] + a[3] * b[3],
            a[0] * b[4] + a[2] * b[5] + a[4],
            a[1] * b[4] + a[3] * b[5] + a[5],
        ]);
    }

    fn pop_transform(&mut self) {
        self.transforms.pop();
    }

    fn push_clip_glyph(&mut self, glyph_id: GlyphId) {
        self.stack.push(self.glyph_path(glyph_id));
    }

    fn push_clip_box(&mut self, clip_box: skrifa::raw::types::BoundingBox<f32>) {
        let p = tiny_skia::Rect::from_ltrb(
            clip_box.x_min,
            clip_box.y_min,
            clip_box.x_max,
            clip_box.y_max,
        )
        .and_then(|r| PathBuilder::from_rect(r).into());
        self.stack.push(p);
    }

    fn pop_clip(&mut self) {
        self.stack.pop();
    }

    fn fill(&mut self, brush: Brush<'_>) {
        if let Some(Some(clip)) = self.stack.last() {
            if let Brush::Solid {
                palette_index,
                alpha,
            } = brush
            {
                self.layers
                    .push((clip.clone(), self.color(palette_index, alpha)));
            }
        }
    }

    fn fill_glyph(
        &mut self,
        glyph_id: GlyphId,
        _brush_transform: Option<ColorTransform>,
        brush: Brush<'_>,
    ) {
        let Brush::Solid {
            palette_index,
            alpha,
        } = brush
        else {
            return;
        };
        let Some(path) = self.glyph_path(glyph_id) else {
            return;
        };
        self.layers.push((path, self.color(palette_index, alpha)));
    }

    fn push_layer(&mut self, _mode: CompositeMode) {}

    fn pop_layer(&mut self) {}

    fn paint_cached_color_glyph(
        &mut self,
        _glyph: GlyphId,
    ) -> Result<PaintCachedColorGlyph, skrifa::color::PaintError> {
        Ok(PaintCachedColorGlyph::Unimplemented)
    }
}
