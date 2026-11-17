//! Link cards: `summary_large_image` (an image with the title pill over it)
//! and `summary` (a square thumbnail beside the text).
//!
//! These are the two branches of X's card component (`HlKpYMvh.js`,
//! `variant: 'image-overlay' | 'thumbnail-detail'`), class for class:
//!
//! ```html
//! <!-- summary_large_image -->
//! <div class="flex flex-col gap-1">
//!   <a class="border-normal bg-secondary relative block overflow-hidden rounded-md border">
//!     <div style="aspect-ratio:1.91"><img class="h-full w-full object-cover"></div>
//!     <div class="absolute inset-s-3 inset-e-3 bottom-3">
//!       <div class="inline-flex h-5 max-w-full items-center rounded-xs bg-black/75 px-2">
//!         <div class="text-subtext2 line-clamp-1 text-ellipsis text-white">title</div>
//!       </div>
//!     </div>
//!   </a>
//!   <a class="text-secondary block w-fit">
//!     <div class="text-subtext2 line-clamp-1">From nytimes.com</div>
//!   </a>
//! </div>
//!
//! <!-- summary -->
//! <a class="border-normal bg-secondary flex overflow-hidden rounded-md border">
//!   <div class="border-normal relative aspect-square w-[90px] shrink-0 border-e sm:w-[130px]">
//!     <img class="h-full">                       <!-- or the bg-secondary fallback -->
//!   </div>
//!   <div class="flex min-w-0 flex-1 flex-col justify-center gap-0.5 p-3">
//!     <div class="text-subtext2 line-clamp-1 text-secondary">domain</div>
//!     <div class="text-body line-clamp-2 sm:line-clamp-1">title</div>
//!     <div class="text-subtext2 line-clamp-1 text-secondary max-sm:hidden sm:line-clamp-2">description</div>
//!   </div>
//! </a>
//! ```
//!
//! Two consequences of the markup that are easy to get wrong, both measured
//! against Chrome in `tools/oracle/cards-parity.mjs`:
//!
//! * The card's 1 px border sits *outside* the image, so the large card's
//!   height is `(w - 2) / 1.91 + 2`, not `w / 1.91`. The text pill and the
//!   thumbnail are inset by that same border.
//! * The title is plain `line-clamp-1` + `text-ellipsis` with no
//!   `wrap-anywhere`, so it breaks at word boundaries and the ellipsis lands
//!   where Chrome puts it.
//!
//! The `sm:` classes are viewport breakpoints at **512 px** (X overrides
//! Tailwind's 640 px default with `@media (width>=512px)`), so a narrower
//! preview gets the mobile card: a 90 px thumbnail, two title lines and no
//! description.
//!
//! ## Unverified
//!
//! `player` and `app` cards come from other modules in the client. Until there
//! is a capture of each they render as the closest verified shape: `player` as
//! a large image card with a play control over the image, `app` as a thumbnail
//! card.

use super::{
    fill, outline, paragraph_wrapped, push_text, rounded, shape, Ctx, Item, Rect, BODY, SUBTEXT2,
};
use crate::display::{CornerShape, ImageRef};
use crate::inline::Wrap;
use crate::model::{ImageMeta, LinkMeta};

/// The large card image's fixed aspect ratio (`aspectRatio: 1.91`).
const IMAGE_ASPECT: f32 = 1.91;
/// A player card's image, which X shows at 16/9.
const PLAYER_ASPECT: f32 = 16.0 / 9.0;
/// `sm:w-[130px]`.
const THUMB_DESKTOP: f32 = 130.0;
/// `w-[90px]`.
const THUMB_MOBILE: f32 = 90.0;
/// The viewport width at which X's `sm:` variant starts. The stylesheet sets
/// it with `@media (width>=512px)`, not Tailwind's 640 px default.
const SM_BREAKPOINT: f32 = 512.0;
/// The card's own `border` (1 px).
const BORDER: f32 = 1.0;
/// `h-5` on the title pill.
const PILL_H: f32 = 20.0;
/// `px-2` on the title pill.
const PILL_PAD: f32 = 8.0;

/// Paints the card X would show for `meta` and returns the block's height.
pub(crate) fn layout(
    cx: &Ctx,
    meta: &LinkMeta,
    card_type: &str,
    url: &str,
    y0: f32,
    out: &mut Vec<Item>,
) -> f32 {
    let image = meta.image.as_ref().filter(|i| has_image(i));
    match card_type {
        "summary_large_image" | "player" if image.is_some() => {
            image_card(cx, meta, image, url, card_type == "player", y0, out)
        }
        _ => thumbnail_card(cx, meta, image, url, y0, out),
    }
}

