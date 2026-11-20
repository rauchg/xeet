//! The attachment slot: media (images, carousels, video, GIF), link cards,
//! quoted posts and polls.
//!
//! Everything here is a port of X's own layout code, not a look-alike: the
//! media carousel is a line-by-line port of the `mediaCarouselLayout` module
//! in the shipped client bundle (`DIdg36-I.js`, app version `89824c04`), and
//! every constant comes from `assets/vendor/upstream/styles.css`. The evidence
//! for each number (which capture, which computed style) is in
//! `docs/CARDS.md`.
//!
//! A [`Block`] is positioned by the frame: items are relative to the block's
//! top-left corner, `height` is the block's height, and the block is exactly
//! `width` wide. Nothing here reads the clock, the filesystem or the network.
//!
//! ## Theme
//!
//! Colors are the *computed* values of X's utility classes in each theme:
//! `text-primary` is `#000` on light and `#fff` on dark, `text-secondary` is
//! `#666`/`#999`, `bg-secondary` is `#f2f2f2`/`#141414`, and `border-normal`
//! is `rgb(0 0 0 / 15%)`/`rgb(255 255 255 / 15%)`.
//!
//! ## Corner radii
//!
//! X multiplies rounded corners by `--x-radius-m`, which is `1` by default and
//! `1.8` inside the stylesheet's `@supports (corner-shape: squircle)` block —
//! and every browser we target supports it. So `rounded-md` is 28.8 px drawn
//! as a squircle, `rounded-sm` is 14.4 px, `rounded-xs` is 7.2 px.
//! `rounded-2xl` (the quoted-post frame) is *not* in that list: it stays 16 px
//! and round.

use crate::color::Rgba;
use crate::display::{self, CornerShape, Item, Radii, Rect, Shape};
use crate::inline::{self, BlockStyle, Dir, Span, TextBlock, Wrap};
use crate::model::{self, Theme};

mod card;
mod media;
mod poll;
mod quote;
mod time;

pub use media::{layout_carousel, single_box, MediaLayout};
pub use time::time_label;

/// What X chose to show in the attachment slot, decided by the rules stream.
pub use crate::rules::AttachmentKind;

/// A laid-out attachment: display items relative to the block's origin.
#[derive(Clone, Debug, Default)]
pub struct Block {
    pub items: Vec<Item>,
    pub height: f32,
}

impl Block {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.height == 0.0
    }
}

/// Lays out the attachment for `post` in a slot `width` CSS pixels wide.
///
/// `attachment` is what the rules stream decided to show; the matching payload
/// (`post.links`, `post.media`, `post.quote`, `post.poll`) is read from `post`.
/// If the payload behind the decision is missing the block is empty, so a
/// caller never gets half a card.
pub fn layout(
    post: &model::Post,
    opts: &model::RenderOptions,
    attachment: &AttachmentKind,
    width: f32,
) -> Block {
    let body_empty = matches!(attachment, AttachmentKind::Media)
        && crate::rules::decide(post, opts)
            .body
            .iter()
            .all(|s| s.text.trim().is_empty());
    layout_with_body(post, opts, attachment, width, body_empty)
}

