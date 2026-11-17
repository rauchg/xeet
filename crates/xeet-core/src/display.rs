//! The display list: what layout hands to the painter.
//!
//! Layout (post frame, cards, media) produces a flat list of items in CSS
//! pixels, painted in order. The painter (raster, and optionally SVG) knows
//! nothing about posts. Keep this the only interface between the two, and
//! keep changes additive so both sides can be developed in parallel.

use crate::color::Rgba;
use crate::fonts::FaceId;
use crate::inline::TextBlock;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
}

/// Corner radii, clockwise from top left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Radii {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl Radii {
    pub const fn all(r: f32) -> Radii {
        Radii {
            tl: r,
            tr: r,
            br: r,
            bl: r,
        }
    }
}

/// CSS `corner-shape`. X uses `squircle` on media where the browser supports it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CornerShape {
    #[default]
    Round,
    /// CSS `corner-shape: squircle` (a superellipse, `superellipse(2)`).
    Squircle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shape {
    pub rect: Rect,
    pub radii: Radii,
    pub corner: CornerShape,
}

impl Shape {
    pub const fn rect(rect: Rect) -> Shape {
        Shape {
            rect,
            radii: Radii::all(0.0),
            corner: CornerShape::Round,
        }
    }
    pub const fn rounded(rect: Rect, r: f32) -> Shape {
        Shape {
            rect,
            radii: Radii::all(r),
            corner: CornerShape::Round,
        }
    }
}

/// One positioned glyph, relative to its run's origin.
#[derive(Clone, Copy, Debug)]
pub struct GlyphPos {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug)]
pub enum Item {
    /// A filled shape.
    Fill {
        shape: Shape,
        color: Rgba,
    },
    /// A border drawn inside the shape's edge, like a CSS border or the
    /// hairline `::after` overlay X puts on media.
    Stroke {
        shape: Shape,
        color: Rgba,
        width: f32,
    },
    /// Glyphs of one face at one size, origin at the baseline.
    Glyphs {
        x: f32,
        y: f32,
        face: FaceId,
        size: f32,
        color: Rgba,
        glyphs: Vec<GlyphPos>,
    },
    /// An icon from X's sprite (`assets/vendor/icons.json`), scaled to `rect`.
    Icon {
        name: String,
        rect: Rect,
        color: Rgba,
    },
    /// A raster image, scaled to cover `shape` (CSS `object-fit: cover`) with
    /// the focal point `focus` in 0..=1 of the source.
    ///
    /// `service` marks a picture the browser gets from X's image service as a
    /// copy fitted to a box rather than as the file. The painter picks the box
    /// from this item's slot width and the render's dpr (`paint::service_box`)
    /// and resizes into it before sampling; `false` draws the file as given.
    Image {
        shape: Shape,
        image: ImageRef,
        focus: (f32, f32),
        service: bool,
    },
    /// Where an image would go when no bytes were given.
    Placeholder {
        shape: Shape,
        label: String,
    },
    /// Clip following items to a shape until the matching `PopClip`.
    PushClip {
        shape: Shape,
    },
    PopClip,
}

/// Which image an `Item::Image` refers to; bytes stay in the `Post`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageRef {
    Avatar,
    /// The author's affiliation badge (`Author::affiliate`).
    AffiliateBadge,
    /// Index into `Post::media`.
    Media(usize),
    /// Avatar of `Post::media[i].source_author` ("Post video" attribution pill).
    MediaSourceAvatar(usize),
    /// Card image of the link with this URL.
    Card(String),
    /// An image inside the quoted post.
    Quote(Box<ImageRef>),
}

/// A complete frame, in CSS pixels. The painter multiplies by the DPR.
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    pub width: f32,
    pub height: f32,
    pub background: Rgba,
    pub items: Vec<Item>,
}

impl DisplayList {
    pub fn new(width: f32, height: f32, background: Rgba) -> DisplayList {
        DisplayList {
            width,
            height,
            background,
            items: Vec::new(),
        }
    }

    pub fn push(&mut self, item: Item) {
        self.items.push(item);
    }

    /// Adds a laid-out text block with its top-left corner at (`x`, `y`).
    /// One `Glyphs` item per run and color.
    pub fn push_text(&mut self, block: &TextBlock, x: f32, y: f32) {
        for line in &block.lines {
            let baseline = y + line.top + line.baseline;
            for run in &line.runs {
                // A run covers one contiguous slice of text, so its span
                // indices never decrease: equal first and last means one color,
                // and then every glyph of the run lands in one batch that can
                // be sized exactly, instead of growing by doubling.
                let single =
                    run.glyphs.first().map(|g| g.span) == run.glyphs.last().map(|g| g.span);
                let cap = if single { run.glyphs.len() } else { 0 };
                let mut cur: Option<(Rgba, Vec<GlyphPos>)> = None;
                for g in &run.glyphs {
                    let color = block
                        .spans
                        .get(g.span as usize)
                        .map_or(Rgba::BLACK, |s| s.color);
                    let pos = GlyphPos {
                        id: g.id,
                        x: g.x,
                        y: g.y,
                    };
                    match &mut cur {
                        Some((c, v)) if *c == color => v.push(pos),
                        _ => {
                            if let Some((c, v)) = cur.take() {
                                self.items.push(Item::Glyphs {
                                    x: x + run.x,
                                    y: baseline,
                                    face: run.face,
                                    size: block.size,
                                    color: c,
                                    glyphs: v,
                                });
                            }
                            let mut v = Vec::with_capacity(cap);
                            v.push(pos);
                            cur = Some((color, v));
                        }
                    }
                }
                if let Some((c, v)) = cur {
                    self.items.push(Item::Glyphs {
                        x: x + run.x,
                        y: baseline,
                        face: run.face,
                        size: block.size,
                        color: c,
                        glyphs: v,
                    });
                }
            }
        }
    }
}