/// True when the card metadata describes a drawable image.
fn has_image(i: &ImageMeta) -> bool {
    i.width > 0 && i.height > 0
}

fn domain_of(meta: &LinkMeta, url: &str) -> String {
    if let Some(d) = meta
        .domain
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        return d.strip_prefix("www.").unwrap_or(d).to_string();
    }
    // `https://www.host/path` → `host`.
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// `summary_large_image`: a full-width image with the title pill over it, and
/// the `From …` line 4 px underneath.
fn image_card(
    cx: &Ctx,
    meta: &LinkMeta,
    image: Option<&ImageMeta>,
    url: &str,
    player: bool,
    y0: f32,
    out: &mut Vec<Item>,
) -> f32 {
    let w = cx.width;
    let aspect = if player { PLAYER_ASPECT } else { IMAGE_ASPECT };
    // The image fills the border box minus its border.
    let image_h = super::snap(((w - 2.0 * BORDER) / aspect) as f64);
    let h = image_h + 2.0 * BORDER;
    let card = Rect::new(0.0, y0, w, h);
    fill(
        out,
        rounded(card, crate::tokens::RADIUS_MD_RADIUS),
        cx.p.bg_secondary,
    );

    let inner = Rect::new(BORDER, y0 + BORDER, w - 2.0 * BORDER, image_h);
    if image.is_some() {
        let s = rounded(inner, crate::tokens::RADIUS_MD_RADIUS);
        out.push(Item::PushClip { shape: s });
        out.push(Item::Image {
            shape: s,
            image: ImageRef::Card(url.to_string()),
            focus: super::FOCUS,
            // The large card's picture is the image service's copy for the slot
            // (`paint::service_box`); the `summary` thumbnail's variant is not
            // measured (the payload lists 144x144, 280x150, 386x202 and 600x314
            // copies), so it draws the file as given.
            service: true,
        });
        out.push(Item::PopClip);
        if player {
            play_control(cx, inner, out);
        }
        if let Some(title) = meta
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            title_pill(cx, inner, title, out);
        }
    } else {
        missing_icon(cx, inner, out);
    }
    outline(
        out,
        rounded(card, crate::tokens::RADIUS_MD_RADIUS),
        cx.p.border,
    );

    // `flex flex-col gap-1`: the credit line sits 4 px below the card.
    let label = format!("From {}", domain_of(meta, url));
    let block = credit(cx, &label, w);
    push_text(out, &block, 0.0, y0 + h + 4.0, w);
    h + 4.0 + block.height
}

fn credit(cx: &Ctx, label: &str, width: f32) -> crate::inline::TextBlock {
    paragraph_wrapped(
        cx,
        label,
        SUBTEXT2,
        400,
        cx.p.secondary,
        width,
        Some(1),
        Wrap::Normal,
    )
}

/// The overlaid title: a 20 px pill 12 px inside the card's border, 8 px of
/// horizontal padding, `rounded-xs` (7.2 px squircle), `bg-black/75`.
///
/// The pill is an `inline-flex` with `max-w-full`, so it takes its content's
/// preferred width — the whole title on one line — capped at the space
/// available; when it is capped, the text wraps inside it at word boundaries
/// and the first line is ellipsized.
fn title_pill(cx: &Ctx, card: Rect, title: &str, out: &mut Vec<Item>) {
    let (rect, block) = pill_of(cx, card, title);
    fill(
        out,
        rounded(rect, crate::tokens::RADIUS_XS_RADIUS),
        cx.p.black_75,
    );
    push_text(
        out,
        &block,
        rect.x + PILL_PAD,
        rect.y + (rect.h - block.height) * 0.5,
        rect.w - 2.0 * PILL_PAD,
    );
}

