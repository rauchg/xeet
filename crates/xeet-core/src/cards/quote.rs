//! The quoted post.
//!
//! The frame is a `rounded-2xl` box (16 px, *not* scaled by the radius
//! multiplier, and not a squircle) with a 1 px `border-normal` hairline. Inside
//! it, X's markup puts two siblings — the padded header/text block, then the
//! attachment — which is why the media here is flush with the frame's sides
//! while the text is inset by 12 px:
//!
//! ```html
//! <div class="border-normal overflow-hidden rounded-2xl border
//!             [--media-overhang-end:0px] [--media-overhang-start:0px]">
//!   <a class="absolute inset-0 pointer-events-none"></a>
//!   <article>
//!     <div class="flex flex-col gap-1 p-3">            <!-- header + text -->
//!       <div class="flex min-w-0 flex-1 flex-col gap-1">
//!         <div class="flex min-w-0 items-center gap-1"> <!-- avatar 24, name, handle, time -->
//!         <div class="text-body line-clamp-5 whitespace-pre-wrap wrap-anywhere">…</div>
//!       </div>
//!     </div>
//!     <div class="mx-auto relative overflow-hidden"     <!-- media, no padding -->
//!          style="aspect-ratio:0.8;max-width:408px">…</div>
//!   </article>
//! </div>
//! ```
//!
//! The quoted post's media uses the compact carousel variant: its
//! `--media-overhang` is 0 and the layout code runs with `isMobile: true`, so
//! the fallback height ratio is 0.66 instead of 1.24446 and the base is
//! 0.68 × 0.85. Evidence: the quote in `text-levelsio.html`, whose two 0.70588
//! cells carry `padding-bottom: 66%`.

use super::{
    body_block, media, outline, paragraph, push_text, rounded, shape, shift, Ctx, Item, Rect, BODY,
    SUBTEXT1,
};
use crate::display::{CornerShape, ImageRef};
use crate::model::{self, Verified};

/// The frame's own `border`.
const BORDER: f32 = 1.0;
/// `p-3` on the header/text block.
const PAD: f32 = 12.0;
/// `gap-1` between the header and the text, and before the attachment.
const GAP: f32 = 4.0;
/// `size-6`: the avatar of a quoted post shown with its media below the text.
const AVATAR: f32 = 24.0;
/// `size-4`: the avatar of a quoted post shown in the *compact* form (its media
/// is a video, so it becomes a thumbnail beside the text).
const AVATAR_COMPACT: f32 = 16.0;
/// `w-24 aspect-square`: the compact form's thumbnail.
const THUMB: f32 = 96.0;
/// `gap-2` between that thumbnail and the text column.
const THUMB_GAP: f32 = 8.0;
/// The quote's verified clamp, from `rules::decide_quote`.
const TEXT_CLAMP: usize = 5;

