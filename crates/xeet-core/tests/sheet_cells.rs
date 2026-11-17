//! A sheet's cell is the cell `render` draws alone. The card picture's
//! image-service copy is chosen from the picture's own slot and the dpr
//! (`paint::service_box`), never from the sheet's canvas, so a 390 px cell
//! takes `name=small` whether it sits alone or beside another column.

use xeet_core::limits;
use xeet_core::model::{Post, Theme};
use xeet_core::paint::{decode_image, encode_png, Limits as ImageLimits, Pixmap};
use xeet_core::render::sheet;

/// Standard base64, for the test's own PNG; the crate decodes, it does not encode.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, &b| (n << 8) | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A post with one large card whose picture is wider than any service copy:
/// a ramp with a fine checker, the pattern that tells one downscale from
/// another.
fn post_with_card_picture(w: u32, h: u32) -> Post {
    post_with_picture(&ramp(w, h))
}

/// A ramp with a fine checker, the pattern that tells one downscale from another.
fn ramp(w: u32, h: u32) -> Pixmap {
    let mut pm = Pixmap::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let fine = ((x >> 1) + (y >> 1)) % 2 == 0;
            pm.rgba[i] = (x * 255 / w) as u8;
            pm.rgba[i + 1] = if fine { 40 } else { 220 };
            pm.rgba[i + 2] = (y * 255 / h) as u8;
            pm.rgba[i + 3] = 255;
        }
    }
    pm
}

/// The same post with the given picture.
fn post_with_picture(pm: &Pixmap) -> Post {
    let (w, h) = (pm.width, pm.height);
    let png = encode_png(pm);
    let json = format!(
        r#"{{"text":"A card picture through the service: https://vercel.com/blog/the-thing",
            "author":{{"name":"Vercel","handle":"vercel","verified":"business"}},
            "links":{{"https://vercel.com/blog/the-thing":{{"card":"summary_large_image",
            "title":"The thing, shipped","domain":"vercel.com",
            "image":{{"width":{w},"height":{h},"data":"{}"}}}}}}}}"#,
        base64(&png)
    );
    limits::parse_post(&json).unwrap_or_else(|e| panic!("{e}"))
}

/// Pixels of a rectangle, row by row.
fn region(
    img: &xeet_core::paint::DecodedImage,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) -> Vec<&[u8]> {
    (y..y + h)
        .map(|row| {
            &img.rgba[(row * img.width as usize + x) * 4..(row * img.width as usize + x + w) * 4]
        })
        .collect()
}

#[test]
fn a_cell_on_a_sheet_is_the_cell_alone() {
    let post = post_with_card_picture(1400, 733);
    let limits = ImageLimits::default();
    let one = decode_image(
        &sheet(&post, &[390.0], &[Theme::Light], 1.0).unwrap(),
        &limits,
    )
    .unwrap();
    let two = decode_image(
        &sheet(&post, &[390.0, 390.0], &[Theme::Light], 1.0).unwrap(),
        &limits,
    )
    .unwrap();
    // `sheet`: MARGIN 16 around, LABEL_H 18 above each row, so the first cell
    // starts at (16, 34) and the row is the canvas less the margins and label.
    let h = one.height as usize - 16 - 18 - 16;
    assert_eq!(one.height, two.height);
    assert_eq!(
        region(&one, 16, 34, 390, h),
        region(&two, 16, 34, 390, h),
        "the 390 cell alone and beside another column"
    );
    // The same for a 600 cell (at dpr 1 its 518 css px slot still takes `name=small`), beside the 390 one.
    let six = decode_image(
        &sheet(&post, &[600.0], &[Theme::Light], 1.0).unwrap(),
        &limits,
    )
    .unwrap();
    let both = decode_image(
        &sheet(&post, &[390.0, 600.0], &[Theme::Light], 1.0).unwrap(),
        &limits,
    )
    .unwrap();
    let h6 = six.height as usize - 16 - 18 - 16;
    assert_eq!(six.height, both.height);
    assert_eq!(
        region(&six, 16, 34, 600, h6),
        region(&both, 16 + 390 + 16, 34, 600, h6),
        "the 600 cell alone and beside a 390 column"
    );
}

