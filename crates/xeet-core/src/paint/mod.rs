//! Rasterization: a [`DisplayList`] in, pixels out.
//!
//! The painter is deliberately unknowable about posts. It walks the item list
//! in order, scales every coordinate by the device pixel ratio, and draws with
//! a Skia-derived rasterizer ([`tiny_skia`], a pure-Rust port of Skia's engine)
//! so anti-aliasing behaves the way Chrome's does.
//!
//! ## Coordinates and formats
//!
//! The display list is in CSS pixels with y down. Every item is multiplied by
//! `dpr` exactly once, inside this module. [`Pixmap::rgba`] is **straight
//! (not premultiplied) sRGB RGBA8**, row-major, top-left origin — the layout
//! PNG, canvas `ImageData` and most callers expect. Internally the canvas is
//! premultiplied, as Skia's is; when the frame is opaque (background alpha
//! 255, the normal case) the two are identical and no conversion happens.
//!
//! ## Robustness
//!
//! Display lists come from layout code that may be mid-change, so every item
//! is sanitized: non-finite or absurd geometry is skipped, sizes are clamped,
//! clips pop defensively, and a dangling `PushClip` never leaks into
//! anything after it. Nothing here may panic.

use crate::color::Rgba;
use crate::display::{self, DisplayList, ImageRef, Item, Rect, Shape};
use crate::fonts::{db, FaceKind};
use std::sync::{Arc, Mutex, OnceLock};
use tiny_skia::{FillRule, Mask, Paint, Path, Pixmap as SkPixmap, Transform};

mod coverage;
mod emoji;
mod encode;
mod fonts;
mod glyph;
mod icons;
mod images;
mod shapes;
mod svg;

pub use encode::{encode_png, encode_png_best};
pub use images::{decode_image, service_box, service_copy, DecodeError, Limits};
pub use svg::to_svg;

/// Fills up to this many device pixels are rasterized with exact-area coverage
/// (icons, badges); larger ones keep tiny-skia's scanline AA, which is much
/// cheaper for shapes that big.
const SMALL_FILL_PIXELS: u32 = 64 * 64;

/// An RGBA8 image: straight alpha, row-major, top-left origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Pixmap {
    pub fn new(width: u32, height: u32) -> Pixmap {
        Pixmap {
            width,
            height,
            rgba: vec![0; width as usize * height as usize * 4],
        }
    }

    /// The color at one pixel; `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgba> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let p = &self.rgba[i..i + 4];
        Some(Rgba {
            r: p[0],
            g: p[1],
            b: p[2],
            a: p[3],
        })
    }

    /// The PNG encoding of this pixmap (fast settings; see [`encode_png`]).
    pub fn to_png(&self) -> Vec<u8> {
        encode_png(self)
    }
}

/// A decoded image, straight-alpha sRGB RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A decoded image with the work every draw would otherwise repeat.
///
/// Beyond the pixels, a draw needs to know whether the image is fully opaque
/// (an opaque source is sampled straight from its own bytes), a premultiplied
/// copy for the ones that are not, and a halved mip chain for downscaling. All
/// three depend only on the bytes, so they are computed here once and shared:
/// a sheet or a batch draws the same photo at every viewport width.
type MipLevel = (u32, u32, u32, Arc<Vec<u8>>);

pub struct PreparedImage {
    decoded: DecodedImage,
    opaque: bool,
    premultiplied: OnceLock<Vec<u8>>,
    /// Halved levels below the source: `(level, width, height, texels)`.
    mips: Mutex<Vec<MipLevel>>,
    /// X's image-service copies of a card picture, by the side of the service's
    /// box ([`images::service_copy`]), each prepared in turn, so a second draw
    /// into the same box pays for neither the resize nor the mips.
    service: Mutex<Vec<(u32, Arc<PreparedImage>)>>,
}

impl PreparedImage {
    pub fn new(decoded: DecodedImage) -> PreparedImage {
        let opaque = images::is_opaque(&decoded.rgba);
        PreparedImage {
            decoded,
            opaque,
            premultiplied: OnceLock::new(),
            mips: Mutex::new(Vec::new()),
            service: Mutex::new(Vec::new()),
        }
    }

    /// The image as X's image service serves it in a `side` × `side` box,
    /// prepared, built on first use and kept.
    pub(crate) fn service_copy(&self, side: u32) -> Arc<PreparedImage> {
        let mut cache = self.service.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, copy)) = cache.iter().find(|(w, _)| *w == side) {
            return Arc::clone(copy);
        }
        let copy = Arc::new(PreparedImage::new(images::service_copy(
            &self.decoded,
            side,
        )));
        cache.push((side, Arc::clone(&copy)));
        copy
    }

    /// The straight-alpha pixels.
    pub fn decoded(&self) -> &DecodedImage {
        &self.decoded
    }

    /// Bytes this image is holding, for a cache's budget.
    pub fn bytes(&self) -> usize {
        let mips = self
            .mips
            .lock()
            .map(|m| m.iter().map(|(_, _, _, d)| d.len()).sum::<usize>())
            .unwrap_or(0);
        let service = self
            .service
            .lock()
            .map(|s| s.iter().map(|(_, d)| d.bytes()).sum::<usize>())
            .unwrap_or(0);
        self.decoded.rgba.len()
            + self.premultiplied.get().map_or(0, |p| p.len())
            + mips
            + service
            + std::mem::size_of::<PreparedImage>()
    }

    /// `true` when every pixel is opaque, which lets sampling skip
    /// premultiplication.
    pub(crate) fn is_opaque(&self) -> bool {
        self.opaque
    }

    /// The premultiplied copy, built on first use.
    pub(crate) fn premultiplied(&self) -> &[u8] {
        self.premultiplied
            .get_or_init(|| images::premultiply(&self.decoded.rgba))
    }
}

/// Supplies pixels for `Item::Image`.
///
/// The painter asks for an image each time it draws one; implementations are
/// free to cache. Returning `None` means "no bytes", and the painter draws the
/// image's neutral placeholder instead, so a frame never fails for missing
/// media.
///
/// Implement [`rgba_ref`](ImageSource::rgba_ref) as well if you keep decoded
/// images alive between draws: the painter uses it when present and so avoids
/// copying the whole buffer (a 1600×900 photo is 5.8 MB per copy).
pub trait ImageSource {
    fn rgba(&self, image: &ImageRef) -> Option<DecodedImage>;

