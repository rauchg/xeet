//! Media: a single image, video or GIF poster, and the image carousel.
//!
//! ## What the client actually renders (app version `89824c04`)
//!
//! A post with two or more images is **not** a grid any more: it is a
//! horizontally scrolling carousel, one card per image, at most 20 cards.
//! Every capture in `assets/vendor/corpus/` shows this (github: 2 images,
//! MrBeast: 3, NASA: 4). The layout numbers are computed by the client too, in
//! the `mediaCarouselLayout` module of `DIdg36-I.js`; [`layout_carousel`] is a
//! port of it, including its odd corners, because those corners are what
//! Chrome renders:
//!
//! * `height_ratio` is the carousel's height as a fraction of the slot width,
//!   `height_offset` an extra pixel term. X writes them as
//!   `padding-bottom: calc(68.4346% - 2.737px)`.
//! * Each cell's width is its computed item aspect ratio times that height.
//! * The first cell starts at the slot's left edge; later cells run past the
//!   right edge and are clipped, which is the carousel's "there is more" hint.
//!
//! The constants that drive it are `BASE = 0.68` (the height ratio a single
//! slide wants), `MAX = 0.8` (the widest a cell may be relative to its height),
//! `PEEK = 0.67` and the 4 px gap. Quoted posts lay their media out with the
//! "compact" variant, which uses a 0.66 fallback and a `0.68 × 0.85` base.
//!
//! ## Evidence
//!
//! | post | images | item ars | X's `padding-bottom` | computed here |
//! |---|---|---|---|---|
//! | github art0 | 2 | 0.8, 0.8 | `calc(62.5% - 2.5px)` | same |
//! | Firefox art2 | 2 | 2/3, 2/3 | `calc(75% - 3px)` | same |
//! | NASA art0 | 4 | 1.5 ×4 | `68%` | same |
//! | F1 art1 | 4 | 0.8 ×4 | `calc(74.8503% - 2.994px)` | same |
//! | MrBeast art3 | 3 | 0.875 ×3 | `calc(68.4346% - 2.737px)` | same |
//! | WHO art0 | 4 | 0.82533 ×4 | `calc(72.5528% - 2.902px)` | same |
//! | quote (levels.io) | 2 | 0.70588 ×2 | `66%` | same |

use super::{
    badge, fill, outline, push_text, rounded, shape, shift, Ctx, Item, Rect, SUBTEXT1, SUBTEXT2,
    SUBTEXT3,
};
use crate::display::{CornerShape, ImageRef};
use crate::model::{self, MediaKind, Verified};

/// A cell's `border` (1 px).
const BORDER: f32 = 1.0;
/// The carousel gap, `--spacing(1)`.
pub(crate) const GAP: f64 = 4.0;
/// `EC`: height ratio of one slide.
const BASE: f64 = 0.68;
/// `DC`: widest a cell may be relative to its own height.
const MAX_CELL: f64 = 0.8;
/// `t` in the client's peek candidates.
const PEEK: f64 = 0.67;
/// `NC`: fallback height ratio when every image has a size.
const FALLBACK: f64 = 1.24446;
/// `MC`: `BASE` multiplier in the compact (quoted) variant.
const COMPACT_BASE: f64 = 0.85;
/// `PC`: fallback height ratio in the compact variant.
const COMPACT_FALLBACK: f64 = 0.66;
/// `jC`: if at least this much of the slot would be empty, keep the base.
const SPARE: f64 = 0.33;
/// `AC`: the same threshold for the two-image branch.
const SPARE_2: f64 = 0.33;
/// The tallest a single media item may render (`maxHeight: 510`).
pub(crate) const MAX_HEIGHT: f32 = 510.0;

/// One attachment item, as the layout needs it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MediaItem {
    Image {
        index: usize,
        ar: f64,
        alt: bool,
        source: Option<Source>,
    },
    Gif {
        index: usize,
        ar: f64,
        source: Option<Source>,
    },
    Video {
        index: usize,
        ar: f64,
        seconds: Option<f32>,
        source: Option<Source>,
    },
}

/// X's "Post video" / "Post photo" attribution: the account whose post the
/// media was embedded from (`source_user_results` in the payload).
///
/// `BNn3ozGQ.js` (`video_attribution_label` / `photo_attribution_label`) reads
/// `media.source_user_results.result.user`, builds the label's href from that
/// user's `screen_name`, and **renders nothing at all when it cannot** — so a
/// media item without a source, or whose source is the post's own author, shows
/// no label. The name falls back to `@handle` when the display name is empty.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    /// The media's index, for `ImageRef::MediaSourceAvatar`.
    pub index: usize,
    pub name: String,
    pub verified: Verified,
    /// `x-avatar rounded-[10%]` (business accounts) instead of `rounded-full`.
    pub square: bool,
}

impl Source {
    /// The label from one media item's payload, or `None` when X would not draw
    /// one: no `source_author`, neither a name nor a handle to show, or a source
    /// that *is* the post's own author (then the label would be a self-reference
    /// — `@espn` re-posting `@espn`'s video shows no attribution).
    fn of(index: usize, m: &model::Media, post_handle: &str) -> Option<Source> {
        let author = m.source_author.as_ref()?;
        let name = author.name.trim();
        let handle = author.handle.trim().trim_start_matches('@');
        if name.is_empty() && handle.is_empty() {
            return None;
        }
        let own = post_handle.trim().trim_start_matches('@');
        if !own.is_empty() && !handle.is_empty() && own.eq_ignore_ascii_case(handle) {
            return None;
        }
        Some(Source {
            index,
            name: if name.is_empty() {
                format!("@{handle}")
            } else {
                name.to_string()
            },
            verified: author.verified,
            square: author.square_avatar,
        })
    }
}

impl MediaItem {
    pub(crate) fn aspect(&self) -> Option<f64> {
        match self {
            MediaItem::Image { ar, .. }
            | MediaItem::Gif { ar, .. }
            | MediaItem::Video { ar, .. } => (ar.is_finite() && *ar > 0.0).then_some(*ar),
        }
    }
    fn image(&self) -> ImageRef {
        match self {
            MediaItem::Image { index, .. }
            | MediaItem::Gif { index, .. }
            | MediaItem::Video { index, .. } => ImageRef::Media(*index),
        }
    }
    fn source(&self) -> Option<&Source> {
        match self {
            MediaItem::Image { source, .. }
            | MediaItem::Gif { source, .. }
            | MediaItem::Video { source, .. } => source.as_ref(),
        }
    }
}