/// Lays out the quoted post and returns its height.
pub(crate) fn layout(
    cx: &Ctx,
    outer: &model::Post,
    post: &model::Post,
    opts: &model::RenderOptions,
    compact: bool,
    y0: f32,
    out: &mut Vec<Item>,
) -> f32 {
    // A quoted payload carries `createdAt` but not a clock sample of its own in
    // practice, so the reference instant is the outer post's `now` unless the
    // quote has one.
    let now = clock(post, outer.now.as_deref());
    let content = cx.width - 2.0 * BORDER;
    let text_w = content - 2.0 * PAD;

    // X draws a quote in one of two forms. When the *quoting* post has media of
    // its own the quote is compact: a 16 px avatar and, if the quoted post has
    // media, a 96 px square thumbnail beside the text instead of a full-width
    // media box. A post with nothing but a quote keeps the full-width form (24 px
    // avatar, the quoted media below the text). Every one of the 30 quotes with
    // media in the corpus follows that split; the three compact ones are the
    // three that sit under a post with media (`NBA.html`).

    // The padded block: header row, the `Replying to` row, then the body.
    let mut y = BORDER + PAD;
    let avatar = if compact {
        AVATAR_COMPACT
    } else if cx.ios {
        20.0
    } else {
        AVATAR
    };
    y += header(cx, post, now, BORDER + PAD, text_w, y0 + y, avatar, 1, out);
    if let Some(handle) = post
        .reply_to_handle
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        y += GAP;
        y += replying_row(cx, handle, text_w, y0 + y, out);
    }
    let decisions = crate::rules::decide_quote(post, opts);
    let clamp = decisions.clamp.or(Some(TEXT_CLAMP));

    if compact {
        let thumbed = !post.media.is_empty();
        let indent = if thumbed { THUMB + THUMB_GAP } else { 0.0 };
        let text_x = BORDER + PAD + indent;
        let column = (text_w - indent).max(1.0);
        let mut row_h = if thumbed { THUMB } else { 0.0 };
        if !decisions.body.is_empty() {
            y += GAP;
            let block = body_block(
                cx,
                &decisions.body,
                BODY,
                400,
                cx.p.primary,
                cx.p.brand,
                column,
                clamp,
            );
            push_text(out, &block, text_x, y0 + y, column);
            row_h = row_h.max(block.height);
        } else if thumbed {
            y += GAP;
        }
        if thumbed {
            // The thumbnail is the first child of the `items-start` row, so its
            // top is the row's top even when the text is taller.
            let items = media::items_of(&post.media, &post.author.handle);
            let first = out.len();
            media::compact_thumb(
                cx,
                &items[0],
                Rect::new(BORDER + PAD, y0 + y, THUMB, THUMB),
                out,
            );
            as_quoted(&mut out[first..]);
        }
        y += row_h;
    } else {
        if !decisions.body.is_empty() {
            y += GAP;
            let block = body_block(
                cx,
                &decisions.body,
                BODY,
                400,
                cx.p.primary,
                cx.p.brand,
                text_w,
                clamp,
            );
            push_text(out, &block, BORDER + PAD, y0 + y, text_w);
            y += block.height;
        }
    }
    y += PAD;

    // The attachment is the frame's next child: full content width, centered
    // when it is narrower (`mx-auto`).
    let qcx = Ctx {
        width: content,
        ..*cx
    };
    let mut att: Vec<Item> = Vec::new();
    let att_h = if compact {
        0.0
    } else if !post.media.is_empty() {
        let items = media::items_of(&post.media, &post.author.handle);
        let h = media::layout_quote(&qcx, &items, 0.0, &mut att);
        as_quoted(&mut att);
        h
    } else if let Some(poll) = post.poll.as_ref() {
        super::poll::layout(&qcx, poll, 0.0, &mut att)
    } else {
        0.0
    };
    if att_h > 0.0 {
        shift(&mut att, BORDER, y0 + y);
        out.append(&mut att);
        y += att_h;
    }

    // X's nested quote preview: q1 quotes q2, and q2 gets its own compact card.
    // Eligibility is `rules::decide_nested_quote` (X's `H` in
    // `nested-quote-preview-SIs_tkfq`); it also hands back q2's body and the
    // `clamp: 2` / `plain_entities` / `media_thumb` decisions the preview needs.
    if let Some((q2, decisions)) = post
        .quote
        .as_deref()
        .and_then(|q2| crate::rules::decide_nested_quote(outer, post, q2, opts).map(|d| (q2, d)))
    {
        // The wrapper is `p-3 [:first-child+&]:pt-0`: its top padding is
        // dropped only when the preview is the *second* child of the quoted
        // post's `<article>` — i.e. when the header/text block is its only
        // sibling. A quote that also has media keeps the 12 px at the top.
        {
            if att_h > 0.0 {
                y += PAD;
            }
            y += nested_preview(cx, q2, &decisions, now, content, y0 + y, out);
        }
    }

    let height = y + BORDER;
    outline(
        out,
        rounded(
            Rect::new(0.0, y0, cx.width, height),
            crate::tokens::RADIUS_2XL_RADIUS,
        ),
        cx.p.border,
    );
    height
}