/// [`layout`] with the caller's already-decided `body_empty` flag, so `frame`
/// avoids running `rules::decide` a second time.
pub fn layout_with_body(
    post: &model::Post,
    opts: &model::RenderOptions,
    attachment: &AttachmentKind,
    width: f32,
    body_empty: bool,
) -> Block {
    if !(width > 0.0) || !width.is_finite() {
        return Block::default();
    }
    let cx = Ctx::new(opts, width);
    let mut out: Vec<Item> = Vec::new();
    let is_timeline = matches!(
        opts.surface,
        model::Surface::WebTimeline | model::Surface::IosTimeline
    );
    let height = match attachment {
        AttachmentKind::Media => {
            let items = media::items_of(&post.media, &post.author.handle);
            if items.is_empty() {
                return Block::default();
            }
            let mut h = media::layout(&cx, &items, 0.0, &mut out, body_empty && is_timeline);
            // Media and a quoted post are shown *together*: the slot is a
            // `flex flex-col gap-3` whose children are the media wrapper and the
            // quote. (`rules::decide` reports the media attachment; the quote is
            // not suppressed, so it is laid out here.)
            if let Some(q) = post.quote.as_deref().filter(|q| quote_visible(q)) {
                // Timeline cells collapse a quote under media to the compact
                // side-thumbnail form (`NBA.html`); detail pages keep the
                // full-width quote with full-aspect media below the text
                // (`anabology/status/2095355496530071898`).
                h += GAP_ATTACHMENT
                    + quote::layout(
                        &cx,
                        post,
                        q,
                        opts,
                        is_timeline,
                        h + GAP_ATTACHMENT,
                        &mut out,
                    );
                return Block {
                    items: out,
                    height: h,
                };
            }
            if !post.media_tags.is_empty() {
                // The tag line is the next child of the `flex flex-col gap-3`
                // wrapper (12 px) and carries `mt-1` itself (4 px).
                h += 16.0 + media::tag_line(&cx, &post.media_tags, h + 16.0, &mut out);
            }
            h
        }
        AttachmentKind::Card { url, card } => {
            let Some(meta) = post.links.get(url) else {
                return Block::default();
            };
            card::layout(&cx, meta, card, url, 0.0, &mut out)
        }
        AttachmentKind::Quote => {
            let Some(q) = post.quote.as_deref() else {
                return Block::default();
            };
            // The quote's own clock sample wins; `quote::layout` falls back to
            // the outer post's `now`, which is the one a payload normally has.
            quote::layout(&cx, post, q, opts, false, 0.0, &mut out)
        }
        AttachmentKind::Poll => {
            let Some(p) = post.poll.as_ref() else {
                return Block::default();
            };
            poll::layout(&cx, p, 0.0, &mut out)
        }
    };
    Block { items: out, height }
}

/// True when a quoted post has anything to draw. An empty quote (the payload
/// kept the wrapper but nothing in it) is skipped rather than drawn as an empty
/// frame.
pub(crate) fn quote_visible(q: &model::Post) -> bool {
    !q.text.trim().is_empty() || !q.media.is_empty() || q.poll.is_some() || !q.links.is_empty()
}

/// `gap-3` between the children of an attachment slot (media, quote, tag line).
const GAP_ATTACHMENT: f32 = 12.0;

/// Aspect ratio used when a media item's dimensions are unknown: the 16/9 a
/// video card falls back to (`j7em2Hk6.js`: `?? 1.7777777777777777`).
pub(crate) const DEFAULT_ASPECT: f64 = 16.0 / 9.0;

/// Type scale, in CSS pixels, before the user's font-scale setting.
pub(crate) const BODY: (f32, f32) = (15.0, 20.0);
pub(crate) const SUBTEXT1: (f32, f32) = (14.0, 16.0);
pub(crate) const SUBTEXT2: (f32, f32) = (13.0, 16.0);
/// `text-subtext3`: 11/12.
pub(crate) const SUBTEXT3: (f32, f32) = (11.0, 12.0);

/// Colors for one theme.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Palette {
    /// `text-primary`.
    pub primary: Rgba,
    /// `text-secondary`.
    pub secondary: Rgba,
    /// `border-normal`: `--x-black-15` / `--x-white-15`.
    pub border: Rgba,
    /// `bg-secondary`: `--x-neutral-000` / `--x-neutral-1000`.
    pub bg_secondary: Rgba,
    /// `bg-tertiary`: `--x-neutral-050` / `--x-neutral-900`. The nested quote
    /// preview's 3 px bar.
    pub bg_tertiary: Rgba,
    /// `bg-black/60`: the nested quote preview's play circle.
    pub black_60: Rgba,
    /// `bg-black-40`: the play button and the ALT badge.
    pub black_40: Rgba,
    /// `bg-black/70`: the photo form of the source attribution badge.
    pub black_70: Rgba,
    /// `bg-black-75`: the link-card title pill.
    pub black_75: Rgba,
    /// `bg-black-80`: the video duration badge.
    pub black_80: Rgba,
    /// `text-on-color`: white, used on every dark overlay.
    pub on_color: Rgba,
    /// `--x-border-active`, the poll result bar.
    pub poll_bar: Rgba,
    /// `--x-fg-brand`, the filled poll bar for a winner.
    pub brand: Rgba,
    /// `bg-brand-wash`: the winner's bar. In the light and dark themes this is
    /// the solid brand color; the 58% mix only applies to high contrast.
    pub poll_brand: Rgba,
}

