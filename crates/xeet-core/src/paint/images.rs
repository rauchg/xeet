//! Raster images: decoding, `object-fit: cover` scaling, and placeholders.
//!
//! ## Decoding
//!
//! PNG, JPEG and WebP, in pure Rust. Input is hostile: every decoder is
//! bounded by [`Limits`] *before* it allocates, and a malformed file is an
//! error, never a panic. Callers that only need layout can pass no bytes at
//! all and get placeholders (which is also the fastest path).
//!
//! ## Scaling
//!
//! Chrome samples scaled images with bilinear filtering over a mipmap chain
//! (`kLow_SkFilterQuality`). We reproduce that: a box-filtered pyramid, the
//! level chosen by downscale factor, then bilinear inside the level. Sampling
//! happens in premultiplied space, as Skia does, so transparent edges do not
//! darken.
//!
//! A link card's picture takes one step before that. X does not hand the
//! browser the page's `og:image`; it re-hosts it on its image service and the
//! client asks for a copy fitted to a box, the smallest of `name=small` (680),
//! `900x900`, `medium` (1200) and `large` (2048) that is wider than the
//! picture's slot in device pixels ([`service_box`]), and Chrome scales
//! *that*. Photos arrive
//! the same way, but the resolver can fetch their variant (`docs/MEDIA.md`); a
//! card's copy exists only on X's side, so the painter reproduces the
//! service's resize ([`service_copy`]) before sampling.
//!
//! ## Placeholders
//!
//! When bytes are absent the image becomes a neutral grey card with the label
//! from the display list, centered at 13px/16px in a slightly darker grey.
//! The greys are deliberately theme-neutral (`#CFD9DE` fill, `#536471` label —
//! two of X's own light-theme greys) because the painter does not know the
//! theme; the caller's display list already carries the colors it wants around
//! the image.

use crate::color::Rgba;
use crate::display::{ImageRef, Shape};
use crate::inline;
use crate::paint::{shapes, DecodedImage, ImageSource, PreparedImage, Renderer};
use tiny_skia::FillRule;

/// Hard limits for one image decode.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Longest side, in pixels.
    pub max_dimension: u32,
    /// Total pixels (width × height).
    pub max_pixels: u64,
    /// Bytes of decoded RGBA we are willing to allocate.
    pub max_bytes: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            // X serves media up to 4096 px; 8192 leaves room for originals.
            max_dimension: 8192,
            // 4096² — 64 MB of RGBA.
            max_pixels: 16_777_216,
            max_bytes: 64 << 20,
        }
    }
}

/// Why an image could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Not PNG, JPEG or WebP.
    Unsupported,
    /// The header is malformed or truncated.
    Malformed,
    /// Larger than [`Limits`] allows.
    TooLarge,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DecodeError::Unsupported => "unsupported image format",
            DecodeError::Malformed => "malformed image",
            DecodeError::TooLarge => "image exceeds limits",
        })
    }
}

impl std::error::Error for DecodeError {}

fn check(w: u32, h: u32, limits: &Limits) -> Result<(), DecodeError> {
    if w == 0 || h == 0 {
        return Err(DecodeError::Malformed);
    }
    if w > limits.max_dimension || h > limits.max_dimension {
        return Err(DecodeError::TooLarge);
    }
    let pixels = w as u64 * h as u64;
    if pixels > limits.max_pixels || pixels * 4 > limits.max_bytes as u64 {
        return Err(DecodeError::TooLarge);
    }
    Ok(())
}

/// Decodes PNG, JPEG or WebP bytes into straight-alpha RGBA.
pub fn decode_image(bytes: &[u8], limits: &Limits) -> Result<super::DecodedImage, DecodeError> {
    if bytes.len() < 12 {
        return Err(DecodeError::Malformed);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes, limits)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(bytes, limits)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        decode_webp(bytes, limits)
    } else {
        Err(DecodeError::Unsupported)
    }
}

fn decode_png(bytes: &[u8], limits: &Limits) -> Result<super::DecodedImage, DecodeError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Expand palette and tRNS to RGBA, strip 16-bit to 8-bit: everything the
    // display can use, in one pass, without a second buffer.
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(|_| DecodeError::Malformed)?;
    let info = reader.info();
    check(info.width, info.height, limits)?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|_| DecodeError::Malformed)?;
    let (w, h) = (frame.width, frame.height);
    let rgba = match frame.color_type {
        png::ColorType::Rgba => {
            buf.truncate(frame.buffer_size());
            buf
        }
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(w as usize * h as usize * 4);
            for px in buf[..frame.buffer_size()].as_chunks::<3>().0 {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity(w as usize * h as usize * 4);
            for px in buf[..frame.buffer_size()].as_chunks::<2>().0 {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity(w as usize * h as usize * 4);
            for &v in &buf[..frame.buffer_size()] {
                out.extend_from_slice(&[v, v, v, 255]);
            }
            out
        }
        png::ColorType::Indexed => return Err(DecodeError::Malformed),
    };
    if rgba.len() != w as usize * h as usize * 4 {
        return Err(DecodeError::Malformed);
    }
    Ok(super::DecodedImage {
        width: w,
        height: h,
        rgba,
    })
}