/// The avatar, name, verified badge, `@handle`, `·` and the timestamp in one
/// row, and the row's height: the avatar's, or one `text-body` line when the
/// avatar is smaller than one (16 px + 20 px line in the compact form). The `·`
/// comes from `.x-middot-group`, 4 px on each side.
fn header(
    cx: &Ctx,
    post: &model::Post,
    now: Option<&str>,
    x0: f32,
    width: f32,
    y: f32,
    avatar_px: f32,
    // 1 for the quote's own header, 2 for the nested preview's: the avatar of a
    // post quoted *inside* a quote is `Quote(Quote(Avatar))`, and the painter
    // reads that to know whose bytes to load.
    depth: u8,
    out: &mut Vec<Item>,
) -> f32 {
    let avatar_h = avatar_px.max(cx.ty(BODY).1);
    let r = if post.author.square_avatar {
        avatar_px * 0.1
    } else {
        avatar_px * 0.5
    };
    let top = y + (avatar_h - avatar_px) * 0.5;
    let avatar = shape(
        Rect::new(x0, top, avatar_px, avatar_px),
        r,
        CornerShape::Round,
    );
    // Always an image item: the renderer falls back to X's default profile
    // image when the quoted post has no avatar bytes, so the slot is never a
    // placeholder (x.com always draws a picture there).
    out.push(Item::PushClip { shape: avatar });
    out.push(Item::Image {
        shape: avatar,
        image: ImageRef::Quote(Box::new(if depth >= 2 {
            // A post quoted *inside* a quote: the painter needs both hops to know
            // whose avatar bytes to load.
            ImageRef::Quote(Box::new(ImageRef::Avatar))
        } else {
            ImageRef::Avatar
        })),
        focus: super::FOCUS,
        service: false,
    });
    out.push(Item::PopClip);

    let mut x = x0 + avatar_px + GAP;
    let right = x0 + width;
    let avail = |x: f32| (right - x).max(0.0);
    let centered = |h: f32| y + (avatar_h - h) * 0.5;

    // The name yields enough width for the badges, the middot and the stamp;
    // X's flex row shrinks it in place (a long business name at 320 px is cut by
    // 36.6 px and the handle after it collapses to nothing). Measured against
    // Chrome: name = row width - (badges + `·` + stamp).
    let stamp = super::time::time_label_for(post.created_at.as_deref(), now, cx.ios);
    let stamp_block = paragraph(cx, &stamp, BODY, 400, cx.p.secondary, f32::MAX, Some(1));
    let dot_probe = paragraph(cx, "·", BODY, 400, cx.p.secondary, f32::MAX, Some(1));
    // Chrome adds the pieces up in LayoutUnits and rounds each text box up to
    // the next 1/64, which is what makes the clamp land on 109.39 rather than
    // 109.42 at 320.
    let ceil64 = |v: f32| (v * 64.0).ceil() / 64.0;
    let mut tail = 0.0;
    if verified_icon(post.author.verified).is_some() {
        tail += GAP + cx.em_box(15.0);
    }
    if post
        .author
        .affiliate
        .as_ref()
        .is_some_and(|a| !a.name.trim().is_empty())
    {
        tail += GAP + cx.em_box(15.0);
    }
    // The handle's slot keeps its leading gap even when the handle collapses to
    // nothing, so the name yields that too; without it the stamp ran 4 px past
    // the frame at 320 (`· 20h` drawn as `· 20`).
    tail += GAP;
    tail += GAP + ceil64(dot_probe.width) + GAP + ceil64(stamp_block.width);
    let name_cap = (avail(x) - tail).max(0.0);
    let natural = paragraph(
        cx,
        post.author.name.trim(),
        BODY,
        700,
        cx.p.primary,
        f32::MAX,
        Some(1),
    );
    // A clamped name keeps the width of its box (`max-w-full` on a flex item),
    // not of the text it managed to fit.
    let name_adv = natural.width.min(name_cap);
    // The name's box (`block truncate`, `dir="auto"`) shrinks to the name, so a
    // name that fits is the max-content block itself. Laid out across the whole
    // cap instead, a right-to-left name sat against the cap's far edge, on top
    // of its own badge and handle.
    let name = if natural.width <= name_cap {
        natural
    } else {
        paragraph(
            cx,
            post.author.name.trim(),
            BODY,
            700,
            cx.p.primary,
            name_cap,
            Some(1),
        )
    };
    push_text(out, &name, x, centered(name.height), avail(x));
    x += name_adv;

    if let Some(icon) = verified_icon(post.author.verified) {
        x += GAP;
        let size = cx.em_box(15.0);
        out.push(Item::Icon {
            name: icon.to_string(),
            rect: Rect::new(x, y + (avatar_h - size) * 0.5, size, size),
            color: cx.p.primary,
        });
        x += size;
    }

    // An affiliated organization's badge follows the check, as a
    // `size-[1em]` image (X's `identity_profile_labels_highlighted_label`).
    if let Some(affiliate) = post
        .author
        .affiliate
        .as_ref()
        .filter(|a| !a.name.trim().is_empty())
    {
        x += GAP;
        let size = cx.em_box(15.0);
        let rect = Rect::new(x, y + (avatar_h - size) * 0.5, size, size);
        // `rounded-xs` with a 1 px `border-normal` ring, as in the frame's
        // header (crates/xeet-core/src/frame/header.rs).
        let badge = rounded(rect, crate::tokens::RADIUS_XS_RADIUS);
        if affiliate.badge.is_some() {
            out.push(Item::Image {
                shape: badge,
                image: ImageRef::Quote(Box::new(if depth >= 2 {
                    ImageRef::Quote(Box::new(ImageRef::AffiliateBadge))
                } else {
                    ImageRef::AffiliateBadge
                })),
                focus: super::FOCUS,
                service: false,
            });
        } else {
            // No artwork: the placeholder draws the square, the stroke is the
            // badge's own ring (as in the frame's header).
            out.push(Item::Placeholder {
                shape: badge,
                label: affiliate.name.clone(),
            });
        }
        outline(out, badge, cx.p.border);
        x += size;
    }

    x += GAP;
    // The handle shrinks before the timestamp does: it gets what is left after
    // the `·` and the stamp, and collapses to nothing when that is nothing. It
    // used to take the whole remaining width, which pushed the stamp past the
    // frame's edge at 320 px (`@r… · 20h` cut to `20`).
    let dot = paragraph(cx, "·", BODY, 400, cx.p.secondary, f32::MAX, Some(1));
    let stamp_tail = GAP + ceil64(dot.width) + GAP + ceil64(stamp_block.width);
    let handle_cap = (avail(x) - stamp_tail).max(0.0);
    let handle = format!("@{}", post.author.handle.trim().trim_start_matches('@'));
    if handle_cap >= 1.0 {
        let handle_block = paragraph(cx, &handle, BODY, 400, cx.p.secondary, handle_cap, Some(1));
        push_text(
            out,
            &handle_block,
            x,
            centered(handle_block.height),
            handle_cap,
        );
        x += handle_block.width.min(handle_cap);
    }

    // `.x-middot-group > :not(:first-child)::before` is the whole separator:
    // `content: "·"` with `padding-inline: 4px`, so the `·` sits 4 px after the
    // handle and the timestamp 4 px after it — 8 px + the dot's advance in all.
    // (An extra `gap-1` here pushed the timestamp 4 px right of Chrome's; the
    // capture's header row puts `@NBA` at x=88.3125 w=43.9063 and its `15h` at
    // x=144.2813, i.e. exactly handle end + 12.0625.)
    push_text(out, &dot, x + GAP, centered(dot.height), dot.width);
    // The next box starts after the `::before`'s `padding-inline`, and Chrome
    // adds that padding up in LayoutUnits (the dot is 4.0625 there, 4.05 in
    // hand), so the advance is rounded up to the 1/64 the way the name clamp
    // does above.
    x += GAP + ceil64(dot.width) + GAP;

    push_text(out, &stamp_block, x, centered(stamp_block.height), avail(x));
    avatar_h
}