/// `post_handle` is the author of the post the media belongs to: a media item
/// whose `source_author` is that same account (or has nothing to show) gets no
/// attribution badge.
pub(crate) fn items_of(media: &[model::Media], post_handle: &str) -> Vec<MediaItem> {
    media
        .iter()
        .enumerate()
        .take(20)
        .map(|(index, m)| {
            let ar = if m.width > 0 && m.height > 0 {
                m.width as f64 / m.height as f64
            } else {
                super::DEFAULT_ASPECT
            };
            let source = Source::of(index, m, post_handle);
            match m.kind {
                MediaKind::Image => MediaItem::Image {
                    index,
                    ar,
                    alt: m.alt.as_deref().is_some_and(|a| !a.trim().is_empty()),
                    source,
                },
                MediaKind::Gif => MediaItem::Gif { index, ar, source },
                MediaKind::Video => MediaItem::Video {
                    index,
                    ar,
                    seconds: m.duration,
                    source,
                },
            }
        })
        .collect()
}

/// Where the carousel landed.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaLayout {
    /// Cell height as a fraction of the slot width.
    pub height_ratio: f32,
    /// Extra pixels added to that height (always ≤ 0).
    pub height_offset: f32,
    /// Cell width for each item, as a multiple of the cell height, in double
    /// precision: X writes these as 16-digit CSS numbers (`1.1764705882352942`)
    /// and the extra digits decide which 1/64 px the cell lands on.
    pub item_aspects: Vec<f64>,
    /// True when the cells do not all fit, which is when X draws a carousel.
    pub overflows: bool,
}

impl MediaLayout {
    /// Cell height in CSS pixels for a slot `width` wide, on Chrome's 1/64 px
    /// layout grid.
    pub fn height(&self, width: f32) -> f32 {
        super::snap(self.height_ratio as f64 * width as f64 + self.height_offset as f64)
    }