fn decode_jpeg(bytes: &[u8], limits: &Limits) -> Result<super::DecodedImage, DecodeError> {
    use zune_jpeg::zune_core::options::DecoderOptions;
    let mut opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
    opts = opts
        .set_max_width(limits.max_dimension as usize)
        .set_max_height(limits.max_dimension as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(bytes), opts);
    // Parse the header first: until it is parsed the decoder cannot say how
    // big the output will be, and a zero-length buffer would silently fail.
    decoder
        .decode_headers()
        .map_err(|_| DecodeError::Malformed)?;
    let (w, h) = {
        let info = decoder.info().ok_or(DecodeError::Malformed)?;
        (info.width as u32, info.height as u32)
    };
    check(w, h, limits)?;
    // Decode straight into our buffer: the decoder would otherwise build its
    // own (5.8 MB for a 1600×900 photo) and hand it over, which measured ~5%
    // slower and costs one extra full-size allocation.
    let size = decoder.output_buffer_size().ok_or(DecodeError::Malformed)?;
    let mut pixels = vec![0u8; size];
    decoder
        .decode_into(&mut pixels)
        .map_err(|_| DecodeError::Malformed)?;
    if pixels.len() != w as usize * h as usize * 4 {
        return Err(DecodeError::Malformed);
    }
    Ok(super::DecodedImage {
        width: w,
        height: h,
        rgba: pixels,
    })
}

fn decode_webp(bytes: &[u8], limits: &Limits) -> Result<super::DecodedImage, DecodeError> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|_| DecodeError::Malformed)?;
    let (w, h) = decoder.dimensions();
    check(w, h, limits)?;
    // The decoder writes RGBA only for images with alpha; everything else comes
    // out at three bytes per pixel, so the buffer has to be sized for that and
    // expanded, exactly as the PNG path does.
    let size = decoder.output_buffer_size().ok_or(DecodeError::Malformed)?;
    let mut raw = vec![0u8; size];
    decoder
        .read_image(&mut raw)
        .map_err(|_| DecodeError::Malformed)?;
    let pixels = w as usize * h as usize;
    let rgba = if decoder.has_alpha() {
        if raw.len() != pixels * 4 {
            return Err(DecodeError::Malformed);
        }
        raw
    } else {
        if raw.len() != pixels * 3 {
            return Err(DecodeError::Malformed);
        }
        let mut out = Vec::with_capacity(pixels * 4);
        for px in raw.as_chunks::<3>().0 {
            out.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        out
    };
    Ok(super::DecodedImage {
        width: w,
        height: h,
        rgba,
    })
}

// ------------------------------------------------------------------- drawing

/// One bilinear tap along an axis: the two texel indices and the weight of the
/// second, in 0..=256.
#[derive(Clone, Copy)]
struct Tap {
    a: u32,
    b: u32,
    w: u32,
}

impl Tap {
    #[inline]
    fn new(pos: f32, len: u32) -> Tap {
        let fx = pos - 0.5;
        let x0 = fx.floor();
        let t = (fx - x0) * 256.0;
        let last = len.saturating_sub(1) as f32;
        Tap {
            a: x0.clamp(0.0, last) as u32,
            b: (x0 + 1.0).clamp(0.0, last) as u32,
            w: t.round().clamp(0.0, 256.0) as u32,
        }
    }
}