/// X's `Replying to @handle` row inside a quote: one `text-body` line in the
/// secondary color whose mention is `text-brand` (with `ss01`, which the text
/// engine applies to `@` runs already).
fn replying_row(cx: &Ctx, handle: &str, width: f32, y: f32, out: &mut Vec<Item>) -> f32 {
    let label = "Replying to ";
    let lead = paragraph(cx, label, BODY, 400, cx.p.secondary, width, Some(1));
    push_text(out, &lead, BORDER + PAD, y, width);
    let handle = format!("@{}", handle.trim_start_matches('@'));
    let x = BORDER + PAD + lead.width;
    let rest = (BORDER + PAD + width - x).max(0.0);
    let mention = paragraph(cx, &handle, BODY, 400, cx.p.brand, rest, Some(1));
    push_text(out, &mention, x, y, rest);
    lead.height.max(mention.height)
}

/// The reference instant for a quote's timestamp: the quote's own `now` when the
/// payload carries one, otherwise the post that quotes it.
fn clock<'a>(quote: &'a model::Post, outer_now: Option<&'a str>) -> Option<&'a str> {
    quote.now.as_deref().or(outer_now)
}

fn verified_icon(v: Verified) -> Option<&'static str> {
    match v {
        Verified::None => None,
        Verified::Blue => Some("icon-verified-premium-color"),
        Verified::Business => Some("icon-verified-business-color"),
        Verified::Government => Some("icon-verified-government-color"),
    }
}

/// The media layout names images as the outer post's (`ImageRef::Media(i)`);
/// inside a quote they are the quoted post's, so re-point them. Without this a
/// quoted video drew the outer post's poster once real bytes were present.
fn as_quoted(items: &mut [Item]) {
    for item in items {
        if let Item::Image { image, .. } = item {
            if matches!(image, ImageRef::Media(_) | ImageRef::MediaSourceAvatar(_)) {
                *image = ImageRef::Quote(Box::new(image.clone()));
            }
        }
    }
}

#[cfg(test)]
mod quoted_images {
    use super::*;