/// The `h-5 px-2` pill and its title, for a card whose content box is `card`.
///
/// The pill is `inline-flex max-w-full` inside `inset-s-3 inset-e-3`: its width
/// is the title's advance plus 16 px (`px-2` on each side), capped at the card's
/// inset box (12 px in from each side). The title itself is `line-clamp-1
/// text-ellipsis`, so it only gets an ellipsis when the cap is what limits the
/// pill.
fn pill_of(cx: &Ctx, card: Rect, title: &str) -> (Rect, crate::inline::TextBlock) {
    let avail = (card.w - 24.0).max(0.0);
    let natural = paragraph_wrapped(
        cx,
        title,
        SUBTEXT2,
        400,
        cx.p.on_color,
        f32::MAX,
        None,
        Wrap::Normal,
    )
    .width;
    let fits = natural + 2.0 * PILL_PAD <= avail;
    let w = if fits {
        natural + 2.0 * PILL_PAD
    } else {
        avail
    };
    // Laying the title out at exactly its own advance is one rounding step
    // short of what the text engine needs to *fit* that line: its line width is
    // a LayoutUnit sum, so a title that just fits came back as a two-line block
    // clamped to one line and painted "The thing,…". A 1/64 px of slack on the
    // fit budget is invisible (the text still ends at `natural`) and keeps the
    // ellipsis for the case it is meant for, a title capped by the card.
    let slack = if fits { 1.0 / 64.0 } else { 0.0 };
    let text_w = (w - 2.0 * PILL_PAD).max(1.0);
    let block = paragraph_wrapped(
        cx,
        title,
        SUBTEXT2,
        400,
        cx.p.on_color,
        text_w + slack,
        Some(1),
        Wrap::Normal,
    );
    let x = card.x + 12.0;
    let y = card.y + card.h - 12.0 - PILL_H;
    (Rect::new(x, y, w, PILL_H), block)
}

/// `summary`: a square thumbnail on the left, a three-line text column that is
/// vertically centered against it, and a `border-e` hairline between them.
fn thumbnail_card(
    cx: &Ctx,
    meta: &LinkMeta,
    image: Option<&ImageMeta>,
    url: &str,
    y0: f32,
    out: &mut Vec<Item>,
) -> f32 {
    let desktop = cx.viewport >= SM_BREAKPOINT;
    let thumb_w = if desktop { THUMB_DESKTOP } else { THUMB_MOBILE };
    let pad = 12.0;
    let gap = 2.0;
    // Everything sits inside the card's 1 px border.
    let text_w = (cx.width - 2.0 * BORDER - thumb_w - 2.0 * pad).max(1.0);

    struct Line {
        text: String,
        size: (f32, f32),
        color: crate::color::Rgba,
        max: usize,
    }
    let mut lines: Vec<Line> = Vec::new();
    let domain = domain_of(meta, url);
    if !domain.is_empty() {
        lines.push(Line {
            text: domain,
            size: SUBTEXT2,
            color: cx.p.secondary,
            max: 1,
        });
    }
    if let Some(title) = meta
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        lines.push(Line {
            text: title.to_string(),
            size: BODY,
            color: cx.p.primary,
            max: if desktop { 1 } else { 2 },
        });
    }
    if desktop {
        if let Some(desc) = meta
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            lines.push(Line {
                text: desc.to_string(),
                size: SUBTEXT2,
                color: cx.p.secondary,
                max: 2,
            });
        }
    }

    let mut blocks = Vec::with_capacity(lines.len());
    let mut text_h = 0.0;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            text_h += gap;
        }
        let b = paragraph_wrapped(
            cx,
            &line.text,
            line.size,
            400,
            line.color,
            text_w,
            Some(line.max),
            Wrap::Normal,
        );
        text_h += b.height;
        blocks.push(b);
    }

    let content_h = thumb_w.max(text_h + 2.0 * pad);
    let h = content_h + 2.0 * BORDER;
    let card = Rect::new(0.0, y0, cx.width, h);
    fill(
        out,
        rounded(card, crate::tokens::RADIUS_MD_RADIUS),
        cx.p.bg_secondary,
    );

    let thumb = Rect::new(BORDER, y0 + BORDER, thumb_w, thumb_w);
    if image.is_some() {
        let s = rounded(thumb, crate::tokens::RADIUS_MD_RADIUS);
        out.push(Item::PushClip { shape: s });
        out.push(Item::Image {
            shape: s,
            image: ImageRef::Card(url.to_string()),
            focus: super::FOCUS,
            service: false,
        });
        out.push(Item::PopClip);
    } else {
        // X's fallback is a `bg-secondary` box with a centered icon.
        fill(
            out,
            shape(
                Rect::new(thumb.x, thumb.y, thumb.w - 1.0, thumb.h),
                0.0,
                CornerShape::Round,
            ),
            cx.p.bg_secondary,
        );
        // The `border-e` hairline takes 1 px off the box the icon centers in.
        missing_icon(cx, Rect::new(thumb.x, thumb.y, thumb.w - 1.0, thumb.h), out);
    }
    // `border-e`: a 1 px line inside the thumbnail's inline end edge.
    out.push(Item::Stroke {
        shape: shape(
            Rect::new(BORDER + thumb_w - 1.0, y0 + BORDER, 1.0, thumb_w),
            0.0,
            CornerShape::Round,
        ),
        color: cx.p.border,
        width: 1.0,
    });
    outline(
        out,
        rounded(card, crate::tokens::RADIUS_MD_RADIUS),
        cx.p.border,
    );

    let mut y = y0 + BORDER + (content_h - text_h) * 0.5;
    for b in &blocks {
        push_text(out, b, BORDER + thumb_w + pad, y, text_w);
        y += b.height + gap;
    }
    h
}