/// Draws an image into the shape, cropped like `object-fit: cover`.
///
/// The transform is an axis-aligned scale plus a translation, so the source
/// coordinate and the bilinear weights depend only on the destination column
/// (or row) index. They are computed once per draw and the per-pixel work is
/// integer taps: no floats, no divisions, no bounds checks in the inner loop.
/// Both the upscale and the downscale case use the same loop; they differ only
/// in which mip level is sampled.
///
/// Pixels are composited straight into the canvas. A clip, a rounded corner and
/// the mask all fold into the same pass in the order the sprite path used to
/// apply them (coverage first, then the clip), so the result is the same bytes
/// without a full-size sprite buffer in between.
pub(crate) fn draw(
    r: &mut Renderer,
    shape: &Shape,
    image: &ImageRef,
    focus: (f32, f32),
    service: bool,
) {
    // Copy the `&dyn ImageSource` out of the renderer: the image borrow then
    // hangs off the source, not off `r`, so the canvas can still be borrowed
    // mutably below.
    let images = r.images;
    let prepared = images.prepared_ref(image);
    let owned = match prepared {
        Some(_) => None,
        None => fetch(images, image),
    };
    let src = match (prepared, &owned) {
        (Some(p), _) => p.decoded(),
        (None, Some(d)) => &**d,
        (None, None) => return placeholder(r, shape, ""),
    };
    if src.width == 0
        || src.height == 0
        || src.rgba.len() != src.width as usize * src.height as usize * 4
    {
        return placeholder(r, shape, "");
    }
    let dpr = r.dpr;
    // X hands the browser a copy of a card picture fitted to the service's
    // box, not the file; the client picks the box from the slot's device
    // pixels (`service_box`), and the copy is what gets sampled, as Chrome
    // samples it. A picture that already fits the box is the copy.
    let copy = if service {
        service_box(shape.rect.w, dpr)
    } else {
        None
    };
    let service: Option<std::sync::Arc<PreparedImage>> = match copy {
        Some(side) if src.width > side || src.height > side => Some(match prepared {
            Some(p) => p.service_copy(side),
            None => std::sync::Arc::new(PreparedImage::new(service_copy(src, side))),
        }),
        _ => None,
    };
    let (prepared, src) = match &service {
        Some(c) => (Some(&**c), c.decoded()),
        None => (prepared, src),
    };
    let dst = shapes::scale_rect(shape.rect, dpr);
    if !dst.w.is_finite() || !dst.h.is_finite() || dst.w <= 0.0 || dst.h <= 0.0 {
        return;
    }
    let (fx, fy) = (focus.0.clamp(0.0, 1.0), focus.1.clamp(0.0, 1.0));
    let (sw, sh) = (src.width as f32, src.height as f32);
    // cover: the scale that fills the box, then the focal point decides what
    // hangs off each edge (CSS object-position: x% of image at x% of box).
    let s = (dst.w / sw).max(dst.h / sh);
    if !s.is_finite() || s <= 0.0 {
        return;
    }
    let off_x = (sw * s - dst.w) * fx;
    let off_y = (sh * s - dst.h) * fy;

    let left = dst.x.floor() as i32;
    let top = dst.y.floor() as i32;
    let w = (dst.x + dst.w).ceil() as i32 - left;
    let h = (dst.y + dst.h).ceil() as i32 - top;
    if w <= 0 || h <= 0 || w > 16384 || h > 16384 {
        return;
    }

    // The sampler picks the mip level whose texels are nearest a destination
    // pixel (Skia's `kLow`), then hands out premultiplied texels: borrowed
    // straight from the source when it is opaque, which is the common case.
    let sampling = match prepared {
        Some(p) => Sampling::prepared(p, s),
        None => Sampling::plain(&src.rgba, src.width, src.height, s),
    };
    let (lw, lh, texels) = sampling.texels();
    // Device pixels per sampled texel: source→device scale times the level's
    // power-of-two reduction.
    let step = s * (1u32 << sampling.level) as f32;
    let ox = dst.x - off_x;
    let oy = dst.y - off_y;
    let cols: Vec<Tap> = (0..w)
        .map(|i| Tap::new((left as f32 + i as f32 + 0.5 - ox) / step, lw))
        .collect();
    let rows: Vec<Tap> = (0..h)
        .map(|j| Tap::new((top as f32 + j as f32 + 0.5 - oy) / step, lh))
        .collect();

    // Rounded corners and squircles: coverage per sprite pixel.
    let coverage = coverage_mask(shape, dpr, w, h, left, top);
    let cov: Option<&[u8]> = coverage.as_deref();

    // Only the visible part of the sprite is sampled: outside the canvas or the
    // clip nothing is written, so the block is clamped before the loop and the
    // per-pixel taps are computed for the columns and rows that survive.
    let (x0, y0, x1, y1) = r.span(left, top, w as u32, h as u32);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    // Borrow the canvas and the clip together: no mask clone per draw.
    let Renderer { pm, clip, .. } = r;
    let (mask, m_left, m_top) = match clip {
        crate::paint::Clip::Mask {
            mask,
            left: mx,
            top: my,
            ..
        } => (Some(&**mask), *mx, *my),
        _ => (None, 0, 0),
    };
    let mw = mask.map_or(0, |m| m.width() as usize);
    let canvas_w = pm.width() as usize;
    let out = pm.data_mut();
    let stride = lw as usize * 4;
    for py in y0..y1 {
        let j = (py - top) as usize;
        let rt = rows[j];
        let row0 = rt.a as usize * stride;
        let row1 = rt.b as usize * stride;
        let wy = rt.w;
        let iwy = 256 - wy;
        let mut di = (py as usize * canvas_w + x0 as usize) * 4;
        for (si, px_x) in (j * w as usize + (x0 - left) as usize..).zip(x0..x1) {
            let cov_val = match cov {
                Some(c) => {
                    let v = c[si];
                    if v == 0 {
                        di += 4;
                        continue;
                    }
                    v as u32
                }
                None => 255,
            };
            let ct = cols[(px_x - left) as usize];
            let ax = ct.a as usize * 4;
            let bx = ct.b as usize * 4;
            let wx = ct.w;
            let iwx = 256 - wx;
            let (p0, p1) = (row0 + ax, row0 + bx);
            let (p2, p3) = (row1 + ax, row1 + bx);
            let mut px = [0u8; 4];
            if wx == 0 && wy == 0 {
                px.copy_from_slice(&texels[p0..p0 + 4]);
            } else if wy == 0 {
                for k in 0..4 {
                    px[k] = ((texels[p0 + k] as u32 * iwx + texels[p1 + k] as u32 * wx + 128) >> 8)
                        as u8;
                }
            } else if wx == 0 {
                for k in 0..4 {
                    px[k] = ((texels[p0 + k] as u32 * iwy + texels[p2 + k] as u32 * wy + 128) >> 8)
                        as u8;
                }
            } else {
                for k in 0..4 {
                    let top_v = texels[p0 + k] as u32 * iwx + texels[p1 + k] as u32 * wx;
                    let bot_v = texels[p2 + k] as u32 * iwx + texels[p3 + k] as u32 * wx;
                    px[k] = ((top_v * iwy + bot_v * wy + 32768) >> 16) as u8;
                }
            }
            if cov_val != 255 {
                for v in px.iter_mut() {
                    *v = ((*v as u32 * cov_val + 127) / 255) as u8;
                }
            }
            // The clip scales color and alpha together, exactly as the sprite
            // blit did, so a clipped draw is the same bytes as before.
            let mut scale = 255u32;
            if let Some(m) = mask {
                scale = m.data()[(py - m_top) as usize * mw + (px_x - m_left) as usize] as u32;
            }
            let sa = (px[3] as u32 * scale + 127) / 255;
            if sa == 255 {
                out[di..di + 4].copy_from_slice(&px);
            } else if sa > 0 {
                let inv = 255 - sa;
                for k in 0..3 {
                    let s = (px[k] as u32 * scale + 127) / 255;
                    let d = out[di + k] as u32;
                    out[di + k] = (s + (d * inv + 127) / 255).min(255) as u8;
                }
                out[di + 3] = (sa + (out[di + 3] as u32 * inv + 127) / 255).min(255) as u8;
            }
            di += 4;
        }
    }
}

/// The source image, or `None` if the caller has no bytes for it.
fn fetch<'a>(
    images: &'a dyn ImageSource,
    image: &ImageRef,
) -> Option<std::borrow::Cow<'a, DecodedImage>> {
    // Callers that keep decoded images around answer by reference, which
    // avoids copying a multi-megabyte buffer per draw.
    if let Some(img) = images.rgba_ref(image) {
        return Some(std::borrow::Cow::Borrowed(img));
    }
    images.rgba(image).map(std::borrow::Cow::Owned)
}