impl Palette {
    pub(crate) fn of(theme: Theme) -> Palette {
        match theme {
            Theme::Light => Palette {
                primary: Rgba::rgb(0, 0, 0),
                secondary: Rgba::rgb(0x66, 0x66, 0x66),
                border: Rgba::rgba(0, 0, 0, 0x26),
                bg_secondary: Rgba::rgb(0xf2, 0xf2, 0xf2),
                bg_tertiary: Rgba::rgb(0xeb, 0xeb, 0xeb),
                black_60: Rgba::rgba(0, 0, 0, 0x99),
                black_40: Rgba::rgba(0, 0, 0, 0x66),
                black_70: Rgba::rgba(0, 0, 0, 0xb3),
                black_75: Rgba::rgba(0, 0, 0, 0xbf),
                black_80: Rgba::rgba(0, 0, 0, 0xcc),
                on_color: Rgba::WHITE,
                poll_bar: Rgba::rgba(0, 0, 0, 0x4d),
                brand: Rgba::rgb(0x1d, 0x9b, 0xf0),
                poll_brand: Rgba::rgb(0x1d, 0x9b, 0xf0),
            },
            Theme::Dark => Palette {
                primary: Rgba::WHITE,
                secondary: Rgba::rgb(0x99, 0x99, 0x99),
                border: Rgba::rgba(255, 255, 255, 0x26),
                bg_secondary: Rgba::rgb(0x14, 0x14, 0x14),
                bg_tertiary: Rgba::rgb(0x26, 0x26, 0x26),
                black_60: Rgba::rgba(0, 0, 0, 0x99),
                black_40: Rgba::rgba(0, 0, 0, 0x66),
                black_70: Rgba::rgba(0, 0, 0, 0xb3),
                black_75: Rgba::rgba(0, 0, 0, 0xbf),
                black_80: Rgba::rgba(0, 0, 0, 0xcc),
                on_color: Rgba::WHITE,
                poll_bar: Rgba::rgba(255, 255, 255, 0x4d),
                brand: Rgba::rgb(0x1d, 0x9b, 0xf0),
                poll_brand: Rgba::rgb(0x1d, 0x9b, 0xf0),
            },
        }
    }
}

/// Layout context: palette, font scale, slot width and viewport width.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ctx {
    pub p: Palette,
    /// The user's font-scale setting; X multiplies every `text-*` size by it.
    pub scale: f32,
    /// Width of the slot the block is laid out in.
    pub width: f32,
    /// Viewport width, for X's `sm:` (640 px) breakpoints inside cards.
    pub viewport: f32,
    /// Native iOS surface (`ios-timeline` / `ios-detail`).
    pub ios: bool,
    /// `--media-overhang-end`: how far an overflowing carousel bleeds past
    /// `width` toward the card's right edge (`16px` web, `9pt`/`12pt` iOS).
    pub overhang_end: f32,
}

impl Ctx {
    fn new(opts: &model::RenderOptions, width: f32) -> Ctx {
        let scale = if opts.font_scale > 0.0 {
            opts.font_scale
        } else {
            1.0
        };
        let ios = opts.surface.is_ios();
        let mut p = Palette::of(opts.theme);
        if ios && opts.theme == Theme::Dark {
            p.border = Rgba::rgb(0x33, 0x36, 0x39);
        }
        let max_overhang: f32 = match opts.surface {
            model::Surface::WebTimeline | model::Surface::WebDetail => 16.0,
            model::Surface::IosTimeline => 9.0,
            model::Surface::IosDetail => 12.0,
        };
        let overhang_end = if opts.width.is_finite() && opts.width >= width {
            max_overhang.min(opts.width - width)
        } else {
            0.0
        };
        Ctx {
            p,
            scale,
            width,
            viewport: opts.width,
            ios,
            overhang_end,
        }
    }

    /// A type-scale pair, scaled by the user's font scale.
    pub(crate) fn ty(&self, t: (f32, f32)) -> (f32, f32) {
        (t.0 * self.scale, t.1 * self.scale)
    }

    /// The radius of a carousel cell's image: the cell's `rounded-sm` minus its
    /// 1 px border, i.e. the concentric inner curve Chrome clips the image to.
    pub(crate) fn cell_image_radius(&self, w: f32, h: f32) -> f32 {
        let r = crate::tokens::RADIUS_SM_RADIUS.value.min(w.min(h) * 0.5);
        (r - 1.0).max(0.0)
    }

    /// A length in `em`, scaled by the font setting (icons sized `1em`).
    pub(crate) fn em_box(&self, size: f32) -> f32 {
        size * self.scale
    }
}