    #[test]
    fn quoted_media_points_at_the_quoted_post() {
        let shape = crate::display::Shape::rect(Rect::new(0.0, 0.0, 10.0, 10.0));
        let mut items = vec![
            Item::Image {
                shape,
                image: ImageRef::Media(0),
                focus: (0.5, 0.5),
                service: false,
            },
            Item::Image {
                shape,
                image: ImageRef::Quote(Box::new(ImageRef::Avatar)),
                focus: (0.5, 0.5),
                service: false,
            },
            // The "Post video" attribution avatar is the quoted post's too.
            Item::Image {
                shape,
                image: ImageRef::MediaSourceAvatar(1),
                focus: (0.5, 0.5),
                service: false,
            },
        ];
        as_quoted(&mut items);
        let refs: Vec<_> = items
            .iter()
            .map(|i| match i {
                Item::Image { image, .. } => image.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            refs,
            vec![
                ImageRef::Quote(Box::new(ImageRef::Media(0))),
                ImageRef::Quote(Box::new(ImageRef::Avatar)),
                ImageRef::Quote(Box::new(ImageRef::MediaSourceAvatar(1))),
            ]
        );
    }

    #[test]
    fn quoted_header_wraps_affiliate_badge_for_its_depth() {
        let opts = crate::model::RenderOptions::default();
        let cx = Ctx::new(&opts, 300.0);
        let post = model::Post {
            text: "hi".to_string(),
            author: model::Author {
                name: "Cursor".to_string(),
                handle: "cursor_ai".to_string(),
                verified: Verified::Business,
                square_avatar: true,
                affiliate: Some(model::Affiliate {
                    name: "Cursor".to_string(),
                    badge: Some("iVBORw==".to_string()),
                }),
                ..model::Author::default()
            },
            ..model::Post::default()
        };
        for (depth, want) in [
            (1u8, ImageRef::Quote(Box::new(ImageRef::AffiliateBadge))),
            (
                2u8,
                ImageRef::Quote(Box::new(ImageRef::Quote(Box::new(
                    ImageRef::AffiliateBadge,
                )))),
            ),
        ] {
            let mut items = Vec::new();
            header(&cx, &post, None, 24.0, 0.0, 0.0, 300.0, depth, &mut items);
            assert!(
                items
                    .iter()
                    .any(|i| matches!(i, Item::Image { image, .. } if *image == want)),
                "depth {depth} did not emit {want:?}: {items:?}"
            );
        }
    }
}

#[cfg(test)]
mod header_spacing {
    use super::*;
    use crate::model::{Author, Counts, Media, MediaKind, RenderOptions, Surface, Theme, Verified};

    /// The header row's middot group, against the capture's own boxes.
    ///
    /// `NBA.html`'s quoted video (the same post the `media-quote` fixture uses)
    /// renders, relative to the attachment slot at a 310 px slot:
    ///
    /// | box | x |
    /// |---|---|
    /// | name `NBA` | 33 |
    /// | handle `@NBA` (43.9063 wide) | 88.3125 |
    /// | `·` ink | 136.2188 |
    /// | timestamp `15h` | 144.2813 |
    ///
    /// i.e. `·` is 4 px after the handle and the timestamp 4 px after the dot —
    /// the `.x-middot-group > :not(:first-child)::before` block, whose
    /// `padding-inline: 4px` is the *whole* separator. An extra `gap-1` (what this
    /// used to have) pushed the timestamp to 148.28 and showed as a visibly wide
    /// `@NBA   · 15h` in the playground.
    #[test]
    fn the_timestamp_sits_one_middot_group_after_the_handle() {
        let outer = model::Post {
            text: "quoting".to_string(),
            author: Author {
                name: "Tony".to_string(),
                handle: "tony".to_string(),
                ..Author::default()
            },
            ..model::Post::default()
        };
        let opts = RenderOptions {
            width: 390.0,
            theme: Theme::Light,
            dpr: 2.0,
            surface: Surface::WebTimeline,
            font_scale: 1.0,
            expanded: false,
        };
        let now = "2026-11-10T12:00:00.000Z";
        let post = model::Post {
            text: "Jimmy Butler III is wearing the same outfit as the @warriors coaches"
                .to_string(),
            author: Author {
                name: "NBA".to_string(),
                handle: "NBA".to_string(),
                verified: Verified::Business,
                ..Author::default()
            },
            created_at: Some("2026-11-10T10:00:00.000Z".to_string()),
            now: Some(now.to_string()),
            media: vec![Media {
                kind: MediaKind::Video,
                width: 864,
                height: 1080,
                ..Media::default()
            }],
            counts: Some(Counts::default()),
            ..model::Post::default()
        };
        let mut items = Vec::new();
        let cx = Ctx::new(&opts, 310.0);
        layout(&cx, &outer, &post, &opts, true, 0.0, &mut items);

        // The header line's runs, left to right: name, handle, `·`, timestamp.
        let mut xs: Vec<f32> = items
            .iter()
            .filter_map(|i| match i {
                Item::Glyphs { x, y, glyphs, .. } if *y < 40.0 && !glyphs.is_empty() => {
                    Some(*x + glyphs[0].x)
                }
                _ => None,
            })
            .collect();
        xs.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        assert_eq!(xs.len(), 4, "name, handle, `·`, timestamp: {xs:?}");
        for (got, want) in xs.iter().zip([33.0, 88.3125, 136.2188, 144.2813]) {
            assert!(
                (got - want).abs() <= 1.0 / 64.0 + 1e-4,
                "run at {got:.4}, chrome has {want}"
            );
        }
    }
}

#[cfg(test)]
mod clock_and_label {
    use super::*;
    use crate::cards::time_label;
    use crate::model::Author;

