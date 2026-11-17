//! Paints a display list described in JSON, for oracle fixtures and debugging.
//!
//! This is the painter's front door for the Chrome oracles: it takes geometry,
//! colors and images from a file so the same scene can be rendered by a page
//! in Chrome and by the real rasterizer here.
//!
//! ```sh
//! cat scene.json | cargo run --profile fast --example paint_fixture -- -o out.png
//! ```
//!
//! Scene format (all geometry in CSS pixels):
//!
//! ```json
//! {
//!   "width": 200, "height": 120, "dpr": 2, "background": "#ffffff",
//!   "items": [
//!     {"type":"fill","rect":[8,8,80,40],"radii":[8,8,8,8],"corner":"squircle","color":"#1d9bf0"},
//!     {"type":"stroke","rect":[8,56,80,40],"radii":16,"color":"#0f1419","width":1},
//!     {"type":"icon","name":"icon-heart","rect":[100,8,24,24],"color":"#536471"},
//!     {"type":"image","rect":[100,40,80,60],"radii":8,"image":"media:0","path":"img.png","focus":[0.5,0.5]},
//!     {"type":"placeholder","rect":[8,100,80,60],"label":"Photo"},
//!     {"type":"text","text":"Hi","x":8,"y":110,"width":180,"size":15,"lh":20},
//!     {"type":"pushclip","rect":[0,0,100,200]}, {"type":"popclip"}
//!   ]
//! }
//! ```
//!
//! `radii` is a single number or `[tl, tr, br, bl]`. `image` names the
//! `ImageRef` (`avatar`, `media:N`, `card:URL`, `quote:...`) and `path` is the
//! file its bytes come from.

use std::io::Read;
use xeet_core::color::Rgba;
use xeet_core::display::{CornerShape, DisplayList, ImageRef, Item, Radii, Rect, Shape};
use xeet_core::inline::{self, BlockStyle, Dir, Span, Wrap};
use xeet_core::paint::{self, DecodedImage, ImageSource, Limits};

#[derive(Default)]
struct Files {
    images: Vec<(ImageRef, DecodedImage)>,
}

