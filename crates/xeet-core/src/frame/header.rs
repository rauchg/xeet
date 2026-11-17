//! The post header: avatar column (laid out by the surface), name, badge,
//! handle, the middot, the timestamp and the "More" button.
//!
//! X builds the timeline header and the detail header differently, and the
//! difference is not cosmetic:
//!
//! - **Timeline** (a cell in a timeline or conversation): one 20px row.
//!   `name ✓ @handle · 3h` is a single flex line, with the "More" button on
//!   the right. Name and handle each clamp to one line.
//! - **Post detail** (the focal post): the name row and the handle row are
//!   stacked, 20px each, and the timestamp moves *below the text*, next to
//!   the view count.
//!
//! The captured markup (from `https://x.com/jack/status/20`, logged out)
//! nests the timeline header like this:
//!
//! ```text
//! div.flex.items-start.justify-between.gap-2        the row; gap 8px
//!   div.x-middot-group.flex.text-body                children separated by "·"
//!     div.flex.items-center.gap-1.min-w-0            name block + handle
//!       span.flex.items-center.gap-1.max-w-full.min-w-0.shrink-0
//!         a > div.line-clamp-1.break-all             the name
//!         div.flex.items-center.gap-1                badge, (affiliate)
//!       span.flex.items-center.gap-2.max-w-full.min-w-0
//!         a > span.line-clamp-1.wrap-break-word[ss01]  @handle
//!     span.inline-flex.w-fit.shrink-0.whitespace-nowrap
//!       (::before "·" with 4px padding on each side)
//!       a.text-body.text-secondary                   3h
//!   div.-me-[5px].flex.shrink-0.items-center         the "More" button
//! ```
//!
//! `.x-middot-group > :not(:first-child)::before` from the stylesheet is what
//! draws the separator: `content: "·"; padding-inline: 4px`. That is why the
//! gap between the handle and the time reads as 12px, not 4px.
//!
//! ## How the row shares its width
//!
//! Measured on the replica at 320/390/600 with long names and long handles:
//!
//! 1. The row is `content − "More" outer width − 8`. "More" is 32px wide with
//!    a negative end margin (`-me-[5px]` on the timeline, `-me-0.5` on the
//!    detail view), so its outer width is 27px and 30px respectively. It
//!    overhangs into the 16px padding, which is why its box can reach past the
//!    content column.
//! 2. The timestamp never shrinks (`shrink-0 whitespace-nowrap`). Its width is
//!    4 + "·" + 4 + the time text.
//! 3. The name/handle group takes what is left: `min(natural, row − time)`.
//! 4. Inside it, the name span is `shrink-0` with `max-w-full`, so the name is
//!    `min(natural, group)` and the handle gets `group − name − 4`, clamped at
//!    zero. When the name is long the handle collapses to 0px and the group
//!    overflows by the 4px gap -- Chrome places the timestamp at the group's
//!    width regardless, and so do we.
//!
//! Verified against Chrome for `The Extremely Long Display Name Of A Person
//! Who Really Needed Every Character` (name clamps, handle 0px wide, timestamp
//! overlaps it at 390 and 600) and for a 70-character handle (name natural,
//! handle clamps).

use crate::color::Rgba;
use crate::display::{ImageRef, Item, Radii, Rect, Shape};
use crate::inline::{self, BlockStyle, Dir, Span, TextBlock, Wrap};
use crate::model::Verified;
use crate::tokens;

use super::time;
use super::Ctx;

/// A laid-out header.
pub struct Header {
    pub items: Vec<Item>,
    /// Height of the header block: one line on the timeline, two on the
    /// detail view.
    pub height: f32,
    /// Width of the row itself (`content − "More" outer − 8`).
    pub row_w: f32,
    /// The name span: name block plus badge, capped like Chrome caps it.
    pub name_block: Rect,
    pub name_rect: Rect,
    pub badge: Option<Rect>,
    /// The affiliation badge's box, one `gap-1` after the check.
    pub affiliate: Option<Rect>,
    pub handle_rect: Rect,
    /// The `x-middot-group` box (timeline only).
    pub group_rect: Option<Rect>,
    /// The timestamp span, including the middot's padding (timeline only).
    pub stamp_span: Option<Rect>,
    /// Where the timestamp text starts, if there is one.
    pub stamp_x: Option<f32>,
    pub more_rect: Rect,
    /// The timestamps: (text, x) pieces for the detail view's row.
    pub stamp_pieces: Vec<(String, f32)>,
}