/// Coverage of the shape over the sprite's pixel grid, when the shape has
/// corners. `None` means every pixel is fully covered.
fn coverage_mask(shape: &Shape, dpr: f32, w: i32, h: i32, left: i32, top: i32) -> Option<Vec<u8>> {
    let rounded = shape.radii.tl > 0.0
        || shape.radii.tr > 0.0
        || shape.radii.br > 0.0
        || shape.radii.bl > 0.0
        || !matches!(shape.corner, crate::display::CornerShape::Round);
    if !rounded {
        return None;
    }
    let path = shapes::path(shape, dpr)?;
    crate::paint::coverage::mask_small(
        &path,
        w as u32,
        h as u32,
        FillRule::Winding,
        left as f32,
        top as f32,
    )
}

/// Premultiplied texels for one draw, at the mip level nearest the requested
/// scale.
///
/// The level is `floor(log2(1/scale))` when downscaling and 0 when upscaling.
/// Levels are built lazily and only as far as needed: for an opaque source
/// (every JPEG), level 0 is the source itself, so a large photo scaled down
/// pays for a halving chain and nothing else. Non-opaque images are
/// premultiplied first, so blending at the edges stays correct.
struct Sampling<'a> {
    texels: Texels<'a>,
    w: u32,
    h: u32,
    level: u32,
}

/// Where one level's texels live: borrowed from the source, built for this draw
/// alone, or shared from a [`PreparedImage`]'s cache.
enum Texels<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
    Shared(std::sync::Arc<Vec<u8>>),
}

impl Texels<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Texels::Borrowed(b) => b,
            Texels::Owned(v) => v,
            Texels::Shared(a) => a.as_slice(),
        }
    }
}

/// The mip level whose texels are nearest a device pixel.
fn level_for(scale: f32) -> u32 {
    if scale >= 1.0 || !scale.is_finite() {
        0
    } else {
        (1.0 / scale).log2().floor().max(0.0) as u32
    }
}