/// What X shows when the card has no image: a `bg-secondary` box with a 32 px
/// icon in `text-secondary` centered in it.
fn missing_icon(cx: &Ctx, rect: Rect, out: &mut Vec<Item>) {
    let size = 32.0;
    out.push(Item::Icon {
        name: "icon-link".to_string(),
        rect: Rect::new(
            rect.x + (rect.w - size) * 0.5,
            rect.y + (rect.h - size) * 0.5,
            size,
            size,
        ),
        color: cx.p.secondary,
    });
}

/// A centered play control over a player card's image.
fn play_control(cx: &Ctx, card: Rect, out: &mut Vec<Item>) {
    let size = 48.0;
    let x = card.x + (card.w - size) * 0.5;
    let y = card.y + (card.h - size) * 0.5;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RenderOptions, Surface, Theme};

    fn ctx(width: f32) -> Ctx {
        let opts = RenderOptions {
            width: 390.0,
            theme: Theme::Light,
            dpr: 2.0,
            surface: Surface::WebTimeline,
            font_scale: 1.0,
            expanded: false,
        };
        Ctx::new(&opts, width)
    }

    /// A title that fits its pill must be drawn whole. The pill is the title's
    /// advance plus `px-2` on each side, and laying the title out at exactly
    /// that width used to come back as a clamped two-line block — the bug that
    /// painted "The thing,…" inside a pill Chrome fills completely.
    #[test]
    fn a_title_that_fits_is_not_truncated() {
        let cx = ctx(310.0);
        let card = Rect::new(0.0, 0.0, 310.0, 161.25);
        // The `inset-s-3 inset-e-3` box, minus `px-2` on each side.
        let cap = card.w - 24.0 - 2.0 * PILL_PAD;
        // A title whose advance lands as close to the cap as a digit allows: the
        // shape of the bug (the pill was exactly the text, and the fit test
        // wanted one LayoutUnit more).
        let mut edge = String::from("Shipped");
        while paragraph_wrapped(
            &cx,
            &format!("{edge}i"),
            SUBTEXT2,
            400,
            cx.p.on_color,
            f32::MAX,
            None,
            Wrap::Normal,
        )
        .width
            <= cap - 0.5
        {
            edge.push('i');
        }
        for title in [
            "The thing, shipped",
            "A",
            "iiiiiiii",
            "WWWWWWWW",
            "Breaking: something happened",
            edge.as_str(),
        ] {
            let (rect, block) = pill_of(&cx, card, title);
            assert!(
                !block.truncated,
                "{title:?} must fit its pill, was truncated"
            );
            assert_eq!(block.lines.len(), 1, "{title:?} must be one line");
            assert!(
                rect.w <= card.w - 24.0 + 0.001,
                "{title:?} pill wider than the card"
            );
            assert!(
                block.width <= rect.w - 2.0 * PILL_PAD + 1.0 / 64.0 + 0.001,
                "{title:?} text wider than the pill"
            );
        }
    }

    /// A title longer than the card's inset box is clamped to one line and gets
    /// the ellipsis, with the pill at the full available width.
    #[test]
    fn a_title_too_long_for_the_card_is_clamped() {
        let cx = ctx(310.0);
        let card = Rect::new(0.0, 0.0, 310.0, 161.25);
        let title =
            "A title long enough that it cannot fit across the width of a summary card at all";
        let (rect, block) = pill_of(&cx, card, title);
        assert!(block.truncated, "the long title must be truncated");
        assert_eq!(block.lines.len(), 1);
        assert_eq!(
            rect.w,
            card.w - 24.0,
            "a clamped pill takes the full inset box"
        );
    }
}
