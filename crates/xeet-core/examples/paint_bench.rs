//! Benchmarks the painter against PLAN §12's budget.
//!
//! ```sh
//! cargo run --profile fast --example paint_bench
//! cargo run --release --example paint_bench -- --iters 200
//! ```
//!
//! Reports, in the order of the plan:
//!
//! - a text post, 390 CSS px wide at 3×, raster + PNG encode;
//! - the same with four placeholder media cards;
//! - the same with four decoded images (240×160 each) scaled down;
//! - the PNG encoder alone, and raw RGBA without encoding.
//!
//! Timing is a median over `--iters` runs after a warm-up, because the first
//! run pays for font parsing and glyph cache misses.

use std::time::{Duration, Instant};
use xeet_core::color::Rgba;
use xeet_core::display::{DisplayList, ImageRef, Item, Radii, Rect, Shape};
use xeet_core::inline::{self, BlockStyle, Dir, Span, Wrap};
use xeet_core::paint::{self, DecodedImage, ImageSource, NoImages};

const BODY: &str = "We just shipped the thing. A faster path for every request, \
a smaller cold start, and a much better story for previews. Read more: \
vercel.com/blog/the-thing-we-shipped-and-why";

struct Imgs(Vec<(ImageRef, DecodedImage)>);

impl ImageSource for Imgs {
    fn rgba(&self, image: &ImageRef) -> Option<DecodedImage> {
        self.0
            .iter()
            .find(|(r, _)| r == image)
            .map(|(_, d)| d.clone())
    }
}

/// Encoded bytes: decodes on every call, which is what a caller that does not
/// cache does (measured here so the decode cost is visible separately).
struct Encoded(Vec<(ImageRef, Vec<u8>)>);

impl ImageSource for Encoded {
    fn rgba(&self, image: &ImageRef) -> Option<DecodedImage> {
        let bytes = self.0.iter().find(|(r, _)| r == image).map(|(_, b)| b)?;
        paint::decode_image(bytes, &paint::Limits::default()).ok()
    }
}

fn test_image(w: u32, h: u32) -> DecodedImage {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                ((x * 255 / w.max(1)) as u8),
                ((y * 255 / h.max(1)) as u8),
                (((x + y) * 127 / (w + h).max(1)) as u8),
                255,
            ]);
        }
    }
    DecodedImage {
        width: w,
        height: h,
        rgba,
    }
}

fn text_block(width: f32, size: f32, lh: f32, text: &str, color: Rgba) -> inline::TextBlock {
    let st = BlockStyle {
        size,
        line_height: lh,
        weight: 400,
        width,
        max_lines: None,
        wrap: Wrap::Anywhere,
        preserve: true,
        dir: Dir::Auto,
        tab_size: 4.0,
    };
    inline::layout(&[Span::new(text, color, 400)], &st)
}

fn text_frame(width: f32) -> DisplayList {
    let fg = Rgba::hex(0x0F1419);
    let block = text_block(width - 32.0, 15.0, 20.0, BODY, fg);
    let mut list = DisplayList::new(width, block.height + 32.0, Rgba::WHITE);
    list.push_text(&block, 16.0, 16.0);
    list
}

/// The same post with `n` square media cells, optionally without rounded
/// corners (to separate the cost of the corner mask from scaling).
fn media_frame_opts(width: f32, images: bool, radius: f32) -> DisplayList {
    let mut list = text_frame(width);
    let cell = (width - 32.0 - 4.0) / 2.0;
    list.height += 2.0 * cell + 4.0 + 12.0;
    let top = list.height - 2.0 * cell - 4.0 - 4.0;
    for i in 0..4 {
        let x = 16.0 + (i % 2) as f32 * (cell + 4.0);
        let y = top + (i / 2) as f32 * (cell + 4.0);
        let shape = Shape {
            rect: Rect::new(x, y, cell, cell),
            radii: Radii::all(radius),
            corner: xeet_core::display::CornerShape::Round,
        };
        if images {
            list.push(Item::Image {
                shape,
                image: ImageRef::Media(i),
                focus: (0.5, 0.5),
                service: false,
            });
        } else {
            list.push(Item::Placeholder {
                shape,
                label: format!("Photo {}", i + 1),
            });
        }
    }
    list
}