/// Snaps a length to Chrome's layout unit, 1/64 px.
///
/// Blink stores every used length as a `LayoutUnit`, whose float constructor
/// truncates (`static_cast<int>(value * 64)`). A width derived from an
/// `aspect-ratio` therefore lands on the 1/64 grid *below* the exact product,
/// and a carousel's later cells start up to 1/64 px earlier than float math
/// would put them. Matching that truncation is what keeps a four-image
/// carousel inside the 1/64 px gate.
pub(crate) fn snap(v: f64) -> f32 {
    ((v * 64.0).floor() / 64.0) as f32
}

/// A rounded, optionally squared-off box.
pub(crate) fn shape(rect: Rect, r: f32, corner: CornerShape) -> Shape {
    Shape {
        rect,
        radii: Radii::all(r),
        corner,
    }
}

/// A box with one of X's radius tokens.
///
/// The tokens come from the stylesheet through [`crate::tokens`], so a change
/// up there (radius scale, squircle support) flows through to cards. The value
/// is clamped to half the shorter side, which is what a browser does with
/// `rounded-full`'s 9999 px.
pub(crate) fn rounded(rect: Rect, r: crate::tokens::Radius) -> Shape {
    Shape {
        rect,
        radii: Radii::all(r.value.min(rect.w.min(rect.h) * 0.5)),
        corner: r.corner,
    }
}

/// Fills `shape` with `color`.
pub(crate) fn fill(out: &mut Vec<Item>, shape: Shape, color: Rgba) {
    if shape.rect.w > 0.0 && shape.rect.h > 0.0 {
        out.push(Item::Fill { shape, color });
    }
}

/// Draws a 1 px inset border, X's `border` / `after:border` hairline.
pub(crate) fn outline(out: &mut Vec<Item>, shape: Shape, color: Rgba) {
    if shape.rect.w > 0.0 && shape.rect.h > 0.0 {
        out.push(Item::Stroke {
            shape,
            color,
            width: 1.0,
        });
    }
}

/// Pushes a laid-out text block with its top-left corner at (`x`, `y`).
///
/// `inline::layout` places a line's baseline at
/// `ascent + floor(leading / 2)` with integer-rounded Chirp metrics; that is
/// exactly where Chrome puts the font box too, verified across the type scale
/// at 13/16, 14/16, 15/20, 17/20 and 20/24 (`tools/oracle/cards-parity.mjs`
/// compares the line box, since `-webkit-line-clamp` shifts a *range* rect by a
/// fraction of a pixel inside a clamped box).
pub(crate) fn push_text(out: &mut Vec<Item>, block: &TextBlock, x: f32, y: f32, width: f32) {
    // Chrome's `line-clamp-*` boxes are `overflow: hidden`, so a clamped line
    // that would paint past its box is cut. Frame does the same for post text
    // (`crates/xeet-core/src/frame/header.rs`); the clip here is a no-op for
    // text that fits.
    let clip = Rect::new(x, y, width, block.height);
    let clipped = block.height > 0.0 && width > 0.0;
    if clipped {
        out.push(Item::PushClip {
            shape: Shape::rect(clip),
        });
    }
    let mut dl = display::DisplayList::default();
    dl.push_text(block, x, y);
    out.append(&mut dl.items);
    if clipped {
        out.push(Item::PopClip);
    }
}