    /// The width of each cell in CSS pixels.
    pub fn cell_widths(&self, width: f32) -> Vec<f32> {
        let h = self.height(width);
        self.item_aspects
            .iter()
            .map(|a| super::snap(*a * h as f64))
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
struct Ratio {
    ratio: f64,
    offset: f64,
}

fn ratio(ratio: f64, offset: f64) -> Ratio {
    Ratio { ratio, offset }
}

/// `uC`: the smaller of two layouts.
fn min_ratio(a: Ratio, b: Ratio) -> Ratio {
    if a.ratio <= b.ratio {
        a
    } else {
        b
    }
}

/// `dC`: the larger of two layouts.
fn max_ratio(a: Ratio, b: Ratio) -> Ratio {
    if a.ratio >= b.ratio {
        a
    } else {
        b
    }
}

/// `mC`: one cell's aspect ratio at a given height ratio, capped at `MAX_CELL`.
/// A missing size means the cell fills the height, i.e. a square cell.
fn cell_ratio(height: f64, ar: Option<f64>) -> f64 {
    match ar {
        None => height,
        Some(a) => (height * a).min(MAX_CELL),
    }
}

/// `hC`: the height ratio at which every cell fits the slot width exactly.
fn fit(ars: &[Option<f64>]) -> Option<Ratio> {
    let known: Option<Vec<f64>> = ars.iter().copied().collect();
    let numeric = known?;
    if numeric.len() < 2 || numeric.iter().any(|&a| a <= 0.0) {
        return None;
    }
    let gaps = GAP * (numeric.len() - 1) as f64;
    let mut rest: Vec<f64> = numeric;
    let mut dropped = 0usize;
    loop {
        let sum: f64 = rest.iter().sum();
        if sum <= 0.0 {
            return None;
        }
        let height = (1.0 - dropped as f64 * MAX_CELL) / sum;
        if height <= 0.0 {
            return None;
        }
        let keep: Vec<f64> = rest
            .iter()
            .copied()
            .filter(|&a| height * a <= MAX_CELL)
            .collect();
        if keep.len() == rest.len() {
            return Some(ratio(height, -gaps * height));
        }
        dropped += rest.len() - keep.len();
        if keep.is_empty() {
            return None;
        }
        rest = keep;
    }
}

/// `vC`: the "one slide and a peek of the next" candidates.
fn peek(ars: &[Option<f64>]) -> Option<Ratio> {
    let ar0 = ars.first().copied().flatten().unwrap_or(1.0);
    let ar1 = ars.get(1).copied().flatten().unwrap_or(1.0);
    if ar0 <= 0.0 || ar1 <= 0.0 {
        return None;
    }
    let has0 = ars.first().is_some_and(|a| a.is_some());
    let has1 = ars.get(1).is_some_and(|a| a.is_some());
    let mut candidates: Vec<Ratio> = Vec::with_capacity(3);
    let mut push = |num: f64, den: f64| {
        if den > 0.0 {
            let r = num / den;
            if r > 0.0 {
                candidates.push(ratio(r, -GAP * r));
            }
        }
    };
    // A whole first slide plus `PEEK` of the second.
    let den = ar0 + PEEK * ar1;
    let h = if den > 0.0 { 1.0 / den } else { 0.0 };
    if h > 0.0 && (!has0 || h * ar0 <= MAX_CELL) && (!has1 || h * ar1 <= MAX_CELL) {
        push(1.0, den);
    }
    // Most of the second slide.
    if has0 {
        let d = PEEK * ar1;
        let h = if d > 0.0 { 0.2 / d } else { 0.0 };
        if h > 0.0 && h * ar0 >= MAX_CELL && (!has1 || h * ar1 <= MAX_CELL) {
            push(0.2, d);
        }
    }
    // Most of the first slide.
    if has1 {
        let d = ar0;
        let h = if d > 0.0 {
            (1.0 - PEEK * MAX_CELL) / d
        } else {
            0.0
        };
        if h > 0.0 && h * ar1 >= MAX_CELL && (!has0 || h * ar0 <= MAX_CELL) {
            push(1.0 - PEEK * MAX_CELL, d);
        }
    }
    candidates.into_iter().reduce(min_ratio)
}

/// `gC`: how much of the second slide is visible, 0..=1.
fn second_visible(e: Ratio, ars: &[Option<f64>]) -> f64 {
    let n = cell_ratio(e.ratio, ars.first().copied().flatten());
    let r = cell_ratio(e.ratio, ars.get(1).copied().flatten());
    if r <= 0.0 {
        return 0.0;
    }
    (1.0 - n).max(0.0).min(r) / r
}

/// `_C`: the tallest height ratio at which the first `n` cells stay capped.
fn limited(ars: &[Option<f64>], n: usize) -> Option<Ratio> {
    let mut limit = f64::INFINITY;
    let mut any = false;
    for a in ars.iter().take(n).flatten() {
        if *a > 0.0 {
            limit = limit.min(MAX_CELL / a);
            any = true;
        }
    }
    any.then(|| ratio(limit, 0.0))
}

/// `yC`: prefer the exact fit, but never stretch past the fallback.
fn prefer_fit(e: Ratio, ars: &[Option<f64>], fallback: Ratio) -> Ratio {
    match fit(ars) {
        Some(f) if f.ratio >= e.ratio => min_ratio(f, fallback),
        _ => e,
    }
}

/// `bC`: the two-image case.
fn two(e: Ratio, ars: &[Option<f64>]) -> Ratio {
    match fit(ars) {
        Some(f) if f.ratio < e.ratio && second_visible(e, ars) >= SPARE_2 => f,
        _ => e,
    }
}

/// `xC`: three or more images.
fn many(e: Ratio, ars: &[Option<f64>], fallback: Ratio) -> Ratio {
    if 1.0 - second_visible(e, ars) >= SPARE {
        return e;
    }
    let Some(p) = peek(ars) else { return e };
    let mut best = min_ratio(p, fallback);
    if let Some(cap) = limited(ars, 2) {
        best = min_ratio(best, max_ratio(cap, e));
    }
    if best.ratio > e.ratio {
        best
    } else {
        e
    }
}

/// `SC`: the count-dependent dispatch.
fn dispatch(e: Ratio, ars: &[Option<f64>], fallback: Ratio) -> Ratio {
    match ars.len() {
        0 | 1 => e,
        2 => two(e, ars),
        _ => many(e, ars, fallback),
    }
}

/// Port of the client's `wC`: the carousel layout for a list of image aspect
/// ratios (in attachment order). `compact` is X's quoted-post variant.
pub fn layout_carousel(ars: &[Option<f64>], compact: bool) -> MediaLayout {
    let ars: Vec<Option<f64>> = ars
        .iter()
        .map(|a| a.filter(|v| v.is_finite() && *v > 0.0))
        .collect();
    let base = if compact { BASE * COMPACT_BASE } else { BASE };
    let fallback = if compact {
        ratio(COMPACT_FALLBACK, 0.0)
    } else {
        ratio(FALLBACK, 0.0)
    };
    // `uC(i, FC)` when some size is missing, `i` otherwise.
    let anchor = if ars.iter().any(|a| a.is_none()) {
        min_ratio(fallback, ratio(MAX_CELL, 0.0))
    } else {
        fallback
    };
    let start = min_ratio(ratio(base, 0.0), anchor);
    let chosen = dispatch(prefer_fit(start, &ars, anchor), &ars, anchor);
    let h = chosen.ratio;
    let item_aspects: Vec<f64> = ars.iter().map(|&a| cell_ratio(h, a) / h).collect();
    let sum: f64 = item_aspects.iter().sum();
    MediaLayout {
        height_ratio: chosen.ratio as f32,
        height_offset: chosen.offset as f32,
        overflows: chosen.ratio * sum > 1.000001,
        item_aspects,
    }
}

/// A single media item's box: the item's own aspect ratio, capped so the
/// height never exceeds 510 px (`max-width: 510px × ar` on tall media).
///
/// In a timeline cell the box is left-aligned; only the *quoted* post's media
/// wrapper carries `mx-auto`, so `centered` is true for that case only.
pub fn single_box(ar: f64, width: f32, centered: bool) -> Rect {
    let ar = if ar.is_finite() && ar > 0.0 {
        ar
    } else {
        super::DEFAULT_ASPECT
    };
    let max_w = if ar < 1.0 {
        MAX_HEIGHT as f64 * ar
    } else {
        f64::INFINITY
    };
    let w = (width as f64).min(max_w);
    // Chrome divides the box's LayoutUnit width by the ratio, which lands on a
    // whole unit (310 px of a 16/9 video is exactly 174.375); f64 can land a
    // hair under (`174.37499999999997`) and then truncation loses a unit, so the
    // noise is absorbed before snapping.
    let h = w / ar + 1e-9;
    let x = if centered {
        (width as f64 - w) * 0.5
    } else {
        0.0
    };
    Rect::new(x as f32, 0.0, super::snap(w), super::snap(h))
}

// ------------------------------------------------------------------ painting

/// Lays out the media block and returns its height.
pub(crate) fn layout(
    cx: &Ctx,
    items: &[MediaItem],
    y0: f32,
    out: &mut Vec<Item>,
    with_page_indicator: bool,
) -> f32 {
    match items.len() {
        0 => 0.0,
        1 => single(cx, &items[0], y0, out),
        _ => carousel(cx, items, y0, out, false, with_page_indicator),
    }
}

/// The tag line X shows under media that tags accounts.
///
/// ```html
/// <div class="max-w-full text-subtext2 font-normal text-secondary mt-1 w-fit">
///   <a href="…/media_tags">CAPEWEEK and 6 others</a>
/// </div>
/// ```
///
/// The label depends only on the number of tags: one tag is its name, two are
/// "A and B", more are "A and N others". All three forms are in the corpus
/// (`Google.html`: "Google DeepMind", `NASA.html`: "CAPEWEEK and 6 others" and
/// "Learn With NASA and NASA Administrator Jared Isaacman" — that last one is
/// two tags whose first name reads as a prefix, which the model cannot tell
/// apart from a name).
pub(crate) fn tag_line(cx: &Ctx, tags: &[String], y0: f32, out: &mut Vec<Item>) -> f32 {
    let Some(label) = tag_label(tags) else {
        return 0.0;
    };
    // No `dir` on the box: it takes the page's `ltr` (see `paragraph_ltr`).
    let block = super::paragraph_ltr(cx, &label, SUBTEXT2, 400, cx.p.secondary, cx.width, None);
    push_text(out, &block, 0.0, y0, cx.width);
    block.height
}

/// "A", "A and B", or "A and 6 others".
pub(crate) fn tag_label(tags: &[String]) -> Option<String> {
    let names: Vec<&str> = tags
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    match names.len() {
        0 => None,
        1 => Some(names[0].to_string()),
        2 => Some(format!("{} and {}", names[0], names[1])),
        n => Some(format!("{} and {} others", names[0], n - 1)),
    }
}

/// Media inside a quoted post.
///
/// The markup differs from the timeline's: a single image is
/// `<div class="mx-auto relative overflow-hidden" style="aspect-ratio:…;max-width:…">`
/// — no radius, no hairline border, centered — and a carousel is wrapped in
/// `<div class="flex flex-col mx-3 mb-3 [--media-overhang-end:--spacing(3)]">`,
/// so the cells are inset 12 px on each side with 12 px below them.
pub(crate) fn layout_quote(cx: &Ctx, items: &[MediaItem], y0: f32, out: &mut Vec<Item>) -> f32 {
    match items.len() {
        0 => 0.0,
        1 => {
            let item = &items[0];
            let ar = item.aspect().unwrap_or(super::DEFAULT_ASPECT);
            let b = single_box(ar, cx.width, true);
            let rect = Rect::new(b.x, y0 + b.y, b.w, b.h);
            let square = shape(rect, 0.0, CornerShape::Round);
            if matches!(item, MediaItem::Video { .. } | MediaItem::Gif { .. }) {
                fill(out, square, crate::color::Rgba::BLACK);
            }
            out.push(Item::Image {
                shape: square,
                image: item.image(),
                focus: super::FOCUS,
            });
            overlays(cx, item, rect, 0.0, out);
            rect.h
        }
        _ => {
            let inset = Ctx {
                width: cx.width - 24.0,
                // `<div class="flex flex-col mx-3 mb-3 [--media-overhang-end:--spacing(3)]">`:
                // 12 px inset minus the quote card's 1 px border.
                overhang_end: 11.0,
                ..*cx
            };
            let mut inner: Vec<Item> = Vec::new();
            let h = carousel(&inset, items, 0.0, &mut inner, true, false);
            shift(&mut inner, 12.0, y0);
            out.append(&mut inner);
            h + 12.0
        }
    }
}

/// A single media item: the item's own aspect ratio, capped at 510 px of
/// height, with `rounded-md` and the `::after` hairline.
fn single(cx: &Ctx, item: &MediaItem, y0: f32, out: &mut Vec<Item>) -> f32 {
    let ar = item.aspect().unwrap_or(super::DEFAULT_ASPECT);
    let b = single_box(ar, cx.width, false);
    let rect = Rect::new(b.x, y0 + b.y, b.w, b.h);
    let s = rounded(rect, crate::tokens::RADIUS_MD_RADIUS);
    // `overflow-hidden` on the rounded box clips everything inside it, so the
    // video's black backdrop is a *square* fill (X puts `bg-black` on the inner
    // unrounded div) drawn under the poster.
    out.push(Item::PushClip { shape: s });
    if matches!(item, MediaItem::Video { .. } | MediaItem::Gif { .. }) {
        fill(
            out,
            shape(rect, 0.0, CornerShape::Round),
            crate::color::Rgba::BLACK,
        );
    }
    out.push(Item::Image {
        shape: s,
        image: item.image(),
        focus: super::FOCUS,
    });
    out.push(Item::PopClip);
    outline(out, s, cx.p.border);
    overlays(cx, item, rect, 0.0, out);
    rect.h
}

/// Carousel page indicator (`Pcxk6Kf2.js`, `Wg` + `Gg`): at most 7 visible
/// slots (`t_ = 7`), active pill `22 × 4` at opacity `1.0`, inactive pills
/// `12 × 4` at opacity `0.35`, base spacing `5` px, with edge slots at the
/// window boundary scaled by `0.5` (`6 × 4` pill) and slots outside the window
/// collapsed to `0` px.
const IND_ACTIVE_W: f64 = 22.0;
const IND_INACTIVE_W: f64 = 12.0;
const IND_SPACING: f64 = 5.0;
const IND_INACTIVE_ALPHA: f64 = 0.35;
const IND_WINDOW: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq)]
struct IndicatorBar {
    x: f32,
    width: f32,
    opacity: f32,
}