impl<'a> Sampling<'a> {
    /// Samples a bare decoded buffer: the path for an [`ImageSource`] that only
    /// answers [`rgba`](ImageSource::rgba), with nothing kept between draws.
    fn plain(rgba: &'a [u8], w: u32, h: u32, scale: f32) -> Sampling<'a> {
        let level = level_for(scale);
        let opaque = is_opaque(rgba);
        if level == 0 {
            let texels = if opaque {
                Texels::Borrowed(rgba)
            } else {
                Texels::Owned(premultiply(rgba))
            };
            return Sampling {
                texels,
                w,
                h,
                level: 0,
            };
        }
        // Level 1 comes straight from the source (with premultiplication
        // folded in), then each further level halves the previous one.
        let (mut cw, mut ch) = (w.max(1), h.max(1));
        let mut cur = Vec::new();
        let mut first = true;
        for _ in 1..=level {
            let (dw, dh) = ((cw / 2).max(1), (ch / 2).max(1));
            if dw == cw && dh == ch && !first {
                // Already 1 texel; further levels would be identical.
                break;
            }
            let src: &[u8] = if first { rgba } else { &cur };
            // Only level 0 → 1 converts straight alpha into premultiplied;
            // further halvings average already-premultiplied texels.
            cur = box_down(src, cw, ch, dw, dh, !opaque && first);
            cw = dw;
            ch = dh;
            first = false;
        }
        Sampling {
            texels: Texels::Owned(cur),
            w: cw,
            h: ch,
            level,
        }
    }

    /// Samples a prepared image: opaque level 0 borrows the decoded bytes, and
    /// every halved level is kept on the image, so a second draw at the same
    /// size pays for nothing.
    fn prepared(img: &'a PreparedImage, scale: f32) -> Sampling<'a> {
        let level = level_for(scale);
        let decoded = img.decoded();
        if level == 0 {
            let texels = if img.is_opaque() {
                Texels::Borrowed(&decoded.rgba)
            } else {
                Texels::Borrowed(img.premultiplied())
            };
            return Sampling {
                texels,
                w: decoded.width.max(1),
                h: decoded.height.max(1),
                level: 0,
            };
        }
        let (w, h, data) = mip(img, level);
        Sampling {
            texels: Texels::Shared(data),
            w,
            h,
            level,
        }
    }

    /// The texels to sample and their dimensions.
    fn texels(&self) -> (u32, u32, &[u8]) {
        (self.w, self.h, self.texels.bytes())
    }
}

/// The halved chain below a prepared image, at `level` or at the deepest level
/// it can build (a 1-texel level stops the chain).
fn mip(img: &PreparedImage, level: u32) -> (u32, u32, std::sync::Arc<Vec<u8>>) {
    let decoded = img.decoded();
    let mut cache = img.mips.lock().unwrap_or_else(|e| e.into_inner());
    // The deepest level already built, which is level 0 (the source) if none is.
    let mut base = 0u32;
    let mut cw = decoded.width.max(1);
    let mut ch = decoded.height.max(1);
    let mut cur: Option<std::sync::Arc<Vec<u8>>> = None;
    for &(l, w, h, ref data) in cache.iter() {
        if (base..=level).contains(&l) {
            base = l;
            cw = w;
            ch = h;
            cur = Some(data.clone());
        }
    }
    if base == level {
        if let Some(data) = cur {
            return (cw, ch, data);
        }
    }
    let opaque = img.is_opaque();
    let mut level_now = base;
    while level_now < level {
        let (dw, dh) = ((cw / 2).max(1), (ch / 2).max(1));
        if dw == cw && dh == ch && level_now > 0 {
            // Already 1 texel; further levels would be identical.
            break;
        }
        let src: &[u8] = match &cur {
            Some(a) => a.as_slice(),
            None => &decoded.rgba,
        };
        // Only level 0 → 1 converts straight alpha into premultiplied;
        // further halvings average already-premultiplied texels.
        let next = std::sync::Arc::new(box_down(src, cw, ch, dw, dh, !opaque && level_now == 0));
        level_now += 1;
        cw = dw;
        ch = dh;
        cache.push((level_now, cw, ch, next.clone()));
        cur = Some(next);
    }
    match cur {
        Some(data) => (cw, ch, data),
        // Unreachable: `level >= 1` builds at least one level above. Kept total
        // rather than fatal.
        None => (
            cw,
            ch,
            std::sync::Arc::new(if opaque {
                decoded.rgba.clone()
            } else {
                premultiply(&decoded.rgba)
            }),
        ),
    }
}

/// True when every pixel is fully opaque, which lets the sampler skip
/// premultiplication entirely.
pub(super) fn is_opaque(rgba: &[u8]) -> bool {
    // Eight bytes at a time: alpha sits at offsets 3 and 7, so this reads the
    // buffer contiguously instead of striding through it.
    let chunks = rgba.as_chunks::<8>().0;
    if !chunks.iter().all(|c| c[3] == 255 && c[7] == 255) {
        return false;
    }
    let tail = (chunks.len() * 8 + 3)..rgba.len();
    tail.step_by(4).all(|i| rgba[i] == 255)
}

pub(super) fn premultiply(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.as_chunks::<4>().0 {
        let a = px[3] as u32;
        out.extend_from_slice(&[
            ((px[0] as u32 * a + 127) / 255) as u8,
            ((px[1] as u32 * a + 127) / 255) as u8,
            ((px[2] as u32 * a + 127) / 255) as u8,
            px[3],
        ]);
    }
    out
}

/// Box-filters `src` down to `dw × dh` in integer arithmetic.
///
/// With `premultiply`, the source is straight alpha and each source sample is
/// weighted by its alpha (the average of premultiplied values), which is the
/// correct way to average colours across a transparent edge.
fn box_down(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32, premultiply: bool) -> Vec<u8> {
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    // Source spans per output column and row: contiguous, never empty.
    let span = |i: u32, src_len: u32, dst_len: u32| -> (u32, u32) {
        let a = (i as u64 * src_len as u64 / dst_len as u64) as u32;
        let b = (((i + 1) as u64 * src_len as u64).div_ceil(dst_len as u64)) as u32;
        (a, b.max(a + 1).min(src_len))
    };
    let xs: Vec<(u32, u32)> = (0..dw).map(|i| span(i, sw, dw)).collect();
    let ys: Vec<(u32, u32)> = (0..dh).map(|i| span(i, sh, dh)).collect();
    for (dy, &(y0, y1)) in ys.iter().enumerate() {
        for (dx, &(x0, x1)) in xs.iter().enumerate() {
            let mut acc = [0u32; 4];
            for y in y0..y1 {
                let mut o = (y as usize * sw as usize + x0 as usize) * 4;
                for _ in x0..x1 {
                    if premultiply {
                        let a = src[o + 3] as u32;
                        for k in 0..3 {
                            acc[k] += src[o + k] as u32 * a;
                        }
                        acc[3] += a;
                    } else {
                        for k in 0..4 {
                            acc[k] += src[o + k] as u32;
                        }
                    }
                    o += 4;
                }
            }
            let n = (x1 - x0) * (y1 - y0);
            let d = (dy * dw as usize + dx) * 4;
            if premultiply {
                let total = (n * 255).max(1);
                for k in 0..3 {
                    out[d + k] = ((acc[k] + total / 2) / total) as u8;
                }
                out[d + 3] = ((acc[3] + n / 2) / n) as u8;
            } else {
                // Halving makes `n` 4 for almost every texel (1, 2, 3, 6 or 9
                // at the odd edges), and a constant divisor compiles to a shift
                // or a reciprocal multiply instead of `udiv`. The values are the
                // `(acc + n / 2) / n` ones exactly.
                match n {
                    1 => {
                        for k in 0..4 {
                            out[d + k] = acc[k] as u8;
                        }
                    }
                    2 => {
                        for k in 0..4 {
                            out[d + k] = ((acc[k] + 1) >> 1) as u8;
                        }
                    }
                    3 => {
                        for k in 0..4 {
                            out[d + k] = ((acc[k] + 1) / 3) as u8;
                        }
                    }
                    4 => {
                        for k in 0..4 {
                            out[d + k] = ((acc[k] + 2) >> 2) as u8;
                        }
                    }
                    6 => {
                        for k in 0..4 {
                            out[d + k] = ((acc[k] + 3) / 6) as u8;
                        }
                    }
                    9 => {
                        for k in 0..4 {
                            out[d + k] = ((acc[k] + 4) / 9) as u8;
                        }
                    }
                    _ => {
                        for k in 0..4 {
                            out[d + k] = ((acc[k] + n / 2) / n) as u8;
                        }
                    }
                }
            }
        }
    }
    out
}

// ------------------------------------------------------- image-service copy

/// The fit boxes X's image service offers for a card picture, by the name the
/// client asks for: `small`, `900x900`, `medium`, `large`.
const SERVICE_BOXES: [u32; 4] = [680, 900, 1200, 2048];

/// The side of the box X's client asks the image service for, given the slot
/// the picture fills (`slot` css px) and the device pixel ratio: the smallest
/// box wider than the slot's device pixels, the rule `docs/MEDIA.md` gives for
/// photos. `None` past the largest box: the client takes the original.
///
/// Read off a signed-in session at dpr 2 on the detail page of post
/// 2122162840463376558 (rauchg, 2026-11-16), card box in css px → variant:
/// 284, 304, 324, 336 → `small` (568–672 device px); 340, 354, 365, 396, 408,
/// 416 → `900x900` (680–832); 476, 564 → `medium` (952–1128). The viewport's
/// `sm:` breakpoint at 512 changes nothing; a 680 px box asks for `900x900`,
/// so a box is taken only when it is strictly wider than the slot. No card
/// box above 1,128 device px was observed: the `large` step (2048) and the
/// original past it are the photo ladder's (`docs/MEDIA.md`), assumed to hold
/// for cards.
pub fn service_box(slot: f32, dpr: f32) -> Option<u32> {
    if !slot.is_finite() || !dpr.is_finite() || slot <= 0.0 || dpr <= 0.0 {
        return None;
    }
    let device = slot * dpr;
    SERVICE_BOXES
        .iter()
        .copied()
        .find(|&side| side as f32 > device)
}

/// The picture as X's image service serves it: fitted inside a `side` × `side`
/// box (`name=small` is 680, `name=medium` 1200; a 1000 × 3000 portrait comes
/// out 227 × 680, the shape `docs/MEDIA.md` shows for a photo at `small`),
/// resampled with Lanczos-3 in sRGB. A picture that already fits comes back as
/// it is. Against the viewer's 2× capture of post 2121745693157064899 (mobile
/// web, card picture 2400 px wide), sampling the original straight down left
/// 2,599 mismatched pixels in the card; through the 680 copy Lanczos-3 left 68,
/// bicubic 71, bilinear 118, box 274. The JPEG re-encode X does on top moved
/// 3–4 pixels between quality 85 and 95 and is not reproduced.
pub fn service_copy(src: &DecodedImage, side: u32) -> DecodedImage {
    let (sw, sh) = (src.width, src.height);
    let side = side.max(1);
    if sw <= side && sh <= side {
        return DecodedImage {
            width: sw,
            height: sh,
            rgba: src.rgba.clone(),
        };
    }
    let s = (side as f64 / sw as f64).min(side as f64 / sh as f64);
    let dw = ((sw as f64 * s).round() as u32).max(1);
    let dh = ((sh as f64 * s).round() as u32).max(1);
    // Filtered in f64 from the source bytes to the rounded result, without a
    // copy of the picture in between: the horizontal pass reads the u8 texels
    // and premultiplies each tap (a transparent edge filtered straight would
    // bleed the colour under alpha 0, usually black, into it as a dark
    // fringe), the vertical pass accumulates the resampled source rows one at
    // a time, and straight alpha is divided back and rounded once, so a
    // translucent pixel comes back as it went in. Memory is the output plus a
    // window of resampled rows, not eight times the source.
    let opaque = is_opaque(&src.rgba);
    let htaps = lanczos_taps(sw, dw);
    let vtaps = lanczos_taps(sh, dh);
    let stride = dw as usize * 4;
    let resample_row = |r: usize, row: &mut Vec<f64>| {
        row.clear();
        for (lo, ws) in &htaps {
            let mut acc = [0f64; 4];
            for (k, wgt) in ws.iter().enumerate() {
                let s = (r * sw as usize + lo + k) * 4;
                let a = src.rgba[s + 3] as f64;
                let k = if opaque { 1.0 } else { a / 255.0 };
                acc[0] += src.rgba[s] as f64 * k * wgt;
                acc[1] += src.rgba[s + 1] as f64 * k * wgt;
                acc[2] += src.rgba[s + 2] as f64 * k * wgt;
                acc[3] += a * wgt;
            }
            row.extend_from_slice(&acc);
        }
    };
    // The window of resampled source rows a destination row needs; windows
    // only move down, so a row is resampled once and dropped once passed.
    let mut window: std::collections::VecDeque<(usize, Vec<f64>)> =
        std::collections::VecDeque::new();
    let mut out = Vec::with_capacity(stride * dh as usize);
    let mut acc = vec![0f64; stride];
    for (lo, ws) in &vtaps {
        while window.front().is_some_and(|(r, _)| *r < *lo) {
            window.pop_front();
        }
        for r in *lo..*lo + ws.len() {
            if window.back().is_none_or(|(last, _)| *last < r) {
                let mut row = Vec::with_capacity(stride);
                resample_row(r, &mut row);
                window.push_back((r, row));
            }
        }
        acc.iter_mut().for_each(|v| *v = 0.0);
        for (k, wgt) in ws.iter().enumerate() {
            let row = &window
                .iter()
                .find(|(r, _)| *r == lo + k)
                .expect("row in window")
                .1;
            for (v, x) in acc.iter_mut().zip(row) {
                *v += x * wgt;
            }
        }
        for px in acc.as_chunks::<4>().0 {
            let a = px[3].round().clamp(0.0, 255.0);
            if !opaque && a <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let k = if opaque { 1.0 } else { 255.0 / px[3] };
            for c in &px[..3] {
                out.push((c * k).round().clamp(0.0, 255.0) as u8);
            }
            out.push(a as u8);
        }
    }
    DecodedImage {
        width: dw,
        height: dh,
        rgba: out,
    }
}

/// `sin` without libm. `f64::sin` is the platform's, and its last bits differ
/// between macOS and wasm32 (12,548 of 200,000 kernel weights, by up to 4 ulp),
/// which the determinism gate sees as a byte flip once a texel is rounded. The
/// argument is folded into [-π/2, π/2] and summed as the Taylor series to x¹⁹
/// (within 1e-15 there) with IEEE operations only, the coefficients constants;
/// the taps are pinned by hash natively, and `determinism.mjs` is what proves
/// the wasm build matches.
fn sin_det(x: f64) -> f64 {
    use std::f64::consts::PI;
    let mut x = x - (x / (2.0 * PI)).round() * (2.0 * PI);
    if x > PI / 2.0 {
        x = PI - x;
    } else if x < -PI / 2.0 {
        x = -PI - x;
    }
    const C: [f64; 10] = [
        1.0,
        -1.0 / 6.0,
        1.0 / 120.0,
        -1.0 / 5040.0,
        1.0 / 362_880.0,
        -1.0 / 39_916_800.0,
        1.0 / 6_227_020_800.0,
        -1.0 / 1_307_674_368_000.0,
        1.0 / 355_687_428_096_000.0,
        -1.0 / 121_645_100_408_832_000.0,
    ];
    let x2 = x * x;
    let mut acc = C[9];
    for c in C[..9].iter().rev() {
        acc = acc * x2 + c;
    }
    x * acc
}

/// Lanczos-3 taps for one axis: per destination index, the first source index
/// of its window and the normalised weights, the support widened by the
/// downscale factor as PIL and ImageMagick do.
fn lanczos_taps(len: u32, dst: u32) -> Vec<(usize, Vec<f64>)> {
    const A: f64 = 3.0;
    let lanczos = |x: f64| -> f64 {
        if x == 0.0 {
            1.0
        } else if x.abs() >= A {
            0.0
        } else {
            let px = std::f64::consts::PI * x;
            A * sin_det(px) * sin_det(px / A) / (px * px)
        }
    };
    let scale = len as f64 / dst as f64;
    let support = A * scale.max(1.0);
    (0..dst)
        .map(|i| {
            let center = (i as f64 + 0.5) * scale;
            let lo = (center - support).floor().max(0.0) as usize;
            let hi = ((center + support).ceil() as usize).min(len as usize);
            let mut ws: Vec<f64> = (lo..hi)
                .map(|j| lanczos((j as f64 + 0.5 - center) / scale.max(1.0)))
                .collect();
            let sum: f64 = ws.iter().sum();
            if sum != 0.0 {
                for v in &mut ws {
                    *v /= sum;
                }
            }
            (lo, ws)
        })
        .collect()
}

// --------------------------------------------------------------- placeholder

const PLACEHOLDER_FILL: Rgba = Rgba::hex(0xCFD9DE);
const PLACEHOLDER_LABEL: Rgba = Rgba::hex(0x536471);

/// A neutral card where an image would be, with its label if it has one.
pub(crate) fn placeholder(r: &mut Renderer, shape: &Shape, label: &str) {
    if let Some(path) = shapes::path(shape, r.dpr) {
        r.fill(&path, PLACEHOLDER_FILL, FillRule::Winding);
    }
    let label = label.trim();
    if label.is_empty() || shape.rect.w < 24.0 || shape.rect.h < 18.0 {
        return;
    }
    let size = 13.0;
    let line_height = 16.0;
    let st = inline::BlockStyle {
        size,
        line_height,
        weight: 400,
        width: (shape.rect.w - 16.0).max(1.0),
        max_lines: Some(1),
        wrap: inline::Wrap::Anywhere,
        preserve: false,
        dir: inline::Dir::Auto,
        tab_size: 4.0,
    };
    let block = inline::layout(&[inline::Span::new(label, PLACEHOLDER_LABEL, 400)], &st);
    let x = shape.rect.x + (shape.rect.w - block.width) / 2.0;
    let y = shape.rect.y + (shape.rect.h - line_height) / 2.0;
    for line in &block.lines {
        let baseline = y + line.top + line.baseline;
        for run in &line.runs {
            let glyphs: Vec<crate::display::GlyphPos> = run
                .glyphs
                .iter()
                .map(|g| crate::display::GlyphPos {
                    id: g.id,
                    x: g.x,
                    y: g.y,
                })
                .collect();
            r.glyphs(
                x + run.x,
                baseline,
                run.face,
                size,
                PLACEHOLDER_LABEL,
                &glyphs,
            );
        }
    }
}

#[cfg(test)]
mod service_tests {
    use super::*;

    /// The service's box is square: 2400 × 1256 fits 680 × 356 at `small` and 1200 × 628 at `medium`
    /// (the sizes X served for posts 2121745693157064899 and 2122162840463376558); a 1000 × 3000
    /// portrait fits 227 × 680, the shape `docs/MEDIA.md` shows for a photo at `small`.
    #[test]
    fn the_copy_fits_the_services_box() {
        let wide = DecodedImage {
            width: 2400,
            height: 1256,
            rgba: vec![200; 2400 * 1256 * 4],
        };
        let small = service_copy(&wide, 680);
        assert_eq!((small.width, small.height), (680, 356));
        let medium = service_copy(&wide, 1200);
        assert_eq!((medium.width, medium.height), (1200, 628));
        assert!(
            small.rgba.iter().all(|&v| v == 200),
            "a flat picture stays flat"
        );
        let tall = DecodedImage {
            width: 1000,
            height: 3000,
            rgba: vec![90; 1000 * 3000 * 4],
        };
        let t = service_copy(&tall, 680);
        assert_eq!((t.width, t.height), (227, 680));
        let fits = DecodedImage {
            width: 600,
            height: 314,
            rgba: vec![7; 600 * 314 * 4],
        };
        let f = service_copy(&fits, 680);
        assert_eq!((f.width, f.height, f.rgba), (600, 314, fits.rgba));
    }

    /// A transparent margin (alpha 0, black underneath) must not darken the picture's edge: the filter runs on
    /// premultiplied texels, so a half-covered edge pixel is white at half alpha, not grey.
    #[test]
    fn transparent_edges_do_not_fringe() {
        let (w, h) = (1360u32, 8u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 680..w {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let src = DecodedImage {
            width: w,
            height: h,
            rgba,
        };
        let d = service_copy(&src, 680);
        assert_eq!((d.width, d.height), (680, 4));
        for x in 0..680usize {
            let px = &d.rgba[x * 4..x * 4 + 4];
            if px[3] > 0 {
                assert!(
                    px[0] >= 250 && px[1] >= 250 && px[2] >= 250,
                    "pixel {x} with alpha {} is {:?}, not white",
                    px[3],
                    &px[..3]
                );
            }
        }
    }

    /// A translucent picture comes back as it went in: (200, 200, 200, 128) is premultiplied, filtered and
    /// divided back in f64, rounded once, so no unit is lost on the way; opaque pixels are exact too.
    #[test]
    fn translucent_pixels_round_trip() {
        let (w, h) = (1360u32, 8u32);
        let src = DecodedImage {
            width: w,
            height: h,
            rgba: (0..w * h).flat_map(|_| [200, 200, 200, 128]).collect(),
        };
        let d = service_copy(&src, 680);
        assert_eq!((d.width, d.height), (680, 4));
        assert!(
            d.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [200, 200, 200, 128]),
            "a flat translucent picture changed"
        );
        let opaque = DecodedImage {
            width: w,
            height: h,
            rgba: (0..w * h).flat_map(|_| [17, 250, 3, 255]).collect(),
        };
        let d = service_copy(&opaque, 680);
        assert!(d
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| *px == [17, 250, 3, 255]));
    }

    /// Lanczos keeps a hard edge where a box filter smears it over the whole window.
    #[test]
    fn lanczos_keeps_edges_sharp() {
        let w = 64u32;
        let mut rgba: Vec<u8> = (0..(w * 4) as usize).flat_map(|_| [0, 0, 0, 255]).collect();
        for y in 0..4 {
            for x in 32..64 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let src = DecodedImage {
            width: w,
            height: 4,
            rgba,
        };
        let d = service_copy(&src, 16);
        assert_eq!((d.width, d.height), (16, 1));
        let row: Vec<u8> = (0..16).map(|x| d.rgba[x * 4]).collect();
        assert!(row[..6].iter().all(|&v| v < 24), "left stays dark: {row:?}");
        assert!(
            row[10..].iter().all(|&v| v > 231),
            "right stays light: {row:?}"
        );
    }

    /// `sin_det` is `sin` to 1e-14 across the range the kernel asks for, and the kernel it builds is the
    /// same bits on every target: the hash below was taken on macOS arm64 and holds on wasm32 and Linux
    /// because nothing in it reaches libm.
    #[test]
    fn the_kernel_is_the_same_bits_everywhere() {
        for i in -400..=400 {
            let x = i as f64 * 0.025;
            assert!((sin_det(x) - x.sin()).abs() < 1e-14, "sin_det({x})");
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for (len, dst) in [
            (2400u32, 1200u32),
            (1256, 628),
            (2400, 680),
            (1256, 356),
            (3000, 680),
            (1000, 227),
            (64, 16),
        ] {
            for (lo, ws) in lanczos_taps(len, dst) {
                for b in (lo as u64).to_le_bytes() {
                    hash = (hash ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
                }
                for w in ws {
                    for b in w.to_bits().to_le_bytes() {
                        hash = (hash ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
                    }
                }
            }
        }
        assert_eq!(hash, 0x62aa_b0f0_3ee8_0322, "the Lanczos taps changed bits");
    }

    /// A prepared image keeps the copy, prepared in turn (opacity and mips once): the second ask is the
    /// same allocation.
    #[test]
    fn a_prepared_image_keeps_its_copy() {
        let img = PreparedImage::new(DecodedImage {
            width: 1360,
            height: 8,
            rgba: (0..1360 * 8).flat_map(|_| [90, 90, 90, 255]).collect(),
        });
        let a = img.service_copy(680);
        let b = img.service_copy(680);
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        assert_eq!((a.decoded().width, a.decoded().height), (680, 4));
        assert!(a.is_opaque());
        assert!(img.bytes() >= 1360 * 8 * 4 + 680 * 4 * 4);
    }

    /// The box follows the slot's device pixels, not the viewport: every row of
    /// the signed-in dpr 2 session on post 2122162840463376558's detail page,
    /// and the dpr 1 and 3 cases the same rule gives.
    #[test]
    fn the_service_box_is_the_smallest_wider_than_the_slots_device_pixels() {
        // Observed at dpr 2: card box in css px → variant.
        for (slot, side) in [
            (284.0, 680),
            (304.0, 680),
            (324.0, 680),
            (336.0, 680),
            (340.0, 900),
            (354.0, 900),
            (365.0, 900),
            (396.0, 900),
            (408.0, 900),
            (416.0, 900),
            (476.0, 1200),
            (564.0, 1200),
        ] {
            assert_eq!(service_box(slot, 2.0), Some(side), "{slot} css at dpr 2");
        }
        // 680 device pixels ask for `900x900`: the box has to be strictly wider.
        assert_eq!(service_box(680.0, 1.0), Some(900));
        assert_eq!(service_box(679.0, 1.0), Some(680));
        // A phone: 354 css on the detail page at dpr 3 is 1,062 device pixels,
        // `medium`; a 1200-wide original then fits and is drawn as given.
        assert_eq!(service_box(354.0, 3.0), Some(1200));
        assert_eq!(service_box(322.0, 3.0), Some(1200));
        // Past `large` the client takes the original.
        assert_eq!(service_box(1100.0, 2.0), None);
        assert_eq!(service_box(0.0, 2.0), None);
        assert_eq!(service_box(300.0, f32::NAN), None);
    }
}