    fn quote(created: &str, now: Option<&str>) -> model::Post {
        model::Post {
            text: "quoted post".to_string(),
            author: Author {
                name: "Celeste".to_string(),
                handle: "celestepoasts".to_string(),
                ..Author::default()
            },
            created_at: Some(created.to_string()),
            now: now.map(str::to_string),
            ..model::Post::default()
        }
    }

    /// A quoted payload carries `createdAt` but no clock of its own, so the
    /// reference instant is the *outer* post's `now`. (Without it every quote
    /// printed X's "Now" label — the same bug pg-oracle found in the replica.)
    #[test]
    fn a_quote_without_a_clock_uses_the_outer_posts_now() {
        let outer = Some("2026-11-11T12:00:00Z");
        let q = quote("2026-11-11T09:00:00Z", None);
        assert_eq!(time_label(q.created_at.as_deref(), clock(&q, outer)), "3h");
        // The instant keeps its offset, and the day label follows it.
        let west = quote("2026-11-04T18:12:00-08:00", None);
        assert_eq!(
            time_label(west.created_at.as_deref(), clock(&west, outer)),
            "Nov 4"
        );
        let utc = quote("2026-11-05T02:12:00Z", None);
        assert_eq!(
            time_label(utc.created_at.as_deref(), clock(&utc, outer)),
            "Nov 5"
        );
        // A quote that does carry one keeps it.
        let own = quote("2026-11-11T09:00:00Z", Some("2026-11-11T10:00:00Z"));
        assert_eq!(
            time_label(own.created_at.as_deref(), clock(&own, outer)),
            "1h"
        );
    }
}

// ------------------------------------------------------- the nested preview --
//
// X renders a *second* level of quoting: when the quoted post (q1) itself
// quotes a post (q2), the quote card ends with q2 in a compact card of its own
// (`assets/vendor/upstream/js/dfWSwB9n.js`, the i18n module
// `nested-quote-preview-SIs_tkfq`; the quote card that hosts it is
// `GH6kckO8.js`'s `Rt`, which puts the preview in
// `<div class="p-3 [:first-child+&]:pt-0">` after q1's own media).
//
// The preview's markup, from `ve` in that module:
//
// ```html
// <div class="-mx-1.5 -my-1 rounded-[9px] px-1.5 py-1">      <!-- hover box: net zero -->
//   <div class="flex">
//     <div aria-hidden="true" class="bg-tertiary w-[3px] shrink-0 rounded-full"></div>
//     <div class="flex min-w-0 flex-1 flex-col gap-2 py-1 ps-3">
//       <div class="flex min-w-0 items-center gap-1">       <!-- header: xs avatar + name + · + time -->
//       ...
//       </div>
//       <div class="flex min-w-0 items-center gap-2">        <!-- only when q2 has media -->
//         <div class="border-normal bg-secondary relative size-16 shrink-0 overflow-hidden rounded-sm border">
//           <img class="h-full w-full object-cover">
//           <div class="pointer-events-none absolute inset-0 flex items-center justify-center bg-black/40">
//             <span class="text-white font-bold text-subtext1">+2</span>   <!-- when q2 has >1 media -->
//           </div>                                                       <!-- else, for a video: -->
//           <div class="pointer-events-none absolute inset-0 flex items-center justify-center">
//             <span class="flex size-6 items-center justify-center rounded-full bg-black/60 text-white">
//               <svg class="size-3.5">…play…</svg>
//             </span>
//           </div>
//         </div>
//         <div class="min-w-0 flex-1">…the 2-line text…</div>
//       </div>
//     </div>
//   </div>
// </div>
// ```
//
// The body is the text component at `size: "subtext1"` (14/16) with
// `numberOfLines: 2` and `renderEntity: render-entity-as-text` — mentions keep
// the primary text color here, they are not brand blue.

/// `w-[3px]` bar.
const NESTED_BAR: f32 = 3.0;
/// `size-16` thumbnail.
const NESTED_THUMB: f32 = 64.0;
/// `gap-2` between the thumbnail and the text.
const NESTED_THUMB_GAP: f32 = 8.0;
/// `py-1` on the column.
const NESTED_PAD_Y: f32 = 4.0;
/// `gap-2` between the header row and the body.
const NESTED_GAP: f32 = 8.0;
/// `numberOfLines: 2`.
const NESTED_LINES: usize = 2;
/// `size: "xs"` avatar.
const NESTED_AVATAR: f32 = 16.0;

/// Lays out the nested quote preview and returns its height (including the
/// `p-3` wrapper's 12 px bottom padding; the top padding is `pt-0`, since the
/// preview always follows the quote's own header/text).
///
/// `content` is the quote frame's content width.
fn nested_preview(
    cx: &Ctx,
    q2: &model::Post,
    decisions: &crate::rules::Decisions,
    now: Option<&str>,
    content: f32,
    y0: f32,
    out: &mut Vec<Item>,
) -> f32 {
    // `p-3` wrapper: 12 px in from the frame on both sides, none at the top.
    let x0 = BORDER + PAD;
    let width = (content - 2.0 * PAD).max(1.0);
    let mut y = 0.0;

    // The 3 px bar spans the row; the row is as tall as the column.
    let bar_x = x0;
    let col_x = x0 + NESTED_BAR + PAD;
    let col_w = (width - NESTED_BAR - PAD).max(1.0);

    // Column: `py-1 ps-3 gap-2`.
    y += NESTED_PAD_Y;
    y += header(cx, q2, now, col_x, col_w, y0 + y, NESTED_AVATAR, 2, out);
    y += NESTED_GAP;

    // `render-entity-as-text`: an entity keeps the body's own color, it is not a
    // brand-colored link.
    let plain = decisions.nested.is_none_or(|n| n.plain_entities);
    let mention = if plain { cx.p.primary } else { cx.p.brand };
    let clamp = decisions.clamp.or(Some(NESTED_LINES));
    let body = |width: f32| {
        body_block(
            cx,
            &decisions.body,
            SUBTEXT1,
            400,
            cx.p.primary,
            mention,
            width,
            clamp,
        )
    };
    let thumbed = decisions
        .nested
        .map_or(!q2.media.is_empty(), |n| n.media_thumb);

    match q2.media.first().filter(|_| thumbed) {
        Some(first) => {
            let thumb = Rect::new(col_x, y0 + y, NESTED_THUMB, NESTED_THUMB);
            let thumb_shape = rounded(thumb, crate::tokens::RADIUS_SM_RADIUS);
            out.push(Item::PushClip { shape: thumb_shape });
            // `bg-secondary` is the bordered box itself, so it carries the radius;
            // the `<img>` fills the content box inside the 1 px border.
            super::fill(out, thumb_shape, cx.p.bg_secondary);
            let inner = Rect::new(thumb.x + 1.0, thumb.y + 1.0, thumb.w - 2.0, thumb.h - 2.0);
            out.push(Item::Image {
                shape: shape(inner, 0.0, CornerShape::Round),
                image: ImageRef::Quote(Box::new(ImageRef::Quote(Box::new(ImageRef::Media(0))))),
                focus: super::FOCUS,
                service: false,
            });
            let count = q2.media.len();
            if count > 1 {
                // `+{count-1}` over the whole thumbnail: `absolute inset-0` is the
                // content box, inside the 1 px border.
                super::fill(out, shape(inner, 0.0, CornerShape::Round), cx.p.black_40);
                let label = format!("+{}", count - 1);
                let block =
                    super::paragraph(cx, &label, SUBTEXT1, 700, cx.p.on_color, f32::MAX, Some(1));
                super::push_text(
                    out,
                    &block,
                    inner.x + (inner.w - block.width) * 0.5,
                    inner.y + (inner.h - block.height) * 0.5,
                    block.width,
                );
            } else if matches!(first.kind, model::MediaKind::Video) {
                let size = 24.0;
                let cx_ = inner.x + (inner.w - size) * 0.5;
                let cy = inner.y + (inner.h - size) * 0.5;
                super::fill(
                    out,
                    shape(
                        Rect::new(cx_, cy, size, size),
                        size * 0.5,
                        CornerShape::Round,
                    ),
                    cx.p.black_60,
                );
                let icon = 14.0;
                out.push(Item::Icon {
                    name: "icon-player-play".to_string(),
                    rect: Rect::new(
                        cx_ + (size - icon) * 0.5,
                        cy + (size - icon) * 0.5,
                        icon,
                        icon,
                    ),
                    color: cx.p.on_color,
                });
            }
            out.push(Item::PopClip);
            outline(out, thumb_shape, cx.p.border);

            // The text column is `items-center`: a short text is centred
            // against the 64 px thumbnail.
            let text_x = thumb.x + NESTED_THUMB + NESTED_THUMB_GAP;
            let text_w = (col_x + col_w - text_x).max(1.0);
            y += NESTED_THUMB;
            if !decisions.body.is_empty() {
                let block = body(text_w);
                let top = thumb.y + (NESTED_THUMB - block.height) * 0.5;
                push_text(out, &block, text_x, top, text_w);
            }
        }
        None => {
            if !decisions.body.is_empty() {
                let block = body(col_w);
                push_text(out, &block, col_x, y0 + y, col_w);
                y += block.height;
            }
        }
    }
    y += NESTED_PAD_Y;

    // The bar is full height (`stretch`), `rounded-full`.
    super::fill(
        out,
        super::rounded(
            Rect::new(bar_x, y0 + PAD * 0.0, NESTED_BAR, y),
            crate::tokens::RADIUS_FULL_RADIUS,
        ),
        cx.p.bg_tertiary,
    );
    // `p-3`'s bottom padding; `pt-0` because the preview always has a sibling.
    y + PAD
}

#[cfg(test)]
mod nested_preview_suppression {
    use super::*;
    use crate::model::{Author, Counts, LinkMeta, Media, MediaKind, RenderOptions, Surface, Theme};