impl ImageSource for Files {
    fn rgba(&self, image: &ImageRef) -> Option<DecodedImage> {
        self.images
            .iter()
            .find(|(r, _)| r == image)
            .map(|(_, d)| d.clone())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let out = flag("-o").unwrap_or_else(|| "fixture.png".to_string());
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("stdin");
    let scene: serde_json::Value = serde_json::from_str(&input).expect("scene json");

    let num =
        |v: &serde_json::Value, key: &str, d: f32| v[key].as_f64().map(|f| f as f32).unwrap_or(d);
    let color = |v: &serde_json::Value, key: &str, d: Rgba| {
        v[key].as_str().and_then(Rgba::parse_hex).unwrap_or(d)
    };
    let rect_of = |v: &serde_json::Value| -> Rect {
        let a = v["rect"].as_array();
        match a {
            Some(a) if a.len() == 4 => Rect::new(
                a[0].as_f64().unwrap_or(0.0) as f32,
                a[1].as_f64().unwrap_or(0.0) as f32,
                a[2].as_f64().unwrap_or(0.0) as f32,
                a[3].as_f64().unwrap_or(0.0) as f32,
            ),
            _ => Rect::default(),
        }
    };
    let radii_of = |v: &serde_json::Value| -> Radii {
        match &v["radii"] {
            serde_json::Value::Number(n) => Radii::all(n.as_f64().unwrap_or(0.0) as f32),
            serde_json::Value::Array(a) if a.len() == 4 => Radii {
                tl: a[0].as_f64().unwrap_or(0.0) as f32,
                tr: a[1].as_f64().unwrap_or(0.0) as f32,
                br: a[2].as_f64().unwrap_or(0.0) as f32,
                bl: a[3].as_f64().unwrap_or(0.0) as f32,
            },
            _ => Radii::default(),
        }
    };
    let shape_of = |v: &serde_json::Value| -> Shape {
        Shape {
            rect: rect_of(v),
            radii: radii_of(v),
            corner: match v["corner"].as_str() {
                Some("squircle") => CornerShape::Squircle,
                _ => CornerShape::Round,
            },
        }
    };

    let width = num(&scene, "width", 100.0);
    let height = num(&scene, "height", 100.0);
    let dpr = num(&scene, "dpr", 1.0);
    let background = scene["background"]
        .as_str()
        .and_then(Rgba::parse_hex)
        .unwrap_or(Rgba::WHITE);
    let mut list = DisplayList::new(width, height, background);
    let mut files = Files::default();
    let image_ref = |name: &str| -> ImageRef {
        match name.strip_prefix("media:") {
            Some(i) => ImageRef::Media(i.parse().unwrap_or(0)),
            None => match name {
                "avatar" => ImageRef::Avatar,
                other => match other.strip_prefix("card:") {
                    Some(url) => ImageRef::Card(url.to_string()),
                    None => match other.strip_prefix("quote:") {
                        Some(inner) => ImageRef::Quote(Box::new(ImageRef::Card(inner.to_string()))),
                        None => ImageRef::Media(0),
                    },
                },
            },
        }
    };

    for item in scene["items"].as_array().cloned().unwrap_or_default() {
        match item["type"].as_str().unwrap_or("") {
            "fill" => list.push(Item::Fill {
                shape: shape_of(&item),
                color: color(&item, "color", Rgba::BLACK),
            }),
            "stroke" => list.push(Item::Stroke {
                shape: shape_of(&item),
                color: color(&item, "color", Rgba::BLACK),
                width: num(&item, "width", 1.0),
            }),
            "icon" => list.push(Item::Icon {
                name: item["name"].as_str().unwrap_or("").to_string(),
                rect: rect_of(&item),
                color: color(&item, "color", Rgba::BLACK),
            }),
            "image" => {
                let name = item["image"].as_str().unwrap_or("media:0");
                let r = image_ref(name);
                if let Some(path) = item["path"].as_str() {
                    if let Ok(bytes) = std::fs::read(path) {
                        match paint::decode_image(&bytes, &Limits::default()) {
                            Ok(img) => {
                                if !files.images.iter().any(|(k, _)| *k == r) {
                                    files.images.push((r.clone(), img));
                                }
                            }
                            Err(e) => eprintln!("{path}: {e}"),
                        }
                    }
                }
                let focus = item["focus"].as_array().map_or((0.5, 0.5), |a| {
                    (
                        a.first().and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
                        a.get(1).and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
                    )
                });
                list.push(Item::Image {
                    shape: shape_of(&item),
                    image: r,
                    focus,
                    service: false,
                });
            }
            "placeholder" => list.push(Item::Placeholder {
                shape: shape_of(&item),
                label: item["label"].as_str().unwrap_or("").to_string(),
            }),
            "pushclip" => list.push(Item::PushClip {
                shape: shape_of(&item),
            }),
            "popclip" => list.push(Item::PopClip),
            "text" => {
                let st = BlockStyle {
                    size: num(&item, "size", 15.0),
                    line_height: num(&item, "lh", 20.0),
                    weight: num(&item, "weight", 400.0) as u16,
                    width: num(&item, "width", width),
                    max_lines: item["clamp"].as_u64().map(|v| v as usize),
                    wrap: Wrap::Anywhere,
                    preserve: true,
                    dir: Dir::Auto,
                    tab_size: 4.0,
                };
                let text = item["text"].as_str().unwrap_or("");
                let block = inline::layout(
                    &[Span::new(
                        text,
                        color(&item, "color", Rgba::BLACK),
                        st.weight,
                    )],
                    &st,
                );
                list.push_text(&block, num(&item, "x", 0.0), num(&item, "y", 0.0));
            }
            other => eprintln!("unknown item type {other:?}"),
        }
    }

    let pixmap = paint::rasterize(&list, dpr, &files);
    let png = paint::encode_png(&pixmap);
    std::fs::write(&out, &png).expect("write png");
    if let Some(svg_path) = flag("--svg") {
        std::fs::write(svg_path, paint::to_svg(&list)).expect("write svg");
    }
    println!(
        "{}: {}x{} px, {} items, {} bytes",
        out,
        pixmap.width,
        pixmap.height,
        list.items.len(),
        png.len()
    );
}