/// Turns a string into one clamped line at the given weight and width.
/// One clamped line of text, as the header uses it.
pub fn one_line(
    text: &str,
    color: Rgba,
    weight: u16,
    style: tokens::TypeStyle,
    wrap: Wrap,
    ss01: bool,
    width: f32,
) -> TextBlock {
    let span = Span {
        text: text.to_string(),
        color,
        weight,
        ss01,
        tnum: false,
        atomic: false,
        role: "text",
    };
    inline::layout(
        &[span],
        &BlockStyle {
            size: style.size,
            line_height: style.line_height,
            weight,
            width: width.max(0.0),
            max_lines: Some(1),
            wrap,
            preserve: false,
            dir: Dir::Auto,
            tab_size: 4.0,
        },
    )
}

/// Chrome's layout unit: 1/64 px. Every box width Chrome reports is a multiple.
#[inline]
pub fn ceil64(w: f32) -> f32 {
    (w * 64.0).ceil() / 64.0
}

/// Natural (max-content) width of one line of text, snapped up to 1/64 px.
///
/// The snapping matters: Chrome sizes the name and handle boxes in layout
/// units, so "Vercel" at 15/700 is a 45.390625px box while the shaped advance
/// sums to 45.389984. Laying out at the unrounded width makes the block wrap
/// its last grapheme, which is exactly the bug this rounding prevents.
pub fn natural_width(text: &str, weight: u16, style: tokens::TypeStyle, ss01: bool) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let block = one_line(
        text,
        Rgba::BLACK,
        weight,
        style,
        Wrap::Normal,
        ss01,
        f32::MAX / 4.0,
    );
    ceil64(block.width)
}

/// Width of U+00B7 in the header's font: the middot separator.
fn dot_advance(style: tokens::TypeStyle) -> f32 {
    natural_width("\u{b7}", tokens::WEIGHT_NORMAL, style, false)
}