/// Port of `Wg` + `Gg` (`Pcxk6Kf2.js`): lays out the `count` indicator bars
/// for scroll `position` in `0.0..=(count - 1)`.
fn layout_indicator_bars(position: f64, count: usize) -> (Vec<IndicatorBar>, f32) {
    let max_pos = count.saturating_sub(1) as f64;
    let pos = position.clamp(0.0, max_pos);
    let max_start = count.saturating_sub(IND_WINDOW) as f64;
    let start = (pos - (IND_WINDOW / 2) as f64).clamp(0.0, max_start);
    let left_fade = start.min(1.0);
    let right_fade = (max_start - start).min(1.0);

    let mut bars = Vec::with_capacity(count);
    let mut x = 0.0_f64;
    for i in 0..count {
        let idx = i as f64;
        let active = 1.0 - (pos - idx).abs().min(1.0);
        let rel = idx - start;
        let rel_end = (IND_WINDOW.saturating_sub(1) as f64) - rel;
        let left_scale = 1.0 - left_fade * (1.0 - ((rel + 1.0) * 0.5).clamp(0.0, 1.0));
        let right_scale = 1.0 - right_fade * (1.0 - ((rel_end + 1.0) * 0.5).clamp(0.0, 1.0));
        let scale = left_scale.min(right_scale);
        let width = (IND_INACTIVE_W + (IND_ACTIVE_W - IND_INACTIVE_W) * active) * scale;
        let spacing = IND_SPACING * scale;
        let opacity = IND_INACTIVE_ALPHA + (1.0 - IND_INACTIVE_ALPHA) * active;
        bars.push(IndicatorBar {
            x: x as f32,
            width: width as f32,
            opacity: opacity as f32,
        });
        x += width + spacing;
    }
    (bars, x as f32)
}

fn carousel(
    cx: &Ctx,
    items: &[MediaItem],
    y0: f32,
    out: &mut Vec<Item>,
    compact: bool,
    with_page_indicator: bool,
) -> f32 {
    let ars: Vec<Option<f64>> = items.iter().map(|i| i.aspect()).collect();
    let l = layout_carousel(&ars, compact);
    let h = l.height(cx.width);
    if !(h > 0.0) {
        return 0.0;
    }
    // `B0WW91ux.js`: `-me-(--x-carousel-bleed-end) … pe-(--x-carousel-bleed-end)`
    // extends an overflowing carousel's clip box across `--media-overhang-end`
    // to the right edge of the post.
    let clip_w = cx.width + if l.overflows { cx.overhang_end } else { 0.0 };

    // `Pcxk6Kf2.js` (`qg` + `Wg` + `Gg`): when `withPageIndicator &&
    // layout.overflowsWidth && variant === "timeline"` (`!compact`, i.e. a main
    // post with empty body), X renders `<div class="pointer-events-none relative
    // -mt-2 mb-3 h-1 self-start">` above the slides using a 7-slot window:
    // for `count <= 7`, all pills render full width (`22px` active at opacity
    // `1.0`, `12px` inactive at opacity `0.35`, `5px` spacing); for `count > 7`
    // at slide 0, slot 6 scales by `0.5` (`6px` wide at `x = 112px`, total
    // container width `120.5px`) and slots `7..count` collapse to `0px`.
    let show_indicator = with_page_indicator && l.overflows && !compact;
    let slide_y = if show_indicator {
        let ind_y = y0 - 8.0;
        let base = cx.p.secondary;
        let (bars, ind_w) = layout_indicator_bars(0.0, items.len());
        out.push(Item::PushClip {
            shape: shape(
                Rect::new(0.0, ind_y, ind_w.min(clip_w), 4.0),
                0.0,
                CornerShape::Round,
            ),
        });
        for bar in bars {
            if bar.width <= 0.0 || bar.x >= clip_w {
                continue;
            }
            let color = crate::color::Rgba {
                a: ((f32::from(base.a) * bar.opacity).round()) as u8,
                ..base
            };
            let r = bar.width.min(4.0) * 0.5;
            fill(
                out,
                shape(
                    Rect::new(bar.x, ind_y, bar.width, 4.0),
                    r,
                    CornerShape::Round,
                ),
                color,
            );
        }
        out.push(Item::PopClip);
        y0 + 8.0
    } else {
        y0
    };

    let widths = l.cell_widths(cx.width);
    out.push(Item::PushClip {
        shape: shape(Rect::new(0.0, slide_y, clip_w, h), 0.0, CornerShape::Round),
    });
    let mut x = 0.0;
    for (i, item) in items.iter().enumerate() {
        let w = widths.get(i).copied().unwrap_or(h);
        // The cell's own 1 px border sits outside the image.
        let inner = Rect::new(
            x + BORDER,
            slide_y + BORDER,
            w - 2.0 * BORDER,
            h - 2.0 * BORDER,
        );
        clipped_image(out, inner, item.image(), cx.cell_image_radius(w, h));
        x += w + GAP as f32;
    }
    // Hairline borders sit over the images, one per cell.
    let mut x = 0.0;
    for (i, _) in items.iter().enumerate() {
        let w = widths.get(i).copied().unwrap_or(h);
        outline(
            out,
            rounded(Rect::new(x, slide_y, w, h), crate::tokens::RADIUS_SM_RADIUS),
            cx.p.border,
        );
        x += w + GAP as f32;
    }
    let mut x = 0.0;
    for (i, item) in items.iter().enumerate() {
        let w = widths.get(i).copied().unwrap_or(h);
        overlays(cx, item, Rect::new(x, slide_y, w, h), 1.0, out);
        x += w + GAP as f32;
    }
    out.push(Item::PopClip);
    if show_indicator {
        h + 8.0
    } else {
        h
    }
}