    /// Optional fast path: the decoded image, by reference.
    ///
    /// The default returns `None`, which makes the painter fall back to
    /// [`rgba`](ImageSource::rgba).
    fn rgba_ref(&self, _image: &ImageRef) -> Option<&DecodedImage> {
        None
    }

    /// Optional fastest path: the image with its opacity, premultiplied copy
    /// and mip chain already worked out.
    ///
    /// The default returns `None`, which makes the painter sample an uncached
    /// pyramid built for this draw alone.
    fn prepared_ref(&self, _image: &ImageRef) -> Option<&PreparedImage> {
        None
    }
}

/// An [`ImageSource`] with no images at all.
pub struct NoImages;

impl ImageSource for NoImages {
    fn rgba(&self, _image: &ImageRef) -> Option<DecodedImage> {
        None
    }
}

impl ImageSource for [(ImageRef, DecodedImage)] {
    fn rgba(&self, image: &ImageRef) -> Option<DecodedImage> {
        self.iter()
            .find(|(r, _)| r == image)
            .map(|(_, d)| d.clone())
    }
}

impl ImageSource for Vec<(ImageRef, DecodedImage)> {
    fn rgba(&self, image: &ImageRef) -> Option<DecodedImage> {
        self.as_slice().rgba(image)
    }

    fn rgba_ref(&self, image: &ImageRef) -> Option<&DecodedImage> {
        self.as_slice().rgba_ref(image)
    }
}

impl<T: ImageSource + ?Sized> ImageSource for &T {
    fn rgba(&self, image: &ImageRef) -> Option<DecodedImage> {
        (**self).rgba(image)
    }

    fn rgba_ref(&self, image: &ImageRef) -> Option<&DecodedImage> {
        (**self).rgba_ref(image)
    }

    fn prepared_ref(&self, image: &ImageRef) -> Option<&PreparedImage> {
        (**self).prepared_ref(image)
    }
}

/// Rasterizes a display list at `dpr`.
pub fn rasterize(list: &DisplayList, dpr: f32, images: &dyn ImageSource) -> Pixmap {
    render(list, dpr, images)
}

/// Rasterizes and encodes in one step, the shape most callers want.
pub fn rasterize_png(list: &DisplayList, dpr: f32, images: &dyn ImageSource) -> Vec<u8> {
    encode_png(&render(list, dpr, images))
}

fn render(list: &DisplayList, dpr: f32, images: &dyn ImageSource) -> Pixmap {
    let dpr = if dpr.is_finite() && dpr > 0.0 {
        dpr
    } else {
        1.0
    };
    let w = (list.width * dpr).round().clamp(1.0, 32768.0) as u32;
    let h = (list.height * dpr).round().clamp(1.0, 32768.0) as u32;
    let Some(mut pm) = SkPixmap::new(w, h) else {
        return Pixmap::new(w.min(32768), h.min(32768));
    };
    let bg = list.background;
    if bg.a > 0 {
        pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, bg.a));
    }
    let opaque = bg.a == 255;
    let mut r = Renderer {
        pm,
        dpr,
        images,
        clips: Vec::new(),
        clip: Clip::None,
    };
    for item in &list.items {
        r.item(item);
    }
    let pm = r.pm;
    let mut rgba = pm.take();
    if !opaque {
        // Straight alpha for callers and PNG: undo the premultiplied store,
        // rounding half up, clamped. Fully transparent pixels carry no color.
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            if a == 255 {
                continue;
            }
            if a == 0 {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
                continue;
            }
            let half = a / 2;
            px[0] = ((px[0] as u32 * 255 + half) / a).min(255) as u8;
            px[1] = ((px[1] as u32 * 255 + half) / a).min(255) as u8;
            px[2] = ((px[2] as u32 * 255 + half) / a).min(255) as u8;
        }
    }
    Pixmap {
        width: w,
        height: h,
        rgba,
    }
}

/// The current clip, in device pixels.
pub(crate) enum Clip {
    None,
    /// Every clip on the stack is an axis-aligned rectangle: glyph blits and
    /// images clamp to it instead of paying for a mask.
    Rect(Rect),
    /// The clip is a mask, kept at the size of its bounding box (`left`/`top`
    /// are the box's origin on the canvas). The canvas-sized copy the
    /// `tiny_skia` calls want is made on demand, by draws that need one.
    Mask {
        mask: Box<Mask>,
        left: i32,
        top: i32,
        canvas_mask: Option<Box<Mask>>,
    },
}

struct Renderer<'a> {
    pm: SkPixmap,
    dpr: f32,
    images: &'a dyn ImageSource,
    clips: Vec<Shape>,
    clip: Clip,
}