    fn post(handle: &str, text: &str) -> model::Post {
        model::Post {
            text: text.to_string(),
            author: Author {
                name: "N".to_string(),
                handle: handle.to_string(),
                ..Author::default()
            },
            created_at: Some("2026-11-10T10:00:00.000Z".to_string()),
            now: Some("2026-11-10T12:00:00.000Z".to_string()),
            counts: Some(Counts::default()),
            ..model::Post::default()
        }
    }

    fn with_media(mut p: model::Post) -> model::Post {
        p.media = vec![Media {
            kind: MediaKind::Image,
            width: 1200,
            height: 900,
            ..Media::default()
        }];
        p
    }

    /// Does laying out `q1` (inside `outer`) draw a nested preview? The preview's
    /// images are the only `Quote(Quote(..))` refs, so their presence is the
    /// question — and the reason the suppression rule can be asserted from the
    /// display list rather than by re-testing `rules`.
    fn draws_preview(outer: &model::Post, q1: &model::Post) -> bool {
        let opts = RenderOptions {
            width: 390.0,
            theme: Theme::Light,
            dpr: 2.0,
            surface: Surface::WebTimeline,
            font_scale: 1.0,
            expanded: false,
        };
        let cx = Ctx::new(&opts, 310.0);
        let mut items = Vec::new();
        layout(&cx, outer, q1, &opts, false, 0.0, &mut items);
        items.iter().any(|item| {
            matches!(
                item,
                Item::Image { image: ImageRef::Quote(inner), .. }
                    if matches!(**inner, ImageRef::Quote(_))
            )
        })
    }