/// The whole header, positioned at (`x`, `y`) with `width` available.
///
/// `width` is the content column on the timeline and the header column (after
/// the avatar) on the detail view, because that is what the markup nests.
pub(crate) fn layout(ctx: &Ctx, x: f32, y: f32, width: f32, detail: bool) -> Header {
    // `text-body` on web (15/20), 15.5/20.67 on iOS.
    let style = ctx.header_style();
    let ios_timeline = ctx.opts.surface == crate::model::Surface::IosTimeline;
    let dot_pad = if ios_timeline {
        2.0
    } else {
        tokens::DOT_PADDING
    };
    let bold = tokens::WEIGHT_BOLD;
    let normal = tokens::WEIGHT_NORMAL;
    let post = ctx.post;
    let name_text = &post.author.name;
    let handle_text = if post.author.handle.is_empty() {
        String::new()
    } else {
        format!("@{}", post.author.handle.trim_start_matches('@'))
    };

    let badge_w = match post.author.verified {
        Verified::None => 0.0,
        _ => tokens::BADGE_EM * style.size,
    };
    // The affiliation badge is a second `size-[1em]` square in the same
    // wrapper, one `gap-1` after the check (measured on `jack`'s post:
    // check 15px, affiliate 15px, 4px between, so the name span grows by 19).
    let affiliate_w = if post.author.affiliate.is_some() {
        tokens::BADGE_EM * style.size
    } else {
        0.0
    };
    let decorations_w = (if badge_w > 0.0 {
        tokens::GAP_TIGHT + badge_w
    } else {
        0.0
    }) + (if affiliate_w > 0.0 {
        tokens::GAP_TIGHT + affiliate_w
    } else {
        0.0
    });
    let name_natural_block = (!name_text.is_empty()).then(|| {
        one_line(
            name_text,
            ctx.colors.fg_primary,
            bold,
            style,
            Wrap::Normal,
            false,
            f32::MAX / 4.0,
        )
    });
    let name_natural = name_natural_block.as_ref().map_or(0.0, |b| ceil64(b.width));
    let handle_natural_block = (!handle_text.is_empty()).then(|| {
        one_line(
            &handle_text,
            ctx.colors.fg_secondary,
            normal,
            style,
            Wrap::Normal,
            true,
            f32::MAX / 4.0,
        )
    });
    let handle_natural = handle_natural_block
        .as_ref()
        .map_or(0.0, |b| ceil64(b.width));
    let more_outer = if ios_timeline {
        22.0
    } else {
        tokens::MORE_BUTTON_SIZE
            + if detail {
                tokens::MORE_MARGIN_END_DETAIL
            } else {
                tokens::MORE_MARGIN_END_TIMELINE
            }
    };

    let mut items = Vec::new();
    let (name_rect, handle_rect);

    if detail {
        // Two rows: name, then handle. Each is capped at the header column.
        let row_w = (width - more_outer - tokens::GAP_ROW).max(0.0);
        let name_row_w = name_natural + decorations_w;
        let name_block_w = name_row_w.min(row_w);
        let name_w = (name_block_w - decorations_w).max(0.0);
        let name_block = match name_natural_block {
            Some(b) if name_w >= name_natural => b,
            _ => one_line(
                name_text,
                ctx.colors.fg_primary,
                bold,
                style,
                Wrap::BreakAll,
                false,
                name_w,
            ),
        };
        items.extend(glyph_items(&name_block, x, y));

        // The handle row lives in the same flex column as the name row, and
        // that column is capped at the row width (it is a flex item of the
        // header row), so a long handle clamps there, not at the full column.
        let handle_w = handle_natural.min(row_w);
        if !handle_text.is_empty() {
            // `line-clamp-1 wrap-break-word`: the handle breaks inside the
            // word, the clamp keeps line one, and that line ends in "…".
            let block = match handle_natural_block {
                Some(b) if handle_w >= handle_natural => b,
                _ => one_line(
                    &handle_text,
                    ctx.colors.fg_secondary,
                    normal,
                    style,
                    Wrap::Anywhere,
                    true,
                    handle_w,
                ),
            };
            items.extend(clipped(
                &block,
                x,
                y + style.line_height,
                handle_w,
                style.line_height,
            ));
        }
        name_rect = Rect::new(x, y, name_w, style.line_height);
        handle_rect = Rect::new(x, y + style.line_height, handle_w, style.line_height);
        let badge = (badge_w > 0.0).then(|| {
            Rect::new(
                x + name_w + tokens::GAP_TIGHT,
                y + (style.line_height - badge_w) / 2.0,
                badge_w,
                badge_w,
            )
        });
        if let Some(rect) = badge {
            items.extend(affiliate_items(ctx, rect, affiliate_w));
            items.push(badge_item(ctx, rect, style));
        }
        // The "More" button hangs 8px above the row and 2px into the padding.
        let more_rect = Rect::new(
            x + width - more_outer,
            y + tokens::MORE_MARGIN_Y,
            tokens::MORE_BUTTON_SIZE,
            tokens::MORE_BUTTON_SIZE,
        );
        items.push(more_item(more_rect, ctx.colors.fg_primary));
        Header {
            items,
            height: style.line_height * 2.0,
            row_w,
            name_block: Rect::new(x, y, name_block_w, style.line_height),
            name_rect,
            badge,
            affiliate: affiliate_rect(badge, affiliate_w),
            handle_rect,
            group_rect: None,
            stamp_span: None,
            stamp_x: None,
            more_rect,
            stamp_pieces: stamp_pieces(ctx),
        }
    } else {
        // One row: name, badge, handle, middot, timestamp, then "More".
        let row_w = (width - more_outer - tokens::GAP_ROW).max(0.0);
        let name_group_w = name_natural + decorations_w;
        let inner_natural = name_group_w + tokens::GAP_TIGHT + handle_natural;
        let stamp = ctx.stamp_label();
        let stamp_parts = (!stamp.is_empty()).then(|| {
            let dot = one_line(
                "\u{b7}",
                ctx.colors.fg_secondary,
                normal,
                style,
                Wrap::Normal,
                false,
                f32::MAX / 4.0,
            );
            let dot_w = ceil64(dot.width);
            let block = one_line(
                &stamp,
                ctx.colors.fg_secondary,
                normal,
                style,
                Wrap::Normal,
                false,
                f32::MAX / 4.0,
            );
            let w = ceil64(block.width) + 2.0 * dot_pad + dot_w;
            (w, dot_w, dot, block)
        });
        let stamp_w = stamp_parts.as_ref().map_or(0.0, |(w, _, _, _)| *w);
        let group_w = (inner_natural + stamp_w).min(row_w);
        let inner_w = inner_natural.min((group_w - stamp_w).max(0.0));
        let name_group_used = name_group_w.min(inner_w);
        let name_w = (name_group_used - decorations_w).max(0.0);
        let handle_w = (inner_w - name_group_used - tokens::GAP_TIGHT).max(0.0);

        let name_block = match name_natural_block {
            Some(b) if name_w >= name_natural => b,
            _ => one_line(
                name_text,
                ctx.colors.fg_primary,
                bold,
                style,
                Wrap::BreakAll,
                false,
                name_w,
            ),
        };
        items.extend(clipped(&name_block, x, y, name_w, style.line_height));
        let handle_x = x + name_group_used + tokens::GAP_TIGHT;
        if !handle_text.is_empty() {
            // `line-clamp-1 wrap-break-word`: when the handle is squeezed,
            // Chrome breaks it inside the word and ends the kept line in "…"
            // ("@Twitter" at 320 draws as "@…"), rather than cutting a glyph.
            let block = match handle_natural_block {
                Some(b) if handle_w >= handle_natural => b,
                _ => one_line(
                    &handle_text,
                    ctx.colors.fg_secondary,
                    normal,
                    style,
                    Wrap::Anywhere,
                    true,
                    handle_w,
                ),
            };
            items.extend(clipped(&block, handle_x, y, handle_w, style.line_height));
        }
        name_rect = Rect::new(x, y, name_w, style.line_height);
        handle_rect = Rect::new(handle_x, y, handle_w, style.line_height);
        let badge = (badge_w > 0.0).then(|| {
            Rect::new(
                x + name_w + tokens::GAP_TIGHT,
                y + (style.line_height - badge_w) / 2.0,
                badge_w,
                badge_w,
            )
        });
        if let Some(rect) = badge {
            items.extend(affiliate_items(ctx, rect, affiliate_w));
            items.push(badge_item(ctx, rect, style));
        }

        let mut stamp_x = None;
        let mut stamp_span = None;
        if let Some((_, dot_w, dot, block)) = stamp_parts {
            stamp_span = Some(Rect::new(x + inner_w, y, stamp_w, style.line_height));
            // The middot is a ::before inside the timestamp span: 4px of
            // padding, the glyph, 4px more, then the text.
            let span_x = x + inner_w;
            let dot_x = span_x + dot_pad;
            items.extend(glyph_items(&dot, dot_x, y));
            let text_x = dot_x + dot_w + dot_pad;
            items.extend(glyph_items(&block, text_x, y));
            stamp_x = Some(text_x);
        } else if !post.author.handle.is_empty() {
            // No timestamp: X keeps the middot only between present parts.
        }

        let more_rect = if ios_timeline {
            Rect::new(
                x + width - 18.0,
                y + (style.line_height - 16.0) / 2.0,
                18.0,
                16.0,
            )
        } else {
            Rect::new(
                x + width - more_outer,
                y + tokens::MORE_MARGIN_Y,
                tokens::MORE_BUTTON_SIZE,
                tokens::MORE_BUTTON_SIZE,
            )
        };
        items.push(if ios_timeline {
            more_item(more_rect, ctx.colors.fg_secondary)
        } else {
            more_item(more_rect, ctx.colors.fg_primary)
        });
        Header {
            items,
            height: style.line_height,
            row_w,
            name_block: Rect::new(x, y, name_group_used, style.line_height),
            name_rect,
            badge,
            affiliate: affiliate_rect(badge, affiliate_w),
            handle_rect,
            group_rect: Some(Rect::new(x, y, group_w, style.line_height)),
            stamp_span,
            stamp_x,
            more_rect,
            stamp_pieces: Vec::new(),
        }
    }
}