/// Pushes the image item clipped to a rounded box. The painter resolves
/// [`ImageRef::Media`] and draws a placeholder when there are no bytes.
pub(crate) fn clipped_image(out: &mut Vec<Item>, rect: Rect, image: ImageRef, radius: f32) {
    let s = shape(rect, radius, CornerShape::Squircle);
    out.push(Item::PushClip { shape: s });
    out.push(Item::Image {
        shape: s,
        image,
        focus: super::FOCUS,
    });
    out.push(Item::PopClip);
}

/// ALT, duration, GIF and play overlays for one media box.
///
/// `inset` is how far the box's content edge sits inside `rect`: the carousel's
/// cells have a real 1 px border, a single video does not (its border is the
/// `::after` hairline), and X insets the badges from the content edge.
fn overlays(cx: &Ctx, item: &MediaItem, rect: Rect, inset: f32, out: &mut Vec<Item>) {
    match item {
        MediaItem::Image { alt, .. } => {
            // The bottom-start row is `flex items-center gap-0.5`, so the ALT
            // badge and the attribution sit side by side, 2 px apart.
            let mut x = rect.x + inset + 12.0;
            let mut w = 0.0;
            if *alt {
                let h = 24.0;
                let (block, bw) = badge(cx, "ALT", SUBTEXT1, 500, cx.p.on_color, 12.0, h);
                let bw = bw - block.width + snapped(block.width);
                let y = rect.y + rect.h - inset - 12.0 - h;
                fill(
                    out,
                    shape(Rect::new(x, y, bw, h), h * 0.5, CornerShape::Round),
                    cx.p.black_40,
                );
                push_text(
                    out,
                    &block,
                    x + 12.0,
                    y + (h - block.height) * 0.5,
                    bw - 24.0,
                );
                w = bw;
            }
            if let Some(src) = item.source() {
                if w > 0.0 {
                    // The photo row is `flex items-center gap-1`.
                    x += w + GAP_1;
                }
                let bottom = rect.y + rect.h - inset - 12.0;
                // The photo row is `inset-s-3 … max-w-[calc(100%-24px)]`: 12 px
                // in on both sides.
                photo_attribution(cx, src, x, bottom, rect.x + inset + rect.w - 12.0, out);
            }
        }
        MediaItem::Video { seconds, .. } => {
            play_button(cx, rect, out);
            bottom_badge(cx, rect, &duration_label(*seconds), item.source(), out);
        }
        MediaItem::Gif { .. } => {
            bottom_badge(cx, rect, "GIF", item.source(), out);
        }
    }
}

/// [`super::badge`] with tabular figures.
fn badge_tnum(
    cx: &Ctx,
    text: &str,
    size: (f32, f32),
    weight: u16,
    color: crate::color::Rgba,
    pad_x: f32,
    height: f32,
) -> (crate::inline::TextBlock, f32) {
    let block = super::tnum_paragraph(cx, text, size, weight, color, f32::MAX, Some(1));
    let w = block.width + 2.0 * pad_x;
    (block, w.max(height))
}