impl Renderer<'_> {
    fn item(&mut self, item: &Item) {
        match item {
            Item::Fill { shape, color } => {
                if let Some(p) = shapes::path(shape, self.dpr) {
                    self.fill(&p, *color, FillRule::Winding);
                }
            }
            Item::Stroke {
                shape,
                color,
                width,
            } => {
                if let Some(path) = shapes::path(shape, self.dpr) {
                    self.stroke_inset(&path, *color, *width * self.dpr);
                }
            }
            Item::Glyphs {
                x,
                y,
                face,
                size,
                color,
                glyphs,
            } => {
                self.glyphs(*x, *y, *face, *size, *color, glyphs);
            }
            Item::Icon { name, rect, color } => self.icon(name, rect, *color),
            Item::Image {
                shape,
                image,
                focus,
                service,
            } => self.image(shape, image, *focus, *service),
            Item::Placeholder { shape, label } => self.placeholder(shape, label),
            Item::PushClip { shape } => {
                if self.clips.len() < 64 {
                    self.clips.push(*shape);
                    self.rebuild_clip();
                }
            }
            Item::PopClip => {
                if self.clips.pop().is_some() {
                    self.rebuild_clip();
                }
            }
        }
    }

    fn rebuild_clip(&mut self) {
        self.clip = Clip::None;
        if self.clips.is_empty() {
            return;
        }
        let full = Rect {
            x: 0.0,
            y: 0.0,
            w: self.pm.width() as f32,
            h: self.pm.height() as f32,
        };
        let mut rect = full;
        let mut all_rects = true;
        for s in &self.clips {
            match plain_rect(s, self.dpr) {
                Some(r) => {
                    let x0 = rect.x.max(r.x);
                    let y0 = rect.y.max(r.y);
                    let x1 = (rect.x + rect.w).min(r.x + r.w);
                    let y1 = (rect.y + rect.h).min(r.y + r.h);
                    rect = Rect {
                        x: x0,
                        y: y0,
                        w: (x1 - x0).max(0.0),
                        h: (y1 - y0).max(0.0),
                    };
                }
                None => {
                    all_rects = false;
                    break;
                }
            }
        }
        if all_rects {
            self.clip = Clip::Rect(rect);
            return;
        }
        // A mask only has to cover the box the clip paths occupy: the
        // intersection is empty outside it by construction, so a clip on a
        // small badge does not cost the whole canvas.
        let paths: Vec<Path> = self
            .clips
            .iter()
            .filter_map(|s| shapes::path(s, self.dpr))
            .collect();
        if paths.is_empty() {
            // No clip path has an area: an empty stack draws everything.
            return;
        }
        let (pm_w, pm_h) = (self.pm.width(), self.pm.height());
        let mut box0 = (0.0f32, 0.0f32);
        let mut box1 = (pm_w as f32, pm_h as f32);
        for p in &paths {
            let b = p.bounds();
            box0 = (box0.0.max(b.left()), box0.1.max(b.top()));
            box1 = (box1.0.min(b.right()), box1.1.min(b.bottom()));
        }
        let bx0 = box0.0.floor().clamp(0.0, pm_w as f32) as i32;
        let by0 = box0.1.floor().clamp(0.0, pm_h as f32) as i32;
        let bx1 = box1.0.ceil().clamp(0.0, pm_w as f32) as i32;
        let by1 = box1.1.ceil().clamp(0.0, pm_h as f32) as i32;
        if bx0 >= bx1 || by0 >= by1 {
            // Nothing of the clip is on the canvas: a zeroed mask, which the
            // span math and the canvas-sized copy both treat as "draw nothing".
            if let Some(mask) = Mask::new(1, 1) {
                self.clip = Clip::Mask {
                    mask: Box::new(mask),
                    left: 0,
                    top: 0,
                    canvas_mask: None,
                };
            }
            return;
        }
        if let Some(mut mask) = Mask::new((bx1 - bx0) as u32, (by1 - by0) as u32) {
            mask.invert();
            let ts = Transform::from_translate(-(bx0 as f32), -(by0 as f32));
            for p in &paths {
                mask.intersect_path(p, FillRule::Winding, true, ts);
            }
            self.clip = Clip::Mask {
                mask: Box::new(mask),
                left: bx0,
                top: by0,
                canvas_mask: None,
            };
        }
    }

    /// The clip as a canvas-sized mask, for the `tiny_skia` calls that take
    /// one. Built on first use and kept until the clip changes.
    fn ensure_canvas_mask(&mut self) {
        let (pm_w, pm_h) = (self.pm.width(), self.pm.height());
        let next: Option<Clip> = match &mut self.clip {
            Clip::None => None,
            Clip::Rect(r) => {
                let r = *r;
                let (bx0, by0, bw, bh) = rect_box(r, pm_w, pm_h);
                let empty = bw == 0 || bh == 0;
                let bbox = if empty {
                    Mask::new(1, 1)
                } else {
                    Mask::new(bw, bh).map(|mut m| {
                        if let Some(p) = shapes::rect_path(r) {
                            let ts = Transform::from_translate(-(bx0 as f32), -(by0 as f32));
                            m.fill_path(&p, FillRule::Winding, true, ts);
                        }
                        m
                    })
                };
                bbox.map(|mask| {
                    let (left, top) = if empty { (0, 0) } else { (bx0, by0) };
                    let canvas_mask = canvas_from(&mask, left, top, pm_w, pm_h).map(Box::new);
                    Clip::Mask {
                        mask: Box::new(mask),
                        left,
                        top,
                        canvas_mask,
                    }
                })
            }
            Clip::Mask {
                mask,
                left,
                top,
                canvas_mask,
            } => {
                if canvas_mask.is_none() {
                    *canvas_mask = canvas_from(mask, *left, *top, pm_w, pm_h).map(Box::new);
                }
                None
            }
        };
        if let Some(clip) = next {
            self.clip = clip;
        }
    }

    fn fill(&mut self, path: &Path, color: Rgba, rule: FillRule) {
        if color.a == 0 {
            return;
        }
        // Cheap rejection: entirely outside the canvas.
        let b = path.bounds();
        let visible = b.right() > 0.0
            && b.bottom() > 0.0
            && b.left() < self.pm.width() as f32
            && b.top() < self.pm.height() as f32;
        if !visible {
            return;
        }
        // Small fills (icons, badges, avatars, dots) go through the exact-area
        // coverage rasterizer, which matches Chrome's antialiasing on curved
        // edges; `coverage`'s cost grows with the shape's height, so large fills
        // (media placeholders, backgrounds) keep tiny-skia's scanline AA, whose
        // error is proportional to the edge length rather than the area.
        let left = b.left().floor();
        let top = b.top().floor();
        let w = (b.right().ceil() - left).max(0.0) as u32;
        let h = (b.bottom().ceil() - top).max(0.0) as u32;
        if w > 0 && h > 0 && w * h <= SMALL_FILL_PIXELS {
            if let Some(cov) = coverage::mask_small(path, w, h, rule, left, top) {
                self.blit(&cov, w, h, left as i32, top as i32, color);
                return;
            }
        }
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color_rgba8(color.r, color.g, color.b, color.a);
        // A mask is only needed when the fill is not already inside the clip:
        // inside an axis-aligned clip rectangle `tiny_skia` would multiply
        // every pixel by 255.
        let inside = match self.clip {
            Clip::None => true,
            Clip::Rect(r) => {
                b.left() >= r.x
                    && b.top() >= r.y
                    && b.right() <= r.x + r.w
                    && b.bottom() <= r.y + r.h
            }
            Clip::Mask { .. } => false,
        };
        if !inside {
            self.ensure_canvas_mask();
        }
        let Renderer { pm, clip, .. } = self;
        let mask = match clip {
            Clip::Mask { canvas_mask, .. } => canvas_mask.as_deref(),
            _ => None,
        };
        pm.fill_path(path, &paint, rule, Transform::identity(), mask);
    }

    /// A CSS border, inset hairline or inset shadow: a ring whose outer edge is
    /// the shape and whose inner edge is the shape offset inward by `width`.
    ///
    /// Blink renders these by stroking the border path and clipping to the
    /// box, and for a squircle the inner curve is a true parallel offset, not
    /// another superellipse. So we stroke with double the width into a scratch
    /// pixmap, masked by the shape, then composite. For circular corners this
    /// is identical to the classic "inner radius = r − width" ring.
    fn stroke_inset(&mut self, path: &Path, color: Rgba, width: f32) {
        if color.a == 0 || !width.is_finite() || width <= 0.0 {
            return;
        }
        let b = path.bounds();
        let left = (b.left() - width - 1.0).floor();
        let top = (b.top() - width - 1.0).floor();
        let right = (b.right() + width + 1.0).ceil();
        let bottom = (b.bottom() + width + 1.0).ceil();
        let (x0, y0) = (left.max(0.0), top.max(0.0));
        let (x1, y1) = (
            right.min(self.pm.width() as f32),
            bottom.min(self.pm.height() as f32),
        );
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let (Some(mut tmp), Some(mut mask)) = (SkPixmap::new(w, h), Mask::new(w, h)) else {
            return;
        };
        let ts = Transform::from_translate(-x0, -y0);
        mask.fill_path(path, FillRule::Winding, true, ts);
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color_rgba8(color.r, color.g, color.b, color.a);
        let stroke = tiny_skia::Stroke {
            width: width * 2.0,
            ..tiny_skia::Stroke::default()
        };
        tmp.stroke_path(path, &paint, &stroke, ts, Some(&mask));
        self.blit_rgba(tmp.data(), w, h, x0 as i32, y0 as i32, Rgba::WHITE);
    }

    fn fill_shader(&mut self, path: &Path, shader: tiny_skia::Shader, rule: FillRule) {
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        };
        self.ensure_canvas_mask();
        let Renderer { pm, clip, .. } = self;
        let mask = match clip {
            Clip::Mask { canvas_mask, .. } => canvas_mask.as_deref(),
            _ => None,
        };
        pm.fill_path(path, &paint, rule, Transform::identity(), mask);
    }

    fn glyphs(
        &mut self,
        x: f32,
        y: f32,
        face: u16,
        size: f32,
        color: Rgba,
        glyphs: &[display::GlyphPos],
    ) {
        if !size.is_finite() || size <= 0.0 || !x.is_finite() || !y.is_finite() || color.a == 0 {
            return;
        }
        let fonts = db();
        let Some(f) = fonts.faces.get(face as usize) else {
            return;
        };
        let size_dev = size * self.dpr;
        if size_dev > 1024.0 {
            return;
        }
        let emoji = f.kind == FaceKind::Emoji;
        let base_x = x * self.dpr;
        for g in glyphs {
            if !g.x.is_finite() || !g.y.is_finite() {
                continue;
            }
            let gx = base_x + g.x * self.dpr;
            let gy = snap_baseline(y + g.y) * self.dpr;
            if emoji {
                match emoji::sprite(f, g.id, size, self.dpr, gx, gy) {
                    Some(s) => {
                        let (left, top) = (gx.floor() as i32 + s.left, gy.floor() as i32 + s.top);
                        self.blit_rgba(&s.data, s.w, s.h, left, top, color);
                    }
                    // `.notdef` (glyph 0) is an emoji newer than the font;
                    // non-zero glyphs with no bitmap are AAT deleted/ignorable
                    // glyphs (0xFFFF, VS16, ZWJ) inside a valid sequence.
                    None if g.id == 0 => self.missing_emoji(
                        gx,
                        gy,
                        size_dev,
                        fonts::emoji_advance(size) * self.dpr,
                        color,
                    ),
                    None => {}
                }
                continue;
            }
            // The mask's phase is the 1/4 px bucket the position rounds into, so
            // the blit has to use the *quantized* origin: 10.9 is drawn as 11.0,
            // not as 10 plus a 0.75 phase (see glyph::quantize).
            let gxq = glyph::quantize(gx);
            let Some(b) = glyph::bitmap(face, g.id, size_dev, gxq, gy) else {
                continue;
            };
            let left = gxq.floor() as i32 + b.left;
            let top = gy.floor() as i32 + b.top;
            self.blit(&b.data, b.w, b.h, left, top, color);
        }
    }

    /// A stand-in for an emoji the bundled artwork lacks: a rounded box the
    /// size of the em square, centered in the advance, in a translucent tint of
    /// the text color so it reads as "missing" in either theme.
    fn missing_emoji(&mut self, x: f32, y: f32, size: f32, advance: f32, color: Rgba) {
        let side = size * 0.85;
        let x0 = x + (advance - side) / 2.0;
        let y0 = y - size * 0.8;
        let r = side * 0.15;
        let rect = Rect {
            x: x0,
            y: y0,
            w: side,
            h: side,
        };
        let shape = Shape {
            rect,
            radii: display::Radii::all(r),
            corner: display::CornerShape::Round,
        };
        if let Some(path) = shapes::path(&shape, 1.0) {
            let tint = Rgba { a: 70, ..color };
            self.fill(&path, tint, FillRule::Winding);
        }
    }

    /// Blends an 8-bit coverage mask with `color`.
    fn blit(&mut self, cov: &[u8], w: u32, h: u32, left: i32, top: i32, color: Rgba) {
        if w == 0 || h == 0 {
            return;
        }
        let (x0, y0, x1, y1) = self.span(left, top, w, h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        // Borrow the canvas and the clip together rather than cloning the mask.
        let Renderer { pm, clip, .. } = self;
        let (mask, m_left, m_top) = match clip {
            Clip::Mask {
                mask,
                left: mx,
                top: my,
                ..
            } => (Some(&**mask), *mx, *my),
            _ => (None, 0, 0),
        };
        let mw = mask.map_or(0, |m| m.width() as usize);
        let pm_w = pm.width() as usize;
        let data = pm.data_mut();
        let sa = color.a as u32;
        let span_w = (x1 - x0) as usize;
        let sx0 = (x0 - left) as usize;

        // Fast path: unmasked opaque text color (the common case for post text).
        if mask.is_none() && sa == 255 {
            let (cr, cg, cb) = (color.r, color.g, color.b);
            for py in y0..y1 {
                let s_off = (py - top) as usize * w as usize + sx0;
                let cov_row = &cov[s_off..s_off + span_w];
                let d_off = (py as usize * pm_w + x0 as usize) * 4;
                let dst_row = data[d_off..d_off + span_w * 4].as_chunks_mut::<4>().0;
                for (&c8, dst) in cov_row.iter().zip(dst_row) {
                    if c8 == 0 {
                        continue;
                    }
                    if c8 == 255 {
                        *dst = [cr, cg, cb, 255];
                    } else {
                        let a = c8 as u32;
                        let inv = 255 - a;
                        let sr = (cr as u32 * a + 127) / 255;
                        let sg = (cg as u32 * a + 127) / 255;
                        let sb = (cb as u32 * a + 127) / 255;
                        dst[0] = (sr + (dst[0] as u32 * inv + 127) / 255).min(255) as u8;
                        dst[1] = (sg + (dst[1] as u32 * inv + 127) / 255).min(255) as u8;
                        dst[2] = (sb + (dst[2] as u32 * inv + 127) / 255).min(255) as u8;
                        dst[3] = if dst[3] == 255 {
                            255
                        } else {
                            (a + (dst[3] as u32 * inv + 127) / 255).min(255) as u8
                        };
                    }
                }
            }
            return;
        }

        for py in y0..y1 {
            let row = (py - top) as usize;
            let mut di = (py as usize * pm_w + x0 as usize) * 4;
            for (si, px) in (row * w as usize + sx0..).zip(x0..x1) {
                let mut c = cov[si] as u32;
                if let Some(m) = mask {
                    let mv = m.data()[(py - m_top) as usize * mw + (px - m_left) as usize] as u32;
                    c = (c * mv + 127) / 255;
                }
                if c == 0 {
                    di += 4;
                    continue;
                }
                let a = (sa * c + 127) / 255;
                if a == 0 {
                    di += 4;
                    continue;
                }
                if a == 255 {
                    data[di] = color.r;
                    data[di + 1] = color.g;
                    data[di + 2] = color.b;
                    data[di + 3] = 255;
                } else {
                    let inv = 255 - a;
                    let src = |ch: u8| (ch as u32 * a + 127) / 255;
                    for k in 0..3 {
                        let s = match k {
                            0 => src(color.r),
                            1 => src(color.g),
                            _ => src(color.b),
                        };
                        let d = data[di + k] as u32;
                        data[di + k] = (s + (d * inv + 127) / 255).min(255) as u8;
                    }
                    data[di + 3] = (a + (data[di + 3] as u32 * inv + 127) / 255).min(255) as u8;
                }
                di += 4;
            }
        }
    }

    /// Blends a premultiplied RGBA sprite (color emoji).
    fn blit_rgba(&mut self, src: &[u8], w: u32, h: u32, left: i32, top: i32, color: Rgba) {
        if w == 0 || h == 0 {
            return;
        }
        let (x0, y0, x1, y1) = self.span(left, top, w, h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        // Borrow the canvas and the clip together rather than cloning the mask.
        let Renderer { pm, clip, .. } = self;
        let (mask, m_left, m_top) = match clip {
            Clip::Mask {
                mask,
                left: mx,
                top: my,
                ..
            } => (Some(&**mask), *mx, *my),
            _ => (None, 0, 0),
        };
        let mw = mask.map_or(0, |m| m.width() as usize);
        let pm_w = pm.width() as usize;
        let data = pm.data_mut();
        // An alpha multiplier on the item color scales the sprite; emoji are
        // opaque so this is 255 in practice.
        let alpha = color.a as u32;
        let span_w = (x1 - x0) as usize;
        let sx0 = (x0 - left) as usize;

        // Fast path: unmasked sprite at full item alpha (every normal emoji draw).
        if mask.is_none() && alpha == 255 {
            for py in y0..y1 {
                let s_off = ((py - top) as usize * w as usize + sx0) * 4;
                let d_off = (py as usize * pm_w + x0 as usize) * 4;
                let src_row = src[s_off..s_off + span_w * 4].as_chunks::<4>().0;
                let dst_row = data[d_off..d_off + span_w * 4].as_chunks_mut::<4>().0;
                for (sp, dp) in src_row.iter().zip(dst_row) {
                    let sa = sp[3];
                    if sa == 0 {
                        continue;
                    }
                    if sa == 255 {
                        *dp = *sp;
                    } else {
                        let inv = 255 - sa as u32;
                        dp[0] = (sp[0] as u32 + (dp[0] as u32 * inv + 127) / 255).min(255) as u8;
                        dp[1] = (sp[1] as u32 + (dp[1] as u32 * inv + 127) / 255).min(255) as u8;
                        dp[2] = (sp[2] as u32 + (dp[2] as u32 * inv + 127) / 255).min(255) as u8;
                        dp[3] = if dp[3] == 255 {
                            255
                        } else {
                            (sa as u32 + (dp[3] as u32 * inv + 127) / 255).min(255) as u8
                        };
                    }
                }
            }
            return;
        }

        for py in y0..y1 {
            let mut si = ((py - top) as usize * w as usize + sx0) * 4;
            let mut di = (py as usize * pm_w + x0 as usize) * 4;
            for px in x0..x1 {
                // The source is premultiplied: its color channels already carry
                // its own alpha. Coverage (the clip mask) and the item's alpha
                // scale color and alpha *together*; multiplying the color by the
                // source alpha again would square it (a 15% white hairline came
                // out at 2%).
                let mut scale = 255u32;
                if let Some(m) = mask {
                    let mv = m.data()[(py - m_top) as usize * mw + (px - m_left) as usize] as u32;
                    scale = (scale * mv + 127) / 255;
                }
                if alpha != 255 {
                    scale = (scale * alpha + 127) / 255;
                }
                let sa = (src[si + 3] as u32 * scale + 127) / 255;
                if sa == 0 {
                    si += 4;
                    di += 4;
                    continue;
                }
                if sa == 255 {
                    data[di] = src[si];
                    data[di + 1] = src[si + 1];
                    data[di + 2] = src[si + 2];
                    data[di + 3] = 255;
                } else {
                    let inv = 255 - sa;
                    for k in 0..3 {
                        let s = (src[si + k] as u32 * scale + 127) / 255;
                        let d = data[di + k] as u32;
                        data[di + k] = (s + (d * inv + 127) / 255).min(255) as u8;
                    }
                    data[di + 3] = (sa + (data[di + 3] as u32 * inv + 127) / 255).min(255) as u8;
                }
                si += 4;
                di += 4;
            }
        }
    }

    /// Visible device-pixel span of a `w × h` block placed at (`left`, `top`),
    /// intersected with the canvas and the current clip.
    fn span(&self, left: i32, top: i32, w: u32, h: u32) -> (i32, i32, i32, i32) {
        let (mut x0, mut y0) = (left, top);
        let (mut x1, mut y1) = (left.saturating_add(w as i32), top.saturating_add(h as i32));
        x0 = x0.max(0);
        y0 = y0.max(0);
        x1 = x1.min(self.pm.width() as i32);
        y1 = y1.min(self.pm.height() as i32);
        match &self.clip {
            Clip::Rect(r) => {
                x0 = x0.max(r.x.floor() as i32);
                y0 = y0.max(r.y.floor() as i32);
                x1 = x1.min((r.x + r.w).ceil() as i32);
                y1 = y1.min((r.y + r.h).ceil() as i32);
            }
            Clip::Mask {
                mask,
                left: mx,
                top: my,
                ..
            } => {
                x0 = x0.max(*mx);
                y0 = y0.max(*my);
                x1 = x1.min(mx.saturating_add(mask.width() as i32));
                y1 = y1.min(my.saturating_add(mask.height() as i32));
            }
            Clip::None => {}
        }
        if x0 >= x1 || y0 >= y1 {
            return (0, 0, 0, 0);
        }
        (x0, y0, x1, y1)
    }

    fn icon(&mut self, name: &str, rect: &Rect, color: Rgba) {
        icons::draw(self, name, rect, color);
    }

    fn image(&mut self, shape: &Shape, image: &ImageRef, focus: (f32, f32), service: bool) {
        images::draw(self, shape, image, focus, service);
    }

    fn placeholder(&mut self, shape: &Shape, label: &str) {
        images::placeholder(self, shape, label);
    }
}

/// The CSS y a glyph baseline is actually painted at.
///
/// Blink snaps a text run's baseline to whole CSS pixels, and the painter has to
/// do the same or every glyph edge is half a pixel off. Measured in Chrome with
/// `--font-render-hinting=none --disable-lcd-text` by nudging a line of text
/// with `position: relative`: at dpr 1, 2 and 3 an offset of 0.4 px changes no
/// pixel, and 0.5 px moves the text by exactly one CSS pixel — so the snap is in
/// CSS space, not device space (at dpr 3 a device snap would have moved it by a
/// row at 0.4 px). Horizontal positions are *not* snapped: they are sub-pixel
/// (a 0.1 px left offset changes the rasterization), which is why only `y` goes
/// through here.
fn snap_baseline(css_y: f32) -> f32 {
    if css_y.is_finite() {
        css_y.round()
    } else {
        css_y
    }
}

/// The device-space rectangle of a shape with no radii, if it has one.
fn plain_rect(s: &Shape, dpr: f32) -> Option<Rect> {
    if s.corner != display::CornerShape::Round {
        return None;
    }
    let r = s.radii;
    if r.tl > 0.0 || r.tr > 0.0 || r.br > 0.0 || r.bl > 0.0 {
        return None;
    }
    let rect = shapes::scale_rect(s.rect, dpr);
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return None;
    }
    Some(rect)
}