/// The box follows the slot's device pixels, not the viewport. In a 390 px
/// timeline cell the card picture's slot is under 680 device pixels at dpr 2,
/// so the client's copy is `small`: the 1400 px original and its own 680 copy
/// render the same cell. At dpr 3 the slot is over 900 device pixels, so the
/// copy is `medium` and the 680 copy is not what is drawn; a 1200 px original
/// fits `medium` and is drawn as given, as `main` draws it.
#[test]
fn the_copy_follows_the_device_pixels_not_the_viewport() {
    use xeet_core::model::{RenderOptions, Surface};
    use xeet_core::paint::{service_box, service_copy, DecodedImage};
    use xeet_core::render::render;

    let limits = ImageLimits::default();
    let original = post_with_card_picture(1400, 733);
    // The picture X would serve for that original below 680 device pixels.
    let small = {
        let pm = ramp(1400, 733);
        let copy = service_copy(
            &DecodedImage {
                width: pm.width,
                height: pm.height,
                rgba: pm.rgba,
            },
            680,
        );
        assert_eq!((copy.width, copy.height), (680, 356));
        let mut out = Pixmap::new(copy.width, copy.height);
        out.rgba = copy.rgba;
        post_with_picture(&out)
    };
    let opts = |dpr: f32| RenderOptions {
        surface: Surface::WebTimeline,
        width: 390.0,
        theme: Theme::Light,
        dpr,
        ..Default::default()
    };
    let cell = |post: &Post, dpr: f32| {
        decode_image(&render(post, &opts(dpr)).unwrap().0, &limits).unwrap()
    };

    // dpr 2: the original goes through the 680 box, and a picture that already
    // is that copy is drawn as given; the cells agree byte for byte.
    let a = cell(&original, 2.0);
    let b = cell(&small, 2.0);
    assert_eq!((a.width, a.height), (b.width, b.height));
    assert_eq!(
        a.rgba, b.rgba,
        "at dpr 2 the original is drawn as its `small` copy"
    );

    // dpr 3: the slot is past 900 device pixels, so the copy is `medium`;
    // the original is drawn as its 1200 copy, byte for byte, and not as the
    // 680 copy stretched.
    let medium = {
        let pm = ramp(1400, 733);
        let copy = service_copy(
            &DecodedImage {
                width: pm.width,
                height: pm.height,
                rgba: pm.rgba,
            },
            1200,
        );
        assert_eq!((copy.width, copy.height), (1200, 628));
        let mut out = Pixmap::new(copy.width, copy.height);
        out.rgba = copy.rgba;
        post_with_picture(&out)
    };
    let c = cell(&original, 3.0);
    let d = cell(&small, 3.0);
    let e = cell(&medium, 3.0);
    assert_eq!((c.width, c.height), (d.width, d.height));
    assert_eq!(
        c.rgba, e.rgba,
        "at dpr 3 the original is drawn as its `medium` copy"
    );
    assert_ne!(
        c.rgba, d.rgba,
        "at dpr 3 the client asks for `medium`, not `small`"
    );

    // The rule at the box the detail page gives a 390 px viewport, 354 css px
    // (measured at dpr 2, where the client asked for `900x900`).
    assert_eq!(service_box(354.0, 2.0), Some(900));
    assert_eq!(service_box(354.0, 3.0), Some(1200));
}

/// The resize is work the raster charge does not see: it walks the source,
/// not the canvas. With a budget that covers the raster and nothing more, a
/// 1200 px picture that has to be resampled into the 680 box fails naming the
/// resample, and the same picture in a cell whose box it fits (600 px at dpr
/// 2, `medium`) renders.
#[test]
fn the_resample_is_charged_to_the_work_budget() {
    use xeet_core::limits::Limits;
    use xeet_core::model::{RenderOptions, Surface};
    use xeet_core::render::{render_with, Error};

    let post = post_with_card_picture(1200, 628);
    let opts = |width: f32| RenderOptions {
        surface: Surface::WebTimeline,
        width,
        theme: Theme::Light,
        dpr: 2.0,
        ..Default::default()
    };
    let limits = |work_base: u64| Limits {
        work_base,
        work_per_char: 0,
        work_per_output_pixel: 0,
        ..Limits::standard()
    };
    // The smallest budget a cell renders on, by bisection.
    let least = |width: f32| {
        let (mut lo, mut hi) = (1u64, 1 << 24);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if render_with(&post, &opts(width), &limits(mid)).is_ok() {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
    };
    // One unit short, the last charge is the one that fails. In the 390 px
    // cell the picture is resampled into the 680 box after the raster is
    // paid, so the resample is what the budget refuses; in the 600 px cell
    // the picture fits `medium`, nothing is resampled, and the raster is the
    // last charge.
    let name_of_the_last_charge =
        |width: f32| match render_with(&post, &opts(width), &limits(least(width) - 1)) {
            Err(Error::Limit(e)) => {
                assert_eq!(e.code, "work_budget_exceeded");
                e.detail
            }
            Err(e) => panic!("expected the work budget to refuse the {width} px cell, got {e:?}"),
            Ok(_) => panic!("the {width} px cell rendered one unit under its least budget"),
        };
    let narrow = name_of_the_last_charge(390.0);
    assert!(
        narrow.contains("resampling card pictures"),
        "the 390 px cell's last charge is the resample: {narrow}"
    );
    let wide = name_of_the_last_charge(600.0);
    assert!(
        wide.contains("rasterizing") && !wide.contains("resampling"),
        "the 600 px cell pays no resample: {wide}"
    );
    // The resample costs source pixels over 8, the raster's scale: 1200 × 628 / 8.
    let with_resample = least(390.0);
    assert!(
        with_resample > 1200 * 628 / 8,
        "the 390 px cell's least budget ({with_resample}) includes the resample's {} units",
        1200 * 628 / 8
    );
}