/// The badge's label, from X's own formatter.
///
/// `assets/vendor/upstream/js/BEV1NdT5.js` (sha256 `b616c69066203ef22484…`)
/// registers the i18n module `unit-duration-to-video-timestamp-FqoFhlvP`, whose
/// export `n` is the function the video badge calls — the badge component in
/// the same chunk computes `r = duration >= currentTime ? duration - currentTime
/// : 0` and formats it with `(0, d.n)(r, "seconds")`:
///
/// ```js
/// function r(e, t = `milliseconds`) {
///   let n = [], r = Math.floor(e / (t === `milliseconds` ? 1e3 : 1)),
///       i = Math.floor(r / 3600), a = Math.floor(r % 3600 / 60), o = r % 60;
///   return i && n.push(i.toString().padStart(2, `0`)),
///          n.push(a.toString().padStart(2, `0`)),
///          n.push(o.toString().padStart(2, `0`)),
///          n.join(`:`)
/// }
/// ```
///
/// So: whole seconds **truncated** (`Math.floor`, never rounded), hours only
/// once they are non-zero, and *every* part padded to two digits — minutes
/// included. The captured quote in `NBA.html` agrees: a quoted video with
/// `duration_millis: 11679` carries the server-rendered badge `00:11`, which is
/// the floored 11 s, not the rounded 12 s. The full-size timeline badge shows
/// `00:00` in every capture (57 posts) because the server renders before the
/// player has metadata; that is this same function at zero, which is also what
/// the badge shows when a post has no duration at all.
fn duration_label(seconds: Option<f32>) -> String {
    let total = seconds
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(0.0)
        .floor() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Centered play control: 48 px `bg-black-40` circle with a 24 px triangle.
fn play_button(cx: &Ctx, rect: Rect, out: &mut Vec<Item>) {
    let size = 48.0;
    let x = rect.x + (rect.w - size) * 0.5;
    let y = rect.y + (rect.h - size) * 0.5;
    fill(
        out,
        shape(Rect::new(x, y, size, size), size * 0.5, CornerShape::Round),
        cx.p.black_40,
    );
    let icon = 24.0;
    out.push(Item::Icon {
        name: "icon-player-play".to_string(),
        rect: Rect::new(x + (size - icon) * 0.5, y + (size - icon) * 0.5, icon, icon),
        color: cx.p.on_color,
    });
}

/// A quoted post's video shown in the compact form: a square thumbnail.
///
/// ```html
/// <div class="w-24 shrink-0 relative overflow-hidden aspect-square
///             after:… after:border after:border-normal rounded-md">
///   <div class="relative overflow-hidden h-full w-full object-cover" style="aspect-ratio:0.8">
///     <img class="h-full w-full object-cover" src="…amplify_video_thumb…">
///     <div class="absolute inset-s-1 bottom-1">
///       <div class="inline-flex h-5 shrink-0 items-center gap-1 rounded-sm px-2
///                   text-subtext3 font-medium bg-black-80 text-on-color">00:11</div>
///     </div>
///   </div>
/// </div>
/// ```
///
/// The poster is cropped to the square (`object-cover`), the frame is
/// `rounded-md` with a 1 px `border-normal` ring, and the duration is the same
/// pill as the full-size overlay but inset 4 px (`inset-1`) and set in
/// `text-subtext3` (11/12) rather than `text-subtext2`.
pub(crate) fn compact_thumb(cx: &Ctx, item: &MediaItem, rect: Rect, out: &mut Vec<Item>) {
    let s = rounded(rect, crate::tokens::RADIUS_MD_RADIUS);
    out.push(Item::PushClip { shape: s });
    out.push(Item::Image {
        shape: s,
        image: item.image(),
        focus: super::FOCUS,
    });
    if let MediaItem::Video { seconds, .. } = item {
        let h = 20.0;
        // No `tabular-nums` here (unlike the full-size overlay's player,
        // which carries the class on its container): the compact badge is set
        // in proportional figures, 3 px narrower for "00:11".
        let block = super::paragraph(
            cx,
            &duration_label(*seconds),
            SUBTEXT3,
            500,
            cx.p.on_color,
            f32::MAX,
            Some(1),
        );
        let w = (block.width + 16.0).max(h);
        let x = rect.x + 4.0;
        let y = rect.y + rect.h - 4.0 - h;
        fill(
            out,
            rounded(Rect::new(x, y, w, h), crate::tokens::RADIUS_SM_RADIUS),
            cx.p.black_80,
        );
        push_text(out, &block, x + 8.0, y + (h - block.height) * 0.5, w - 16.0);
    }
    out.push(Item::PopClip);
    outline(out, s, cx.p.border);
}

/// Bottom-start pill: 20 px tall, `rounded-sm`, `px-2`, subtext2 at weight 500.
///
/// The video container carries `tabular-nums`, so the duration is shaped with
/// `tnum`: "00:00" is 35.97 px wide with the feature and 38.9 px without, and
/// the captured badge is the narrow one.
///
/// With a source (`ESPN.html:2120005259485425936`, verbatim):
///
/// ```html
/// <div class="inline-flex h-5 items-center gap-1 rounded-sm px-2 font-medium
///             [&_svg]:size-[1em] bg-black-80 text-on-color text-subtext2 min-w-0 shrink">
///   <span aria-hidden="true">00:00</span>
///   <span class="inline-flex min-w-0 items-center gap-1 align-middle ms-2">…</span>
/// </div>
/// ```
///
/// The pill's `gap-1` (4) and the attribution span's `ms-2` (8) add up to
/// **12 px** between the clock and the mini avatar. The pill is
/// `shrink`/`min-w-0` in a shrink-to-fit `absolute start-3` row, so the name
/// clamps into what is left of `rect.w - 24.0` (12 px of inset each side).
fn bottom_badge(cx: &Ctx, rect: Rect, text: &str, source: Option<&Source>, out: &mut Vec<Item>) {
    let h = 20.0;
    let (clock, clock_w) = badge_tnum(cx, text, SUBTEXT2, 500, cx.p.on_color, 8.0, h);
    let clock_w = clock_w - clock.width + snapped(clock.width);
    // The name is laid out first (clamped to what the pill may grow to), then
    // the pill is as wide as its content: `px-2` + clock + 12 + attribution +
    // `px-2`, where the clock's own box already carries both paddings.
    // The row is `absolute start-3 bottom-3` with no width, so it is
    // shrink-to-fit against the media box's width *minus its own start inset*
    // and may reach the box's right edge: the pill tops out at `rect.w - 12`.
    let attr = source.map(|src| {
        let budget = (rect.w - 12.0 - clock_w - GAP_EXTRA).max(1.0);
        Attr::of(cx, src, budget)
    });
    // When the name does not fit, the pill is a `shrink`/`min-w-0` flex item: it
    // gives up exactly the overflow, so its width is the whole available width
    // (`rect.w - 12`) rather than the width of the clipped text.
    let w = match attr.as_ref() {
        Some(a) if a.clamped => rect.w - 12.0,
        Some(a) => clock_w + GAP_EXTRA + a.width,
        None => clock_w,
    };
    let x = rect.x + 12.0;
    let y = rect.y + rect.h - 12.0 - h;
    fill(
        out,
        rounded(Rect::new(x, y, w, h), crate::tokens::RADIUS_SM_RADIUS),
        cx.p.black_80,
    );
    push_text(
        out,
        &clock,
        x + 8.0,
        y + (h - clock.height) * 0.5,
        clock_w - 16.0,
    );
    if let (Some(src), Some(attr)) = (source, attr.as_ref()) {
        // `items-center` in a 20 px pill: the 16 px line box sits at y + 2. The
        // clock's own box includes `px-2`, so the attribution starts one `px-2`
        // before its right edge: 4 (`gap-1`) + 8 (`ms-2`) after the text.
        attr.draw(cx, src, x + clock_w + GAP_EXTRA - 8.0, y + 2.0, out);
    }
}

/// The photo form of the attribution badge (`BNn3ozGQ.js`, component `ie`):
///
/// ```html
/// <span class="text-subtext2 inline-flex max-w-full min-w-0 items-center
///              rounded-[6px] bg-black/70 py-1 ps-1 pe-2">…</span>
/// ```
///
/// `py-1` (4 px) around a 16 px line makes it 24 px tall; it shares the media's
/// bottom-start row (12 px inset, `gap-0.5` after an ALT badge). `right` is the
/// furthest x the badge may reach.
fn photo_attribution(cx: &Ctx, src: &Source, x: f32, bottom: f32, right: f32, out: &mut Vec<Item>) {
    let h = 24.0;
    let y = bottom - h;
    let budget = (right - x - 4.0 - AVATAR - GAP_1 - 8.0).max(1.0);
    let attr = Attr::of(cx, src, budget);
    // `inline-flex max-w-full` inside the `max-w-[calc(100%-24px)]` row: an
    // overflowing name leaves the badge at the row's full width.
    let w = if attr.clamped {
        right - x
    } else {
        4.0 + attr.width + 8.0
    };
    fill(
        out,
        shape(Rect::new(x, y, w, h), 6.0, CornerShape::Round),
        cx.p.black_70,
    );
    attr.draw(cx, src, x + 4.0, y + 4.0, out);
}

/// A text advance as a box width: Chrome's layout sizes each run in layout
/// units, so the shaped advance is snapped *up* to 1/64 before it is added to
/// padding or to a neighbour. `00:24` at 13/500 sums to 35.958 and Chrome's box
/// is 35.9688; `NFL` at 13/400 sums to 23.335 and Chrome's is 23.34375.
fn snapped(w: f32) -> f32 {
    crate::frame::header::ceil64(w)
}

/// `gap-1`.
const GAP_1: f32 = 4.0;
/// The pill's `gap-1` (4) plus the attribution span's `ms-2` (8).
const GAP_EXTRA: f32 = 12.0;
/// `size-[13px]`: the mini avatar and the `size-[1em]` verified icon at 13 px.
const AVATAR: f32 = 13.0;

/// A source label laid out: the name (clamped to its budget), and what follows
/// it.
struct Attr {
    name: crate::inline::TextBlock,
    /// The name's box width: the shaped advance snapped up to 1/64, which is
    /// how Chrome sizes a text run in layout units.
    name_w: f32,
    verified: Option<&'static str>,
    /// Avatar + 4 + name + (4 + 13 when verified).
    width: f32,
    /// The name is `line-clamp-1 break-all`: it overflows its budget and is
    /// clipped, which is what makes the badge take the full available width.
    clamped: bool,
}

impl Attr {
    fn of(cx: &Ctx, src: &Source, budget: f32) -> Attr {
        let icon = match src.verified {
            Verified::None => None,
            Verified::Blue => Some("icon-verified-premium-color"),
            Verified::Business => Some("icon-verified-business-color"),
            Verified::Government => Some("icon-verified-government-color"),
        };
        let room =
            (budget - AVATAR - GAP_1 - if icon.is_some() { GAP_1 + AVATAR } else { 0.0 }).max(1.0);
        let natural = super::paragraph(
            cx,
            &src.name,
            SUBTEXT2,
            400,
            cx.p.on_color,
            f32::MAX,
            Some(1),
        );
        let clamped = natural.width > room + 0.001;
        let name = if clamped {
            super::paragraph(cx, &src.name, SUBTEXT2, 400, cx.p.on_color, room, Some(1))
        } else {
            natural
        };
        let name_w = snapped(name.width);
        let width = AVATAR + GAP_1 + name_w + if icon.is_some() { GAP_1 + AVATAR } else { 0.0 };
        Attr {
            name,
            name_w,
            verified: icon,
            width,
            clamped,
        }
    }

    /// Draws the mini avatar at `x`, the name after it and the verified icon
    /// last; `top` is the top of the 16 px line box (`items-center` centres the
    /// 13 px boxes in it).
    fn draw(&self, cx: &Ctx, src: &Source, x: f32, top: f32, out: &mut Vec<Item>) {
        let icon_top = top + (self.name.height - AVATAR) * 0.5;
        // `x-avatar rounded-[10%]` (1.3 px on a 13 px box) for a business
        // account, `rounded-full` otherwise; both are round corners.
        let radius = crate::tokens::Radius {
            value: if src.square {
                AVATAR * 0.1
            } else {
                AVATAR * 0.5
            },
            round: if src.square {
                AVATAR * 0.1
            } else {
                AVATAR * 0.5
            },
            corner: CornerShape::Round,
        };
        out.push(Item::Image {
            shape: rounded(Rect::new(x, icon_top, AVATAR, AVATAR), radius),
            image: ImageRef::MediaSourceAvatar(src.index),
            focus: super::FOCUS,
        });
        let name_x = x + AVATAR + GAP_1;
        push_text(out, &self.name, name_x, top, self.name_w);
        if let Some(icon) = self.verified {
            let vx = name_x + self.name_w + GAP_1;
            out.push(Item::Icon {
                name: icon.to_string(),
                rect: Rect::new(vx, icon_top, AVATAR, AVATAR),
                color: cx.p.primary,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::duration_label;

    /// Each case is the client's own formatter
    /// (`unit-duration-to-video-timestamp-FqoFhlvP`, `BEV1NdT5.js`): truncate
    /// to whole seconds, hours only when non-zero, every part `padStart(2,"0")`.
    #[test]
    fn duration_label_matches_the_client_formatter() {
        // 86_708 ms, the NASA video: floor(86.708) = 86 → "01:26" (rounded would
        // be "01:27"; minutes are two digits).
        assert_eq!(duration_label(Some(86.708)), "01:26");
        // `NBA.html`'s quoted video, `duration_millis: 11679`, rendered by the
        // server as exactly this.
        assert_eq!(duration_label(Some(11.679)), "00:11");
        // The badge before the player has metadata, and for a post whose payload
        // carries no duration: the same formatter at zero.
        assert_eq!(duration_label(Some(0.0)), "00:00");
        assert_eq!(duration_label(None), "00:00");
        // Sub-second and boundary values truncate.
        assert_eq!(duration_label(Some(0.9)), "00:00");
        assert_eq!(duration_label(Some(59.99)), "00:59");
        assert_eq!(duration_label(Some(60.0)), "01:00");
        assert_eq!(duration_label(Some(3599.999)), "59:59");
        // Hours appear at 3600 s and are padded like the rest.
        assert_eq!(duration_label(Some(3600.0)), "01:00:00");
        assert_eq!(duration_label(Some(3723.4)), "01:02:03");
        assert_eq!(duration_label(Some(36000.0)), "10:00:00");
        // `padStart` never truncates: a 100-hour video keeps three digits.
        assert_eq!(duration_label(Some(360000.0)), "100:00:00");
        // Nonsense values are the zero state, not a panic or a NaN label.
        assert_eq!(duration_label(Some(-5.0)), "00:00");
        assert_eq!(duration_label(Some(f32::NAN)), "00:00");
    }
}

#[cfg(test)]
mod attribution {
    use super::*;
    use crate::model::{Author, Media, Verified};

    fn media(source: Option<(&str, &str, Verified, bool)>) -> Media {
        Media {
            kind: MediaKind::Video,
            width: 1280,
            height: 720,
            source_author: source.map(|(name, handle, verified, square_avatar)| Author {
                name: name.to_string(),
                handle: handle.to_string(),
                verified,
                square_avatar,
                ..Author::default()
            }),
            ..Media::default()
        }
    }

    /// X draws the badge only when the media came from *someone else*: the
    /// label's href is built from `source_user_results`' `screen_name`
    /// (`BNn3ozGQ.js`, `W`), and a post re-posting its own media has nothing to
    /// attribute. Handles compare case-insensitively with any `@` stripped.
    #[test]
    fn the_posts_own_author_is_not_attributed() {
        for own in ["espn", "ESPN", " @espn "] {
            let m = media(Some(("ESPN", "espn", Verified::Blue, true)));
            assert!(Source::of(0, &m, own).is_none(), "own handle {own:?}");
        }
        // …and a different account is.
        let nfl = media(Some(("NFL", "NFL", Verified::Business, true)));
        let src = Source::of(0, &nfl, "espn").expect("a source");
        assert_eq!(src.name, "NFL");
        assert_eq!(src.verified, Verified::Business);
        assert!(src.square, "the business avatar is `rounded-[10%]`");
        // The media's index, for `ImageRef::MediaSourceAvatar`.
        assert_eq!(Source::of(2, &nfl, "espn").unwrap().index, 2);
    }

    /// No `source_author` at all, or one with neither a name nor a handle: X has
    /// no href to build, so `W` returns its fallback (nothing).
    #[test]
    fn a_source_with_nothing_to_show_is_dropped() {
        assert!(Source::of(0, &media(None), "espn").is_none());
        assert!(Source::of(0, &media(Some(("", "", Verified::Blue, false))), "espn").is_none());
        assert!(Source::of(0, &media(Some(("   ", " ", Verified::Blue, false))), "espn").is_none());
    }

    /// With an empty display name the label shows the handle (`@handle`), and a
    /// post with no handle of its own never matches the source's.
    #[test]
    fn the_label_falls_back_to_the_handle() {
        let m = media(Some(("", "@NFL", Verified::None, false)));
        let src = Source::of(0, &m, "espn").expect("a source");
        assert_eq!(src.name, "@NFL");
        assert_eq!(src.verified, Verified::None);
        assert!(!src.square);
        assert!(Source::of(0, &m, "").is_some(), "no own handle to compare");
    }

    #[test]
    fn overflowing_textless_carousel_draws_page_indicators_and_bleeds() {
        use crate::cards::{layout_with_body, AttachmentKind};
        use crate::display::Item;
        use crate::model::{Media, MediaKind, Post, RenderOptions, Surface};

        let post = Post {
            text: String::new(),
            media: (0..20)
                .map(|_| Media {
                    kind: MediaKind::Image,
                    width: 2048,
                    height: 1356,
                    ..Media::default()
                })
                .collect(),
            ..Post::default()
        };
        let opts = RenderOptions {
            surface: Surface::WebTimeline,
            width: 390.0,
            ..RenderOptions::default()
        };
        let with_ind = layout_with_body(&post, &opts, &AttachmentKind::Media, 310.0, true);
        let without_ind = layout_with_body(&post, &opts, &AttachmentKind::Media, 310.0, false);
        assert!(
            (with_ind.height - without_ind.height - 8.0).abs() < 0.01,
            "indicator bar adds -8 + 4 + 12 = +8 px: {} vs {}",
            with_ind.height,
            without_ind.height
        );
        // 20-image carousel uses X's 7-slot window: active 22×4 pill at x=0,
        // five 12×4 muted pills at x=27..95, one half-scaled 6×4 trailing pill at x=112,
        // and slots 7..20 collapsed to 0 px (total indicator width 120.5 px).
        let pills: Vec<_> = with_ind
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Fill { shape, .. } if (shape.rect.h - 4.0).abs() < 0.01 => Some(shape.rect),
                _ => None,
            })
            .collect();
        assert_eq!(
            pills.len(),
            7,
            "20-image carousel renders only the 7-slot indicator window"
        );
        assert_eq!((pills[0].x, pills[0].y, pills[0].w), (0.0, -8.0, 22.0));
        assert_eq!((pills[1].x, pills[1].y, pills[1].w), (27.0, -8.0, 12.0));
        assert_eq!((pills[5].x, pills[5].y, pills[5].w), (95.0, -8.0, 12.0));
        assert_eq!((pills[6].x, pills[6].y, pills[6].w), (112.0, -8.0, 6.0));

        let (bars_20, total_20) = layout_indicator_bars(0.0, 20);
        assert_eq!(total_20, 120.5);
        assert!(bars_20[7..].iter().all(|b| b.width == 0.0));

        // Up to 7 items: all 7 slots render full width (no half-scaled trailing pill).
        let (bars_7, total_7) = layout_indicator_bars(0.0, 7);
        assert_eq!(total_7, 129.0);
        assert_eq!(bars_7[0].width, 22.0);
        assert!(bars_7[1..7].iter().all(|b| b.width == 12.0));

        // Mid-scroll and end-of-scroll windowing match `Wg` + `Gg` (`Pcxk6Kf2.js`).
        let (bars_mid, _) = layout_indicator_bars(5.0, 20);
        assert_eq!(
            bars_mid[2].width, 6.0,
            "leading edge of shifted window scales by 0.5"
        );
        assert_eq!(bars_mid[5].width, 22.0, "active slot 5 is 22 px");
        assert_eq!(
            bars_mid[8].width, 6.0,
            "trailing edge of shifted window scales by 0.5"
        );
        let (bars_end, total_end) = layout_indicator_bars(19.0, 20);
        assert_eq!(total_end, 120.5);
        assert_eq!(
            bars_end[13].width, 6.0,
            "leading edge at end of carousel scales by 0.5"
        );
        assert_eq!(bars_end[19].width, 22.0, "last slot active at 22 px");

        // Non-overflowing 2-image 3:4 portrait carousel draws no indicator bar even with empty text.
        let two_portrait = Post {
            text: String::new(),
            media: vec![
                Media {
                    kind: MediaKind::Image,
                    width: 1536,
                    height: 2048,
                    ..Media::default()
                },
                Media {
                    kind: MediaKind::Image,
                    width: 1536,
                    height: 2048,
                    ..Media::default()
                },
            ],
            ..Post::default()
        };
        let two_block = layout_with_body(&two_portrait, &opts, &AttachmentKind::Media, 310.0, true);
        let two_pills = two_block
            .items
            .iter()
            .filter(|item| matches!(item, Item::Fill { shape, .. } if (shape.rect.h - 4.0).abs() < 0.01))
            .count();
        assert_eq!(
            two_pills, 0,
            "non-overflowing 2-image carousel has no indicator bar"
        );
    }

    /// The same rule as seen from `items_of`, which is what the layout uses: the
    /// suppression has to happen before an item exists.
    #[test]
    fn items_carry_the_source_only_when_it_is_drawn() {
        let nfl = media(Some(("NFL", "NFL", Verified::Business, true)));
        let own = media(Some(("ESPN", "espn", Verified::Blue, false)));
        let items = items_of(&[nfl, own], "espn");
        assert!(
            items[0].source().is_some(),
            "NFL's video, re-posted by ESPN"
        );
        assert!(items[1].source().is_none(), "ESPN's own video");
    }
}