/// The integer box a device-space rectangle covers on a `w × h` canvas.
fn rect_box(r: Rect, w: u32, h: u32) -> (i32, i32, u32, u32) {
    let bx0 = r.x.floor().clamp(0.0, w as f32) as i32;
    let by0 = r.y.floor().clamp(0.0, h as f32) as i32;
    let bx1 = (r.x + r.w).ceil().clamp(0.0, w as f32) as i32;
    let by1 = (r.y + r.h).ceil().clamp(0.0, h as f32) as i32;
    (
        bx0,
        by0,
        (bx1 - bx0).max(0) as u32,
        (by1 - by0).max(0) as u32,
    )
}

/// A canvas-sized mask holding `src` at (`left`, `top`); zero elsewhere.
fn canvas_from(src: &Mask, left: i32, top: i32, w: u32, h: u32) -> Option<Mask> {
    let mut full = Mask::new(w, h)?;
    let sw = src.width() as isize;
    let sh = src.height() as isize;
    let x0 = (left as isize).max(0);
    let x1 = (left as isize + sw).min(w as isize);
    let y0 = (-(top as isize)).max(0);
    let y1 = (h as isize - top as isize).min(sh);
    if x1 > x0 && y1 > y0 {
        let n = (x1 - x0) as usize;
        let sdata = src.data();
        let ddata = full.data_mut();
        let dw = w as usize;
        for row in y0..y1 {
            let s = row as usize * sw as usize + (x0 - left as isize) as usize;
            let d = (top as isize + row) as usize * dw + x0 as usize;
            ddata[d..d + n].copy_from_slice(&sdata[s..s + n]);
        }
    }
    Some(full)
}