    /// `rules::decide_nested_quote` is X's `H`; this proves this module *obeys*
    /// it — an ineligible q2 is not drawn, and the eligible shapes are.
    #[test]
    fn the_preview_is_drawn_only_when_the_rules_allow_it() {
        let outer = post("tony", "the nesting is getting deep");
        let q2 = post("karpathy", "results for all claudes");
        let mut q1 = post("celeste", "Cool eval.");
        q1.quote = Some(Box::new(q2.clone()));
        assert!(draws_preview(&outer, &q1), "three authors, no attachments");

        // A self-quote at either hop (`outer == q1`, then `q1 == q2`).
        let mut self_quote = post("TONY", "the nesting is getting deep");
        self_quote.quote = Some(Box::new(q2.clone()));
        assert!(!draws_preview(&outer, &self_quote), "outer == q1");
        let mut same_author = post("celeste", "again");
        same_author.quote = Some(Box::new(post("CELESTE", "more")));
        assert!(!draws_preview(&outer, &same_author), "q1 == q2");

        // Two attachments on the card: `[outer, q1, q2].filter(renders).len() <= 1`.
        let mut medias = q1.clone();
        medias.quote = Some(Box::new(with_media(q2.clone())));
        assert!(
            !draws_preview(&with_media(outer.clone()), &medias),
            "the outer post and q2 both have media"
        );
        // One attachment is fine, wherever it sits.
        assert!(draws_preview(&outer, &medias), "q2 has the only media");

        // q2 must stay a plain post (`ie(q2)`).
        let mut carded = q2.clone();
        carded.links.insert(
            "https://e.com".to_string(),
            LinkMeta {
                card: Some("summary".to_string()),
                ..LinkMeta::default()
            },
        );
        let mut q1_with_card = q1.clone();
        q1_with_card.quote = Some(Box::new(carded));
        assert!(!draws_preview(&outer, &q1_with_card), "q2 has a card");

        // A crowded outer cell: a reply never shows the preview.
        let mut reply = outer.clone();
        reply.reply_to = Some("1".to_string());
        assert!(!draws_preview(&reply, &q1), "outer is a reply");
    }
}
