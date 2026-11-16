//! A source-independent document view: blocks in, PNG plus element bounds out.
//!
//! Adapters for sources without a faithful product renderer describe *what to
//! show* as a list of [`Block`]s, each tied to a semantic element id; this
//! module decides *where*, using xeet-core's text layout (HarfBuzz shaping,
//! Blink line breaking) and painter. The result is deterministic, needs no
//! network and no system fonts, and every block's bounds are the boxes it was
//! actually painted in — so the semantic graph points at real pixels.
//!
//! It is a faithful layout of the content, not a replica of any product's UI:
//! views made here carry `FidelityClaim::Generic`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use xeet_core::color::Rgba;
use xeet_core::display::{DisplayList, Item, Rect, Shape};
use xeet_core::inline::{self, BlockStyle, Dir, Span, TextBlock, Wrap};
use xeet_core::model::Theme;
use xeet_core::paint;
use xeet_core::tokens;

/// The renderer's name and version, as artifacts record them.
pub const RENDERER: &str = "xeet-capture/view";
/// Bump when any layout constant below changes: it changes the bytes.
pub const LAYOUT_VERSION: &str = "1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tone {
    #[default]
    Neutral,
    Success,
    Done,
    Danger,
    Attention,
    Accent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Block {
    Heading {
        el: String,
        text: String,
    },
    /// A row of pills: statuses, labels.
    Badges {
        items: Vec<Badge>,
    },
    /// Secondary text.
    Meta {
        el: String,
        text: String,
    },
    /// Prose, whitespace preserved. `maxLines` clamps it (and says so).
    Paragraph {
        el: String,
        text: String,
        max_lines: Option<usize>,
    },
    /// A row of short colored values.
    Stats {
        items: Vec<Stat>,
    },
    /// A titled list.
    Section {
        el: String,
        title: String,
        items: Vec<Entry>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Badge {
    pub el: String,
    pub text: String,
    pub tone: Tone,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stat {
    pub el: String,
    pub text: String,
    pub tone: Tone,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub el: String,
    pub primary: String,
    pub secondary: Option<String>,
    pub tone: Tone,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewOptions {
    pub width: f32,
    pub theme: Theme,
    pub scale: f32,
}

impl Default for ViewOptions {
    fn default() -> Self {
        ViewOptions {
            width: 600.0,
            theme: Theme::Light,
            scale: 2.0,
        }
    }
}

pub struct Rendered {
    pub png: Vec<u8>,
    pub width: f32,
    pub height: f32,
    /// Element id → painted box, in layout units.
    pub bounds: BTreeMap<String, Rect>,
    /// Element ids whose text was clamped.
    pub clamped: Vec<String>,
}

const PAD: f32 = 16.0;
const GAP: f32 = 12.0;

fn tone_color(tone: Tone, c: &tokens::Colors) -> Rgba {
    match tone {
        Tone::Neutral => c.fg_secondary,
        Tone::Success => Rgba::hex(0x1a7f37),
        Tone::Done => Rgba::hex(0x8250df),
        Tone::Danger => Rgba::hex(0xcf222e),
        Tone::Attention => Rgba::hex(0x9a6700),
        Tone::Accent => c.fg_brand,
    }
}

fn style(size: f32, lh: f32, weight: u16, width: f32, max_lines: Option<usize>) -> BlockStyle {
    BlockStyle {
        size,
        line_height: lh,
        weight,
        width,
        max_lines,
        wrap: Wrap::Anywhere,
        preserve: true,
        dir: Dir::Auto,
        tab_size: 4.0,
    }
}

fn text(s: &str, color: Rgba, st: &BlockStyle) -> TextBlock {
    inline::layout(&[Span::new(s, color, st.weight)], st)
}

/// Lays out and paints a document. Pure: same blocks and options, same bytes.
pub fn render(blocks: &[Block], opts: &ViewOptions) -> Rendered {
    let width = if opts.width.is_finite() {
        opts.width.clamp(240.0, 1200.0)
    } else {
        600.0
    };
    let scale = if opts.scale.is_finite() && opts.scale > 0.0 {
        opts.scale.clamp(1.0, 3.0)
    } else {
        2.0
    };
    let c = tokens::colors(opts.theme);
    let inner = width - 2.0 * PAD;
    let mut bounds = BTreeMap::new();
    let mut clamped = Vec::new();
    let mut list = DisplayList::new(width, 0.0, c.bg_primary);
    let mut y = PAD;

    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            y += GAP;
        }
        match block {
            Block::Heading { el, text: t } => {
                let b = text(t, c.fg_primary, &style(20.0, 24.0, 700, inner, None));
                list.push_text(&b, PAD, y);
                bounds.insert(el.clone(), Rect::new(PAD, y, b.width, b.height));
                y += b.height;
            }
            Block::Meta { el, text: t } => {
                let b = text(t, c.fg_secondary, &style(13.0, 16.0, 400, inner, None));
                list.push_text(&b, PAD, y);
                bounds.insert(el.clone(), Rect::new(PAD, y, b.width, b.height));
                y += b.height;
            }
            Block::Paragraph {
                el,
                text: t,
                max_lines,
            } => {
                let b = text(t, c.fg_primary, &style(15.0, 20.0, 400, inner, *max_lines));
                if b.truncated {
                    clamped.push(el.clone());
                }
                list.push_text(&b, PAD, y);
                bounds.insert(el.clone(), Rect::new(PAD, y, inner, b.height));
                y += b.height;
            }
            Block::Badges { items: badges } => {
                let (h, placed) = flow(
                    badges
                        .iter()
                        .map(|b| (b.el.as_str(), b.text.as_str(), b.tone))
                        .collect(),
                    inner,
                    true,
                    &c,
                );
                for (el, rect, block, tone) in placed {
                    let fill = tone_color(tone, &c);
                    items_push_fill(
                        &mut list,
                        Rect::new(PAD + rect.x, y + rect.y, rect.w, rect.h),
                        rect.h / 2.0,
                        fill,
                    );
                    list.push_text(&block, PAD + rect.x + 10.0, y + rect.y + 4.0);
                    bounds.insert(
                        el.to_string(),
                        Rect::new(PAD + rect.x, y + rect.y, rect.w, rect.h),
                    );
                }
                y += h;
            }
            Block::Stats { items: stats } => {
                let (h, placed) = flow(
                    stats
                        .iter()
                        .map(|s| (s.el.as_str(), s.text.as_str(), s.tone))
                        .collect(),
                    inner,
                    false,
                    &c,
                );
                for (el, rect, block, _) in placed {
                    list.push_text(&block, PAD + rect.x, y + rect.y);
                    bounds.insert(
                        el.to_string(),
                        Rect::new(PAD + rect.x, y + rect.y, rect.w, rect.h),
                    );
                }
                y += h;
            }
            Block::Section {
                el,
                title,
                items: entries,
            } => {
                let top = y;
                items_push_fill(
                    &mut list,
                    Rect::new(PAD, y, inner, 1.0),
                    0.0,
                    c.border_divider,
                );
                y += GAP;
                let t = text(title, c.fg_primary, &style(15.0, 20.0, 700, inner, None));
                list.push_text(&t, PAD, y);
                y += t.height;
                for e in entries {
                    y += 6.0;
                    let start = y;
                    let color = if e.tone == Tone::Neutral {
                        c.fg_primary
                    } else {
                        tone_color(e.tone, &c)
                    };
                    let p = text(&e.primary, color, &style(14.0, 18.0, 400, inner, Some(3)));
                    if p.truncated {
                        clamped.push(e.el.clone());
                    }
                    list.push_text(&p, PAD, y);
                    y += p.height;
                    let mut w = p.width;
                    if let Some(sec) = &e.secondary {
                        let s = text(sec, c.fg_secondary, &style(13.0, 16.0, 400, inner, Some(2)));
                        list.push_text(&s, PAD, y);
                        y += s.height;
                        w = w.max(s.width);
                    }
                    bounds.insert(e.el.clone(), Rect::new(PAD, start, w, y - start));
                }
                bounds.insert(el.clone(), Rect::new(PAD, top, inner, y - top));
            }
        }
    }
    let height = (y + PAD).ceil();
    list.height = height;
    let png = paint::rasterize_png(&list, scale, &paint::NoImages);
    Rendered {
        png,
        width,
        height,
        bounds,
        clamped,
    }
}

fn items_push_fill(list: &mut DisplayList, rect: Rect, radius: f32, color: Rgba) {
    list.push(Item::Fill {
        shape: Shape::rounded(rect, radius),
        color,
    });
}

/// Places short single-line items left to right, wrapping to new rows.
/// Returns the total height and each item's box relative to the row origin.
fn flow<'a>(
    items: Vec<(&'a str, &'a str, Tone)>,
    inner: f32,
    pill: bool,
    c: &tokens::Colors,
) -> (f32, Vec<(&'a str, Rect, TextBlock, Tone)>) {
    let (size, lh, weight) = if pill {
        (12.0, 16.0, 700)
    } else {
        (13.0, 16.0, 700)
    };
    let pad_x = if pill { 10.0 } else { 0.0 };
    let pad_y = if pill { 4.0 } else { 0.0 };
    let gap = if pill { 6.0 } else { 14.0 };
    let row_h = lh + 2.0 * pad_y;
    let mut out = Vec::new();
    let (mut x, mut y) = (0.0f32, 0.0f32);
    for (el, t, tone) in items {
        let color = if pill {
            Rgba::WHITE
        } else {
            tone_color(tone, c)
        };
        let b = text(
            t,
            color,
            &style(size, lh, weight, inner - 2.0 * pad_x, Some(1)),
        );
        let w = (b.width + 2.0 * pad_x).min(inner);
        if x > 0.0 && x + w > inner {
            x = 0.0;
            y += row_h + gap / 2.0;
        }
        out.push((el, Rect::new(x, y, w, row_h), b, tone));
        x += w + gap;
    }
    let h = if out.is_empty() { 0.0 } else { y + row_h };
    (h, out)
}