// Accessors for the SVG writer, which describes the same geometry in CSS
// pixels rather than device pixels, and for the shape oracle.

pub(crate) fn shape_path(shape: &Shape, dpr: f32) -> Option<Path> {
    shapes::path(shape, dpr)
}

pub(crate) fn glyph_path(face: &crate::fonts::Face, glyph: u32, size: f32) -> Option<Path> {
    glyph::outline_path(face, glyph, size)
}

pub(crate) fn icon_body(name: &str) -> Option<&'static str> {
    icons::body(name)
}

/// Debug helper for the shape oracle: the path an item would fill.
#[doc(hidden)]
pub fn dbg_shape_path(shape: &Shape, dpr: f32) -> Option<Path> {
    shapes::path(shape, dpr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{CornerShape, Item, Radii};

    fn frame(items: Vec<Item>) -> DisplayList {
        let mut list = DisplayList::new(40.0, 30.0, Rgba::WHITE);
        list.items = items;
        list
    }

    #[test]
    fn fills_cover_the_canvas() {
        let list = frame(vec![Item::Fill {
            shape: Shape {
                rect: Rect::new(0.0, 0.0, 40.0, 30.0),
                radii: Radii::all(0.0),
                corner: CornerShape::Round,
            },
            color: Rgba::hex(0x1D9BF0),
        }]);
        let p = rasterize(&list, 2.0, &NoImages);
        assert_eq!((p.width, p.height), (80, 60));
        assert_eq!(p.pixel(40, 30), Some(Rgba::hex(0x1D9BF0)));
        assert_eq!(p.pixel(0, 0), Some(Rgba::hex(0x1D9BF0)));
    }

    #[test]
    fn png_round_trips_through_the_decoder() {
        let list = frame(vec![Item::Fill {
            shape: Shape::rounded(Rect::new(4.0, 4.0, 32.0, 22.0), 6.0),
            color: Rgba::hex(0xEF4444),
        }]);
        let png = rasterize_png(&list, 1.0, &NoImages);
        let img = decode_image(&png, &Limits::default()).expect("decode");
        assert_eq!((img.width, img.height), (40, 30));
        assert_eq!(&img.rgba[..4], &[255, 255, 255, 255]);
        // The middle of the rounded rect is the fill colour.
        let o = (15 * 40 + 20) * 4;
        assert_eq!(&img.rgba[o..o + 4], &[0xEF, 0x44, 0x44, 255]);
    }

    #[test]
    fn determinism_with_a_warm_cache() {
        let list = frame(vec![
            Item::Fill {
                shape: Shape::rounded(Rect::new(2.0, 2.0, 30.0, 20.0), 4.0),
                color: Rgba::hex(0x0088CC),
            },
            Item::Icon {
                name: "icon-heart".into(),
                rect: Rect::new(8.0, 6.0, 16.0, 16.0),
                color: Rgba::hex(0x536471),
            },
        ]);
        let a = rasterize(&list, 3.0, &NoImages).rgba;
        let b = rasterize(&list, 3.0, &NoImages).rgba;
        assert_eq!(a, b, "the glyph and icon caches must not change output");
    }

    #[test]
    fn absurd_geometry_is_skipped_not_fatal() {
        let list = frame(vec![
            Item::Fill {
                shape: Shape {
                    rect: Rect::new(f32::NAN, 0.0, 10.0, 10.0),
                    ..Shape::default()
                },
                color: Rgba::BLACK,
            },
            Item::Fill {
                shape: Shape {
                    rect: Rect::new(0.0, 0.0, f32::INFINITY, 10.0),
                    ..Shape::default()
                },
                color: Rgba::BLACK,
            },
            Item::Fill {
                shape: Shape {
                    rect: Rect::new(0.0, 0.0, -5.0, -5.0),
                    ..Shape::default()
                },
                color: Rgba::BLACK,
            },
            Item::Glyphs {
                x: f32::NAN,
                y: 0.0,
                face: 999,
                size: 12.0,
                color: Rgba::BLACK,
                glyphs: vec![display::GlyphPos {
                    id: 1,
                    x: 0.0,
                    y: 0.0,
                }],
            },
            Item::PushClip {
                shape: Shape::rounded(Rect::new(0.0, 0.0, 1.0, 1.0), 2.0),
            },
            Item::Image {
                shape: Shape::default(),
                image: ImageRef::Media(0),
                focus: (f32::NAN, 0.0),
                service: false,
            },
            Item::Placeholder {
                shape: Shape::rounded(Rect::new(20.0, 20.0, 10.0, 8.0), 2.0),
                label: "Photo".into(),
            },
        ]);
        let p = rasterize(&list, 2.0, &NoImages);
        assert_eq!((p.width, p.height), (80, 60));
        // The dangling clip must not swallow later items.
        assert!(p.pixel(21, 21).is_some());
    }

    #[test]
    fn invalid_dpr_falls_back_to_one() {
        let list = frame(vec![]);
        let p = rasterize(&list, f32::NAN, &NoImages);
        assert_eq!((p.width, p.height), (40, 30));
    }

    #[test]
    fn decode_rejects_non_images_and_respects_limits() {
        assert_eq!(
            decode_image(b"not an image at all", &Limits::default()),
            Err(DecodeError::Unsupported)
        );
        let list = frame(vec![Item::Fill {
            shape: Shape::default(),
            color: Rgba::BLACK,
        }]);
        let png = rasterize_png(&list, 1.0, &NoImages);
        let tight = Limits {
            max_dimension: 8,
            ..Limits::default()
        };
        assert_eq!(decode_image(&png, &tight), Err(DecodeError::TooLarge));
        let small = Limits {
            max_pixels: 16,
            ..Limits::default()
        };
        assert_eq!(decode_image(&png, &small), Err(DecodeError::TooLarge));
    }

    #[test]
    fn squircle_path_stays_inside_its_box() {
        let shape = Shape {
            rect: Rect::new(4.0, 4.0, 32.0, 22.0),
            radii: Radii::all(8.0),
            corner: CornerShape::Squircle,
        };
        let path = dbg_shape_path(&shape, 2.0).expect("path");
        let b = path.bounds();
        assert!(b.left() >= 7.9 && b.top() >= 7.9, "{b:?}");
        assert!(b.right() <= 72.1 && b.bottom() <= 52.1, "{b:?}");
        // The superellipse and the circle have the same box; the Bézier control
        // points of the superellipse reach a hair past it.
        let round = Shape {
            corner: CornerShape::Round,
            ..shape
        };
        let w = dbg_shape_path(&round, 2.0)
            .map(|p| p.bounds().width())
            .unwrap_or(0.0);
        assert!((b.width() - w).abs() < 0.2, "{} vs {w}", b.width());
    }

    #[test]
    fn images_scale_without_dark_edges() {
        // A 2x2 image: opaque white, opaque white, transparent red, opaque
        // white. Scaled up, the transparent corner must not smear dark red
        // into the white neighbours (the sampler premultiplies), and the
        // whole cell must stay inside the rounded rect.
        let mut src = DecodedImage {
            width: 2,
            height: 2,
            rgba: vec![255; 16],
        };
        src.rgba[8..12].copy_from_slice(&[255, 0, 0, 0]);
        let srcs = vec![(ImageRef::Media(0), src)];
        let list = frame(vec![Item::Image {
            shape: Shape::rounded(Rect::new(4.0, 4.0, 32.0, 22.0), 4.0),
            image: ImageRef::Media(0),
            focus: (0.0, 0.0),
            service: false,
        }]);
        let p = rasterize(&list, 4.0, &srcs);
        let mid = p.pixel(80, 60).unwrap();
        assert_eq!(
            (mid.r, mid.g, mid.b, mid.a),
            (255, 255, 255, 255),
            "{mid:?}"
        );
        // Just inside the top-left corner is also white, not a red halo.
        let near = p.pixel(40, 40).unwrap();
        assert!(near.r >= 250 && near.g >= 250 && near.b >= 250, "{near:?}");
    }

    #[test]
    fn images_decode_once_per_draw_when_the_source_caches() {
        // `rgba_ref` is the zero-copy path; `rgba` must not be called when a
        // source can answer by reference (a 1600x900 photo is 5.8 MB).
        struct Cached {
            img: DecodedImage,
            calls: std::cell::Cell<usize>,
        }
        impl ImageSource for Cached {
            fn rgba(&self, _image: &ImageRef) -> Option<DecodedImage> {
                self.calls.set(self.calls.get() + 1);
                Some(self.img.clone())
            }
            fn rgba_ref(&self, _image: &ImageRef) -> Option<&DecodedImage> {
                Some(&self.img)
            }
        }
        let mut rgba = Vec::with_capacity(8 * 8 * 4);
        for _ in 0..64 {
            rgba.extend_from_slice(&[200, 200, 200, 255]);
        }
        let cached = Cached {
            img: DecodedImage {
                width: 8,
                height: 8,
                rgba,
            },
            calls: std::cell::Cell::new(0),
        };
        let list = frame(vec![Item::Image {
            shape: Shape::rounded(Rect::new(0.0, 0.0, 20.0, 20.0), 2.0),
            image: ImageRef::Media(0),
            focus: (0.5, 0.5),
            service: false,
        }]);
        let p = rasterize(&list, 2.0, &cached);
        assert_eq!(cached.calls.get(), 0, "rgba() should not be needed");
        assert_eq!(p.pixel(20, 20).map(|c| c.r), Some(200));
    }

    #[test]
    fn images_respect_a_clip() {
        // The painter composites images straight into the canvas now; the clip
        // still has to be honoured, and a rounded shape still has to be covered.
        let mut src = DecodedImage {
            width: 4,
            height: 4,
            rgba: Vec::new(),
        };
        for _ in 0..16 {
            src.rgba.extend_from_slice(&[0, 128, 255, 255]);
        }
        let srcs = vec![(ImageRef::Media(0), src)];
        let list = frame(vec![
            Item::PushClip {
                shape: Shape::rect(Rect::new(0.0, 0.0, 20.0, 15.0)),
            },
            Item::Image {
                shape: Shape::rect(Rect::new(0.0, 0.0, 40.0, 30.0)),
                image: ImageRef::Media(0),
                focus: (0.5, 0.5),
                service: false,
            },
            Item::PopClip,
        ]);
        let p = rasterize(&list, 1.0, &srcs);
        assert_eq!(p.pixel(5, 5).map(|c| c.b), Some(255), "inside the clip");
        assert_eq!(p.pixel(30, 25), Some(Rgba::WHITE), "outside the clip");
    }

    #[test]
    fn svg_export_mentions_every_item() {
        let list = frame(vec![
            Item::Fill {
                shape: Shape::rounded(Rect::new(0.0, 0.0, 20.0, 20.0), 4.0),
                color: Rgba::hex(0x123456),
            },
            Item::Icon {
                name: "icon-heart".into(),
                rect: Rect::new(4.0, 4.0, 12.0, 12.0),
                color: Rgba::BLACK,
            },
            Item::Image {
                shape: Shape::default(),
                image: ImageRef::Avatar,
                focus: (0.5, 0.5),
                service: false,
            },
        ]);
        let svg = to_svg(&list);
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("#123456"));
        assert!(svg.contains("icon-heart") || svg.contains("<path"));
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    /// Blink snaps text baselines to whole CSS pixels; the painter has to agree
    /// or the meta text (13px, half-pixel baselines) lands half a pixel off.
    #[test]
    fn baselines_snap_to_whole_css_pixels() {
        assert_eq!(snap_baseline(27.0), 27.0);
        assert_eq!(snap_baseline(80.5), 81.0);
        assert_eq!(snap_baseline(80.25), 80.0);
        assert_eq!(snap_baseline(80.75), 81.0);
        assert_eq!(snap_baseline(0.4), 0.0);
        assert_eq!(snap_baseline(0.6), 1.0);
        assert!(snap_baseline(f32::NAN).is_nan());
    }

    /// A fractional baseline is rasterized on the pixel grid Chrome uses, so the
    /// ink starts at the snapped device row rather than half a row lower.
    #[test]
    fn fractional_baseline_rasterizes_on_the_snapped_row() {
        let glyphs = vec![display::GlyphPos {
            id: db().faces[0].glyph('H').expect("H"),
            x: 0.0,
            y: 0.0,
        }];
        let run = |y: f32| {
            frame(vec![Item::Glyphs {
                x: 2.0,
                y,
                face: 0,
                size: 13.0,
                color: Rgba::BLACK,
                glyphs: glyphs.clone(),
            }])
        };
        // 10.5 snaps to 11, which is also where an exact 11 lands; 10.4 snaps
        // to 10, so the two renders differ from 10.5.
        assert_eq!(
            rasterize(&run(10.5), 1.0, &NoImages).rgba,
            rasterize(&run(11.0), 1.0, &NoImages).rgba
        );
        assert_ne!(
            rasterize(&run(10.5), 1.0, &NoImages).rgba,
            rasterize(&run(10.0), 1.0, &NoImages).rgba
        );
    }

    /// X's `_bigger` affiliation badges are 73×73 (an odd pixel count: 5,329)
    /// and drawn into a 15×15 `rounded-xs` squircle. Downscaling past 2× hits
    /// a mip level ≥ 1, which used to misread the odd tail in `is_opaque`
    /// and wrap `acc[3]` to zero alpha in `box_down`.
    #[test]
    fn odd_dimension_squircle_badge_paints_at_every_dpr() {
        let src = DecodedImage {
            width: 73,
            height: 73,
            rgba: [28u8, 28, 28, 255].repeat(73 * 73),
        };
        let srcs = vec![(ImageRef::AffiliateBadge, src)];
        let badge = Shape {
            rect: Rect::new(4.0, 4.0, 15.0, 15.0),
            radii: Radii::all(7.2),
            corner: display::CornerShape::Squircle,
        };
        let list = frame(vec![Item::Image {
            shape: badge,
            image: ImageRef::AffiliateBadge,
            focus: (0.5, 0.5),
            service: false,
        }]);
        for dpr in [1.0, 2.0, 3.0] {
            let p = rasterize(&list, dpr, &srcs);
            let cx = (11.5 * dpr) as u32;
            let cy = (11.5 * dpr) as u32;
            let px = p.pixel(cx, cy).expect("center pixel");
            assert!(
                px.r < 60 && px.a == 255,
                "badge invisible at dpr {dpr}: {px:?}"
            );
        }
    }
}