/// Shifts a batch of items, used to place a nested component (a quote's media,
/// a card inside a quote) inside its parent.
pub(crate) fn shift(items: &mut [Item], dx: f32, dy: f32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let move_shape = |s: &mut Shape| {
        s.rect.x += dx;
        s.rect.y += dy;
    };
    for item in items {
        match item {
            Item::Fill { shape, .. }
            | Item::Stroke { shape, .. }
            | Item::Image { shape, .. }
            | Item::Placeholder { shape, .. }
            | Item::PushClip { shape } => move_shape(shape),
            Item::Glyphs { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Item::Icon { rect, .. } => {
                rect.x += dx;
                rect.y += dy;
            }
            Item::PopClip => {}
        }
    }
}

/// Lays out one paragraph of X's text: `white-space: pre-wrap`,
/// `overflow-wrap: anywhere`, optionally clamped with a trailing ellipsis.
pub(crate) fn paragraph(
    cx: &Ctx,
    text: &str,
    size: (f32, f32),
    weight: u16,
    color: Rgba,
    width: f32,
    max_lines: Option<usize>,
) -> TextBlock {
    paragraph_wrapped(
        cx,
        text,
        size,
        weight,
        color,
        width,
        max_lines,
        Wrap::Anywhere,
    )
}

/// Like [`paragraph`], with an explicit wrap mode. X only sets
/// `overflow-wrap: anywhere` where the markup says `wrap-anywhere`; plain
/// `line-clamp-1` text (card titles, the `From …` line) breaks at word
/// boundaries, and that changes where the ellipsis lands.
pub(crate) fn paragraph_wrapped(
    cx: &Ctx,
    text: &str,
    size: (f32, f32),
    weight: u16,
    color: Rgba,
    width: f32,
    max_lines: Option<usize>,
    wrap: Wrap,
) -> TextBlock {
    let (size, line_height) = cx.ty(size);
    let span = Span::new(text, color, weight);
    let st = BlockStyle {
        size,
        line_height,
        weight,
        width,
        max_lines,
        wrap,
        preserve: true,
        dir: Dir::Auto,
        tab_size: 4.0,
    };
    inline::layout(&[span], &st)
}

/// Like [`paragraph`], for a box with no `dir` of its own: it takes the page's
/// `ltr`. The media tag line is one (`div.w-fit`, no `dir`), so a right-to-left
/// name starts at its left edge and "A and 6 others" keeps its English order.
/// Names in the post and quote headers and the media "From" credit are not:
/// those boxes carry `dir="auto"`. Adapted from @mrwnh's `names()` in #11.
pub(crate) fn paragraph_ltr(
    cx: &Ctx,
    text: &str,
    size: (f32, f32),
    weight: u16,
    color: Rgba,
    width: f32,
    max_lines: Option<usize>,
) -> TextBlock {
    let (size, line_height) = cx.ty(size);
    let span = Span::new(text, color, weight);
    let st = BlockStyle {
        size,
        line_height,
        weight,
        width,
        max_lines,
        wrap: Wrap::Anywhere,
        preserve: true,
        dir: Dir::Ltr,
        tab_size: 4.0,
    };
    inline::layout(&[span], &st)
}

/// Lays out the rules stream's body spans: one [`Span`] per run, with entities
/// in the brand color, exactly as the frame draws a post's own text.
pub(crate) fn body_block(
    cx: &Ctx,
    spans: &[crate::rules::BodySpan],
    size: (f32, f32),
    weight: u16,
    color: Rgba,
    brand: Rgba,
    width: f32,
    max_lines: Option<usize>,
) -> TextBlock {
    let (size, line_height) = cx.ty(size);
    let spans: Vec<Span> = spans
        .iter()
        .map(|s| {
            Span::new(
                s.text.clone(),
                if s.kind.is_entity() { brand } else { color },
                weight,
            )
            .role(s.kind.role())
        })
        .collect();
    let st = BlockStyle {
        size,
        line_height,
        weight,
        width,
        max_lines,
        wrap: Wrap::Anywhere,
        preserve: true,
        dir: Dir::Auto,
        tab_size: 4.0,
    };
    inline::layout(&spans, &st)
}

/// [`paragraph`] with tabular figures (`font-variant-numeric: tabular-nums`).
pub(crate) fn tnum_paragraph(
    cx: &Ctx,
    text: &str,
    size: (f32, f32),
    weight: u16,
    color: Rgba,
    width: f32,
    max_lines: Option<usize>,
) -> TextBlock {
    let (size, line_height) = cx.ty(size);
    let span = Span::new(text, color, weight).tnum();
    let st = BlockStyle {
        size,
        line_height,
        weight,
        width,
        max_lines,
        wrap: Wrap::Anywhere,
        preserve: true,
        dir: Dir::Auto,
        tab_size: 4.0,
    };
    inline::layout(&[span], &st)
}

/// A horizontal pill with centered text, as X's `x-button` renders one.
///
/// Returns the text block and the pill's width: the text plus `pad_x` on each
/// side, never narrower than the pill is tall.
pub(crate) fn badge(
    cx: &Ctx,
    text: &str,
    size: (f32, f32),
    weight: u16,
    color: Rgba,
    pad_x: f32,
    height: f32,
) -> (TextBlock, f32) {
    let block = paragraph(cx, text, size, weight, color, f32::MAX, Some(1));
    let w = block.width + 2.0 * pad_x;
    (block, w.max(height))
}

/// `object-fit: cover` focal point. X leaves `object-position` at its default.
pub(crate) const FOCUS: (f32, f32) = (0.5, 0.5);