/// Glyphs clipped to their block's box.
///
/// `line-clamp-1` sets `overflow: hidden`, and Chrome relies on it: a name or
/// handle whose box is narrower than its text (down to 0px when the header is
/// squeezed) paints nothing outside the box. Without the clip the overflowing
/// graphemes would be visible.
pub fn clipped(block: &TextBlock, x: f32, y: f32, width: f32, line_height: f32) -> Vec<Item> {
    let mut out = Vec::new();
    if block.lines.is_empty() || width <= 0.0 {
        return out;
    }
    out.push(Item::PushClip {
        shape: Shape::rect(Rect::new(x, y, width, line_height)),
    });
    out.extend(glyph_items(block, x, y));
    out.push(Item::PopClip);
    out
}

/// A text block's glyphs as display items, preserving span colors.
pub fn glyph_items(block: &TextBlock, x: f32, y: f32) -> Vec<Item> {
    let mut list = crate::display::DisplayList::new(0.0, 0.0, Rgba::TRANSPARENT);
    list.push_text(block, x, y);
    list.items
}

/// Where the affiliation badge sits: one `gap-1` after the check, if there is
/// an affiliation at all.
fn affiliate_rect(check: Option<Rect>, width: f32) -> Option<Rect> {
    let check = check?;
    (width > 0.0).then(|| Rect::new(check.x + check.w + tokens::GAP_TIGHT, check.y, width, width))
}