fn media_frame(width: f32, images: bool) -> DisplayList {
    let mut list = text_frame(width);
    let cell = (width - 32.0 - 4.0) / 2.0;
    list.height += 2.0 * cell + 4.0 + 12.0;
    let top = list.height - 2.0 * cell - 4.0 - 4.0;
    for i in 0..4 {
        let x = 16.0 + (i % 2) as f32 * (cell + 4.0);
        let y = top + (i / 2) as f32 * (cell + 4.0);
        let shape = Shape {
            rect: Rect::new(x, y, cell, cell),
            radii: Radii::all(16.0),
            corner: xeet_core::display::CornerShape::Round,
        };
        if images {
            list.push(Item::Image {
                shape,
                image: ImageRef::Media(i),
                focus: (0.5, 0.5),
                service: false,
            });
        } else {
            list.push(Item::Placeholder {
                shape,
                label: format!("Photo {}", i + 1),
            });
        }
    }
    list
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn bench(name: &str, iters: usize, mut f: impl FnMut() -> usize) {
    for _ in 0..3 {
        f();
    }
    let mut times = Vec::with_capacity(iters);
    let mut bytes = 0;
    for _ in 0..iters {
        let t = Instant::now();
        bytes = f();
        times.push(t.elapsed());
    }
    let m = median(times.clone());
    let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
    let best = *times.iter().min().unwrap();
    println!(
        "{:<34} median {:>7.3} ms   best {:>7.3} ms   p95 {:>7.3} ms   {} bytes",
        name,
        m.as_secs_f64() * 1e3,
        best.as_secs_f64() * 1e3,
        p95.as_secs_f64() * 1e3,
        bytes
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str, d: usize| -> usize {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let iters = flag("--iters", 50);
    let dpr = 3.0;

    let text = text_frame(390.0);
    let placeholders = media_frame(390.0, false);
    let media = media_frame(390.0, true);
    let images = Imgs(vec![
        (ImageRef::Media(0), test_image(240, 160)),
        (ImageRef::Media(1), test_image(240, 160)),
        (ImageRef::Media(2), test_image(240, 160)),
        (ImageRef::Media(3), test_image(240, 160)),
    ]);

    println!("dpr {dpr}, {iters} iterations, median of each\n");
    bench("text: raster + png", iters, || {
        let p = paint::rasterize(&text, dpr, &NoImages);
        paint::encode_png(&p).len()
    });
    bench("text: raster only", iters, || {
        paint::rasterize(&text, dpr, &NoImages).rgba.len()
    });
    let pm = paint::rasterize(&text, dpr, &NoImages);
    bench("text: png only", iters, || paint::encode_png(&pm).len());
    bench("text: png only (best)", iters, || {
        paint::encode_png_best(&pm).len()
    });
    bench("text: svg", iters, || paint::to_svg(&text).len());
    bench("4 placeholders: raster + png", iters, || {
        let p = paint::rasterize(&placeholders, dpr, &NoImages);
        paint::encode_png(&p).len()
    });
    bench("4 images 240x160: raster + png", iters, || {
        let p = paint::rasterize(&media, dpr, &images);
        paint::encode_png(&p).len()
    });
    bench("4 images 240x160: raster only", iters, || {
        paint::rasterize(&media, dpr, &images).rgba.len()
    });

    // The case PLAN §12 calls "four real images decoded and scaled": four
    // 1600x900 JPEGs. `--jpeg <file>` supplies one; it is used for all four so
    // the run needs no fixture directory.
    if let Some(path) = args
        .iter()
        .position(|a| a == "--jpeg")
        .and_then(|i| args.get(i + 1))
    {
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("cannot read {path}");
            return;
        };
        let Ok(decoded) = paint::decode_image(&bytes, &paint::Limits::default()) else {
            eprintln!("cannot decode {path}");
            return;
        };
        println!(
            "\nphotos: {path} ({} bytes) decodes to {}x{}",
            bytes.len(),
            decoded.width,
            decoded.height
        );
        let encoded = Encoded(vec![
            (ImageRef::Media(0), bytes.clone()),
            (ImageRef::Media(1), bytes.clone()),
            (ImageRef::Media(2), bytes.clone()),
            (ImageRef::Media(3), bytes.clone()),
        ]);
        let photos = Imgs(vec![
            (ImageRef::Media(0), decoded.clone()),
            (ImageRef::Media(1), decoded.clone()),
            (ImageRef::Media(2), decoded.clone()),
            (ImageRef::Media(3), decoded.clone()),
        ]);
        // Pre-scaled to the device cell size, so the mip pyramid costs nothing.
        let cell_px = ((390.0 - 36.0) / 2.0 * dpr).round() as u32;
        let small = downscale(&decoded, cell_px, cell_px);
        let prescaled = Imgs(vec![
            (ImageRef::Media(0), small.clone()),
            (ImageRef::Media(1), small.clone()),
            (ImageRef::Media(2), small.clone()),
            (ImageRef::Media(3), small.clone()),
        ]);
        let no_radius = media_frame_opts(390.0, true, 0.0);

        bench("4 photos: decode only", iters, || {
            let mut n = 0;
            for _ in 0..4 {
                n += paint::decode_image(&bytes, &paint::Limits::default())
                    .map(|d| d.rgba.len())
                    .unwrap_or(0);
            }
            n
        });
        bench("4 photos: decode + raster + png", iters / 4 + 1, || {
            let p = paint::rasterize(&media, dpr, &encoded);
            paint::encode_png(&p).len()
        });
        bench("4 photos: raster + png", iters, || {
            let p = paint::rasterize(&media, dpr, &photos);
            paint::encode_png(&p).len()
        });
        bench("4 photos: raster only", iters, || {
            paint::rasterize(&media, dpr, &photos).rgba.len()
        });
        bench("4 photos pre-scaled: raster only", iters, || {
            paint::rasterize(&media, dpr, &prescaled).rgba.len()
        });
        bench("4 photos, square cells: raster only", iters, || {
            paint::rasterize(&no_radius, dpr, &photos).rgba.len()
        });
        bench("4 photos: png only", iters, || {
            let p = paint::rasterize(&media, dpr, &photos);
            paint::encode_png(&p).len()
        });
    }
}

/// Nearest-neighbour shrink, only for building the "pre-scaled" comparison.
fn downscale(img: &DecodedImage, w: u32, h: u32) -> DecodedImage {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let sx = (x as u64 * img.width as u64 / w as u64) as u32;
            let sy = (y as u64 * img.height as u64 / h as u64) as u32;
            let o = ((sy as usize * img.width as usize) + sx as usize) * 4;
            rgba.extend_from_slice(&img.rgba[o..o + 4]);
        }
    }
    DecodedImage {
        width: w,
        height: h,
        rgba,
    }
}