fn badge_item(ctx: &Ctx, rect: Rect, style: tokens::TypeStyle) -> Item {
    // The badge is an `<svg class="size-[1em]">` centered in the 20px row, and
    // the three variants differ only in artwork (premium / business /
    // government), which `assets/vendor/icons.json` carries.
    let name = match ctx.post.author.verified {
        Verified::Business => "icon-verified-business-color",
        Verified::Government => "icon-verified-government-color",
        Verified::Blue | Verified::None => "icon-verified-premium-color",
    };
    let _ = style;
    Item::Icon {
        name: name.to_string(),
        rect,
        color: ctx.colors.fg_primary,
    }
}

/// The affiliation badge: an image one em square (`size-[1em]`), `rounded-xs`
/// — 7.2px drawn as a superellipse, because Chrome satisfies
/// `@supports (corner-shape: squircle)` — with a 1px inner border in the
/// theme's normal border color (`border border-normal`, `box-sizing:
/// border-box`, so the border does not grow the box).
///
/// Measured on `jack`'s post: check 15px, 4px gap, affiliate 15px, 4px gap,
/// handle. The name span therefore grows by 19px when an affiliation exists.
/// Without image bytes we draw a neutral square of the same shape and border
/// instead of an `Image`, the way the avatar falls back to a placeholder.
fn affiliate_items(ctx: &Ctx, check: Rect, width: f32) -> Vec<Item> {
    let Some(affiliate) = ctx.post.author.affiliate.as_ref() else {
        return Vec::new();
    };
    if width <= 0.0 {
        return Vec::new();
    }
    let radius = tokens::RADIUS_XS_RADIUS;
    let rect = Rect::new(check.x + check.w + tokens::GAP_TIGHT, check.y, width, width);
    let shape = Shape {
        rect,
        radii: Radii::all(radius.value.min(width / 2.0)),
        corner: radius.corner,
    };
    if affiliate.badge.is_some() {
        vec![
            Item::Image {
                shape,
                image: ImageRef::AffiliateBadge,
                focus: (0.5, 0.5),
                service: false,
            },
            Item::Stroke {
                shape,
                color: ctx.colors.border_normal,
                width: 1.0,
            },
        ]
    } else {
        vec![
            Item::Fill {
                shape,
                color: ctx.colors.bg_secondary,
            },
            Item::Stroke {
                shape,
                color: ctx.colors.border_normal,
                width: 1.0,
            },
        ]
    }
}

fn more_item(rect: Rect, color: Rgba) -> Item {
    // The button is 32x32 with a 16x16 icon centered in it.
    Item::Icon {
        name: "icon-more-dots-hor".to_string(),
        rect: centered(rect, tokens::MORE_ICON_SIZE),
        color,
    }
}

fn centered(rect: Rect, size: f32) -> Rect {
    Rect::new(
        rect.x + (rect.w - size) / 2.0,
        rect.y + (rect.h - size) / 2.0,
        size,
        size,
    )
}

/// The full timestamp line of the detail view: the instant, then the view
/// count, separated by the same middot the header uses.
///
/// X's captured markup for a post without an impression count has only the
/// instant; the view count is server-rendered when it exists
/// (`1.2M Views`). This function returns the pieces so the surface can stack
/// them, and the caller paints them in one row.
pub(crate) fn stamp_pieces(ctx: &Ctx) -> Vec<(String, f32)> {
    // (text, x offset from the row start)
    let style = ctx.header_style();
    let normal = tokens::WEIGHT_NORMAL;
    let mut out = Vec::new();
    let mut x = 0.0;
    let stamp = if ctx.opts.surface.is_ios() {
        time::label_ios(
            ctx.post.created_at.as_deref(),
            ctx.post.now.as_deref(),
            true,
        )
    } else {
        time::label(
            ctx.post.created_at.as_deref(),
            ctx.post.now.as_deref(),
            true,
        )
    };
    let w = natural_width(&stamp, normal, style, false);
    out.push((stamp, 0.0));
    x += w;
    if let Some(views) = ctx
        .post
        .counts
        .as_ref()
        .and_then(|c| (c.views > 0).then_some(c.views))
    {
        let dot = tokens::DOT_PADDING;
        out.push(("\u{b7}".to_string(), x + dot));
        x += dot + dot_advance(style) + dot;
        // The detail label uses X's *view* formatter, not the action row's:
        // plain and grouped below 10,000 ("5,264 Views"), then compact with one
        // truncated fraction digit at every magnitude ("10.3K Views",
        // "133.3K Views"). `view-count-utils`, see SPEC.md §9.
        out.push((format!("{} Views", super::numbers::views(views)), x));
    }
    out
}
