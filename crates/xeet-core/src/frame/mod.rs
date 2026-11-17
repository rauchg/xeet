//! The post frame: everything except the attachment.
//!
//! `layout` turns a `Post`, `RenderOptions` and the rules' `Decisions` into a
//! `display::DisplayList` and a `Report`, with every box where Chrome puts it.
//! The attachment (media, card, quote, poll) is laid out by `cards::layout` and
//! positioned in the slot this module reserves.
//!
//! # The geometry, from X's own markup
//!
//! Evidence: the server-rendered pages in `assets/vendor/corpus/` (public
//! posts, fetched logged out), X's stylesheet in `assets/vendor/upstream/`,
//! and `tools/oracle/replica.mjs`, which renders the real markup with our data
//! and dumps every box. The numbers below are all extracted into
//! `tokens.json` and reach this module through `crate::tokens`.
//!
//! ## The column
//!
//! A post sits in a column of `min(viewport, 600)` pixels (`max-w-[600px]` on
//! the timeline container), inside a cell with 16px horizontal and 12px
//! vertical padding (`px-horizontal`, `py-vertical`). So at width *w*:
//!
//! ```text
//! cell      = 0 .. w                       (this is the whole DisplayList)
//! content   = 16 .. w-16
//! avatar    = (16, 12) 40x40, rounded-full
//! text col  = (64 .. w-16)                avatar + 8px gap (`gap-2`)
//! ```
//!
//! The text column is therefore `w - 80`, which is exactly what X's own
//! `<img sizes="calc(100vw - 80px)">` says out loud.
//!
//! ## Timeline (`web-timeline`)
//!
//! ```text
//! header row  (64, 12, w-80, 20)          name ✓ @handle · 3h        [...]
//! text        (64, 34, w-80, lines*20)    2px below the header (`gap-0.5`)
//! attachment  (64, ..., w-80)             12px above (`gap-3`)
//! action row  (56, ..., w-64, 20)         12px above, -8px bleed (`-mx-2`)
//! ```
//!
//! ## Post detail (`web-detail`)
//!
//! ```text
//! avatar+header (16, 12, w-32, max(40, 40))   name over handle, two 20px rows
//! text          (16, ..., w-32, lines*24)     17/24 (`text-lead`), full width
//! attachment    (16, ..., w-32)               12px above
//! timestamp     (16, ..., w-32, 20)           "8:50 PM · Mar 21, 2006 · 12 Views"
//! action row    (8, ..., w-16, 20)            8px + 12px above (`gap-2` + `mt-3`)
//! ```
//!
//! The detail view's text starts at the cell's left edge, *under* the avatar:
//! its markup puts the text div outside the avatar row. That is the main
//! structural difference between the two surfaces, and the reason a post is 24
//! pixels wider on detail.
//!
//! ## Type
//!
//! Timeline text is `text-body` (15/20), detail text is `text-lead` (17/24),
//! names are `text-body font-bold`, handles `text-body` with `ss01`, the
//! middot and timestamp `text-body text-secondary`, counts `text-subtext2`
//! (13/16) with `tabular-nums`. All sizes multiply by X's font-scale setting.
//!
//! # What is verified, and how
//!
//! `examples/frame_parity.rs` lays out every fixture in
//! `tools/oracle/replica-fixtures.mjs` and compares the boxes and the text
//! lines against the replica's Chrome dump, at widths 320/390/600, light and
//! dark, both surfaces. The gate is 1/64px per box and an identical frame
//! height.
//!
//! Two things this module does *not* decide, because they are rules:
//! the line clamp (applied here, chosen there) and the inline "Show more"
//! link (part of `Decisions::body`, not synthesized here).

use crate::display::{DisplayList, ImageRef, Item, Radii, Rect, Shape};
use crate::inline::{self, BlockStyle, Dir, Span, TextBlock, Wrap};
use crate::model::{self, Post, RenderOptions, Surface};
use crate::tokens;

pub mod actions;
pub mod header;
pub mod numbers;
pub mod social;
pub mod time;

/// X's timeline column is `max-w-[var(--x-timeline-max-width,600px)]`.
pub const MAX_COLUMN: f32 = 600.0;

// The rules stage is another stream's module; `docs/INTERFACES.md` fixes the
// types, and frame only reads them.
pub use crate::rules::{AttachmentKind, BodySpan, Decisions, ShowMore, SpanKind};

// ---------------------------------------------------------------------------

/// Everything the parts of the frame need.
pub(crate) struct Ctx<'a> {
    pub post: &'a Post,
    pub opts: &'a RenderOptions,
    pub colors: tokens::Colors,
    /// X's user font-scale setting, sanitized to a positive finite number.
    pub scale: f32,
}

/// Native iOS frame metrics (measured on iPhone Duo `@3x` captures: `47.5 pt`
/// avatar at `x = 9 pt`, `8 pt` gap → `content_x = 64.5 pt`, `9 pt` right
/// margin → `308.5 pt` text column at `382 pt`; `15.5 / 20.67 pt` body type).
const IOS_PADDING_X_TIMELINE: f32 = 9.0;
const IOS_PADDING_X_DETAIL: f32 = 12.0;
const IOS_AVATAR_SIZE: f32 = 47.5;
const IOS_MAX_LINES: usize = 9;
const IOS_MAX_LINES_PLAIN: usize = 15;
const IOS_SHOW_MORE_RESERVE: f32 = 20.0;
const IOS_BODY: tokens::TypeStyle = tokens::TypeStyle {
    size: 15.5,
    line_height: 62.0 / 3.0,
};
const IOS_LEAD: tokens::TypeStyle = tokens::TypeStyle {
    size: 17.0,
    line_height: 23.0,
};

impl Ctx<'_> {
    /// Body text: 15/20 on web timeline, 17/24 on web detail, 15.5/20.67 on
    /// iOS timeline, 17/23 on iOS detail.
    pub(crate) fn body_style(&self) -> tokens::TypeStyle {
        match self.opts.surface {
            Surface::WebTimeline => tokens::BODY,
            Surface::WebDetail => tokens::LEAD,
            Surface::IosTimeline => IOS_BODY,
            Surface::IosDetail => IOS_LEAD,
        }
        .scaled(self.scale)
    }

    /// Header text: 15/20 on web, 15.5/20.67 on iOS.
    pub(crate) fn header_style(&self) -> tokens::TypeStyle {
        if self.opts.surface.is_ios() {
            IOS_BODY.scaled(self.scale)
        } else {
            tokens::BODY.scaled(self.scale)
        }
    }

    pub(crate) fn pad_x(&self) -> f32 {
        match self.opts.surface {
            Surface::IosTimeline => IOS_PADDING_X_TIMELINE,
            Surface::IosDetail => IOS_PADDING_X_DETAIL,
            Surface::WebTimeline | Surface::WebDetail => tokens::PADDING_X,
        }
    }

    pub(crate) fn avatar_size(&self) -> f32 {
        if self.opts.surface.is_ios() {
            IOS_AVATAR_SIZE
        } else {
            tokens::AVATAR_SIZE
        }
    }

    /// Counts and other small print.
    pub(crate) fn subtext2_style(&self) -> tokens::TypeStyle {
        tokens::SUBTEXT2.scaled(self.scale)
    }

    /// The relative timestamp the timeline shows.
    pub(crate) fn stamp_label(&self) -> String {
        if self.opts.surface.is_ios() {
            time::label_ios(
                self.post.created_at.as_deref(),
                self.post.now.as_deref(),
                false,
            )
        } else {
            time::label(
                self.post.created_at.as_deref(),
                self.post.now.as_deref(),
                false,
            )
        }
    }
}

fn clamp_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale.clamp(0.5, 3.0)
    } else {
        1.0
    }
}

/// Named boxes and text lines, for the parity harness and for debugging.
///
/// This is not part of the pixel pipeline: `layout` fills it as it goes so a
/// comparison against Chrome measures the same numbers that produced the
/// display list, not a second implementation of them. `layout` itself does
/// not need it, and leaves `enabled` false: everything here costs reads and
/// allocations (line fragments, a second layout of the name and handle) that
/// the pixel and report paths would throw away.
#[derive(Clone, Debug)]
pub struct Geometry {
    pub boxes: std::collections::BTreeMap<String, Rect>,
    pub lines: Vec<LineGeom>,
    enabled: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Geometry {
            boxes: std::collections::BTreeMap::new(),
            lines: Vec::new(),
            enabled: true,
        }
    }
}

/// One line box of one text block, in frame coordinates.
#[derive(Clone, Debug)]
pub struct LineGeom {
    /// Which block: "text", "name" or "handle".
    pub block: &'static str,
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Geometry {
    fn put(&mut self, name: &str, rect: Rect) {
        if !self.enabled {
            return;
        }
        self.boxes.insert(name.to_string(), rect);
    }
}

/// Lays out one post.
///
/// `decisions` comes from the rules stage; the attachment, if any, is laid out
/// by `cards::layout` inside the slot this function reserves.
pub fn layout(
    post: &Post,
    opts: &RenderOptions,
    decisions: &Decisions,
) -> (DisplayList, model::Report) {
    let (list, report, _) = layout_impl(post, opts, decisions, false);
    (list, report)
}

/// [`layout`] plus the named geometry, for oracle comparisons.
pub fn layout_debug(
    post: &Post,
    opts: &RenderOptions,
    decisions: &Decisions,
) -> (DisplayList, model::Report, Geometry) {
    let (list, report, geo) = layout_impl(post, opts, decisions, true);
    (list, report, geo)
}

fn layout_impl(
    post: &Post,
    opts: &RenderOptions,
    decisions: &Decisions,
    with_geometry: bool,
) -> (DisplayList, model::Report, Geometry) {
    let column = if opts.width.is_finite() {
        opts.width.clamp(1.0, MAX_COLUMN)
    } else {
        MAX_COLUMN
    };
    let ctx = Ctx {
        post,
        opts,
        colors: tokens::colors(opts.theme),
        scale: clamp_scale(opts.font_scale),
    };
    let mut list = DisplayList::new(column, 0.0, ctx.colors.bg_primary);
    let mut items: Vec<Item> = Vec::new();
    let mut geo = Geometry {
        enabled: with_geometry,
        ..Geometry::default()
    };

    // Body text, shared by every surface. On `ios-timeline`, long bodies clamp
    // at 9 visual rows with an inline `... Show more` tail.
    let (body, ios_clamped_visible) = body_block(&ctx, column, decisions);
    let body_present = !body.lines.is_empty();

    let height = match opts.surface {
        Surface::WebTimeline | Surface::IosTimeline => timeline(
            &ctx,
            column,
            decisions,
            &body,
            body_present,
            ios_clamped_visible.is_some(),
            &mut items,
            &mut geo,
        ),
        Surface::WebDetail | Surface::IosDetail => detail(
            &ctx,
            column,
            decisions,
            &body,
            body_present,
            &mut items,
            &mut geo,
        ),
    };

    let mut report = decisions.report.clone();
    report.size = model::Size {
        width: column,
        height,
    };
    report.text.lines = body.lines.len();
    if let Some(clamped_body) = ios_clamped_visible {
        report.text.truncated = true;
        report.text.show_more = Some("clamped".to_string());
        report.text.has_show_more = true;
        report.text.visible = format!("{clamped_body}\u{2026}");
    } else {
        report.text.truncated = body.truncated;
        let mut visible = body.visible_text().to_string();
        if body.truncated {
            visible.push('\u{2026}');
            // A clamp that cut lines is what "Show more" hangs off in a feed.
            report.text.show_more = Some("clamped".to_string());
        } else if let Some(ShowMore::Inline) = decisions.show_more {
            report.text.show_more = Some("inline".to_string());
        }
        report.text.has_show_more = report.text.show_more.is_some();
        report.text.visible = visible;
    }
    if report.verified_for.is_empty() {
        report.verified_for = vec![opts.surface];
    }
    let pad_x = ctx.pad_x();
    geo.put("cell", Rect::new(0.0, 0.0, column, height));
    geo.put(
        "article",
        Rect::new(
            pad_x,
            tokens::PADDING_Y,
            (column - 2.0 * pad_x).max(0.0),
            (height - 2.0 * tokens::PADDING_Y).max(0.0),
        ),
    );
    list.height = height;
    list.items = items;
    (list, report, geo)
}

/// `layout` with the rules stage run for you.
pub fn layout_post(post: &Post, opts: &RenderOptions) -> (DisplayList, model::Report) {
    layout(post, opts, &crate::rules::decide(post, opts))
}

/// The text column width for a column of `column` pixels.
fn content_width_for(ctx: &Ctx, column: f32) -> f32 {
    (column - 2.0 * ctx.pad_x()).max(1.0)
}

fn body_block(ctx: &Ctx, column: f32, decisions: &Decisions) -> (TextBlock, Option<String>) {
    let style = ctx.body_style();
    let spans: Vec<Span> = decisions
        .body
        .iter()
        .filter(|s| !s.text.is_empty())
        .map(|s| Span {
            text: s.text.clone(),
            color: if s.kind.is_entity() {
                ctx.colors.fg_brand
            } else {
                ctx.colors.fg_primary
            },
            weight: tokens::WEIGHT_NORMAL,
            ss01: false,
            tnum: false,
            atomic: false,
            role: s.kind.role(),
        })
        .collect();
    let content = content_width_for(ctx, column);
    let width = if ctx.opts.surface.is_detail() {
        content
    } else {
        content - ctx.avatar_size() - tokens::AVATAR_GAP
    }
    .max(1.0);
    let block_style = BlockStyle {
        size: style.size,
        line_height: style.line_height,
        weight: tokens::WEIGHT_NORMAL,
        width,
        max_lines: decisions.clamp,
        // X: `whitespace-pre-wrap wrap-anywhere` and `dir="auto"`.
        wrap: Wrap::Anywhere,
        preserve: true,
        dir: Dir::Auto,
        tab_size: 4.0,
    };
    let full = inline::layout(&spans, &block_style);
    let ios_cap = if decisions.attachment.is_some()
        || ctx.post.long_post == Some(true)
        || decisions.report.length.weighted > 280
    {
        IOS_MAX_LINES
    } else {
        IOS_MAX_LINES_PLAIN
    };
    if ctx.opts.surface == Surface::IosTimeline && !ctx.opts.expanded && full.lines.len() > ios_cap
    {
        if let Some((clamped, visible)) = ios_clamp_body(ctx, &full, width, ios_cap, &block_style) {
            return (clamped, Some(visible));
        }
    }
    (full, None)
}

/// Drops the last whitespace-delimited word from `s`, returning the trimmed
/// byte length before it (or `None` when `s` has a single word).
fn drop_last_word(s: &str) -> Option<usize> {
    let trimmed = s.trim_end();
    let ws = trimmed.rfind(char::is_whitespace)?;
    let word_start = trimmed[ws..]
        .chars()
        .next()
        .map_or(ws, |c| ws + c.len_utf8());
    let before = trimmed[..word_start].trim_end().len();
    (before > 0).then_some(before)
}

/// Native X iOS timeline clamp: keeps at most 9 visual rows of `full`, strips
/// trailing empty rows, drops a trailing word when the kept slice ended at a
/// hard newline so `...` visibly elides text, backs off words on the final
/// line until `last_line + "... Show more"` fits with trailing breathing room,
/// and appends inline `...` (`fg_primary`) + ` Show more` (`fg_brand`).
fn ios_clamp_body(
    ctx: &Ctx,
    full: &TextBlock,
    width: f32,
    cap: usize,
    block_style: &BlockStyle,
) -> Option<(TextBlock, String)> {
    let last_line = full.lines[..cap]
        .iter()
        .rfind(|l| !full.text[l.start..l.end].trim().is_empty())?;
    let line_start = last_line.start;
    let mut cut_end = line_start + full.text[line_start..last_line.end].trim_end().len();
    let ended_at_newline = full.text[cut_end..]
        .trim_start_matches(' ')
        .starts_with('\n');
    if ended_at_newline {
        if let Some(shorter) = drop_last_word(&full.text[line_start..cut_end]) {
            cut_end = line_start + shorter;
        }
    }
    let max_line_w = (width - IOS_SHOW_MORE_RESERVE * ctx.scale).max(40.0);
    let probe_style = BlockStyle {
        width: f32::MAX / 4.0,
        max_lines: Some(1),
        preserve: false,
        ..*block_style
    };
    loop {
        let candidate = format!("{}... Show more", &full.text[line_start..cut_end]);
        let probe = inline::layout(
            &[Span::new(
                candidate,
                ctx.colors.fg_primary,
                tokens::WEIGHT_NORMAL,
            )],
            &probe_style,
        );
        if probe.width <= max_line_w || cut_end <= line_start {
            break;
        }
        if let Some(shorter) = drop_last_word(&full.text[line_start..cut_end]) {
            cut_end = line_start + shorter;
        } else if let Some((prev, _)) = full.text[line_start..cut_end].char_indices().next_back() {
            if prev == 0 {
                break;
            }
            cut_end = line_start + prev;
        } else {
            break;
        }
    }
    let visible = full.text[..cut_end].to_string();
    let mut spans: Vec<Span> = full
        .spans
        .iter()
        .filter(|sr| sr.start < cut_end)
        .map(|sr| Span {
            text: full.text[sr.start..sr.end.min(cut_end)].to_string(),
            color: sr.color,
            weight: sr.weight,
            ss01: sr.ss01,
            tnum: sr.tnum,
            atomic: sr.atomic,
            role: sr.role,
        })
        .collect();
    spans.push(Span::new(
        "... ",
        ctx.colors.fg_primary,
        tokens::WEIGHT_NORMAL,
    ));
    spans.push(
        Span::new("Show more", ctx.colors.fg_brand, tokens::WEIGHT_NORMAL)
            .atomic()
            .role("show-more"),
    );
    let clamped = inline::layout(&spans, block_style);
    Some((clamped, visible))
}

/// X's logged-in timeline renders `[data-testid="tweet-text-show-more-link"]`
/// as a `display: block` `<button dir="ltr">` directly beneath
/// `[data-testid="tweetText"]` (`gap: 0`, `15/20` scaled, `font-weight: 400`,
/// `fg_brand` `#1d9bf0`), so "Show more" always sits on its own line.
fn show_more_block(ctx: &Ctx, width: f32) -> TextBlock {
    let style = ctx.body_style();
    inline::layout(
        &[Span {
            text: "Show more".to_string(),
            color: ctx.colors.fg_brand,
            weight: tokens::WEIGHT_NORMAL,
            ss01: false,
            tnum: false,
            atomic: true,
            role: "show-more",
        }],
        &BlockStyle {
            size: style.size,
            line_height: style.line_height,
            weight: tokens::WEIGHT_NORMAL,
            width: width.max(1.0),
            max_lines: Some(1),
            wrap: Wrap::Normal,
            preserve: false,
            dir: Dir::Ltr,
            tab_size: 4.0,
        },
    )
}

/// Body glyphs; clipped when a line clamp cut the text, as `line-clamp-N`
/// sets `overflow: hidden` (the captured markup puts it on quoted posts).
fn body_items(body: &TextBlock, x: f32, y: f32, width: f32) -> Vec<Item> {
    if body.truncated {
        header::clipped(body, x, y, width, body.height)
    } else {
        header::glyph_items(body, x, y)
    }
}

/// Line boxes of a text block, the way a DOM Range reports them.
///
/// Two details come from Chrome rather than from us: each *inline box* (an
/// entity link is one) is snapped up to 1/64 px before the line's width is
/// added up, and a right-to-left line is aligned against that snapped total.
/// `box_width` is the block's width, which right-to-left alignment needs.
fn record_lines(
    geo: &mut Geometry,
    block_name: &'static str,
    block: &TextBlock,
    x: f32,
    y: f32,
    box_width: f32,
) {
    if !geo.enabled {
        return;
    }
    let (ascent, descent) = crate::fonts::db()
        .primary(block.weight)
        .rounded_metrics(block.size);
    // Chrome floors the half-leading when it places an inline box: at 30/25 it
    // is 2, not 2.5 (`inline::make_line` does the same).
    let half =
        ((block.height / block.lines.len().max(1) as f32 - (ascent + descent)) / 2.0).floor();
    let fragments = block.fragments();
    for (index, line) in block.lines.iter().enumerate() {
        // Sum of the line's inline items, each snapped up to 1/64 px.
        let snapped: f32 = fragments
            .iter()
            .filter(|f| f.line == index)
            .map(|f| header::ceil64(f.w))
            .sum();
        // A blank line (a run of newlines) has no runs; the DOM still reports
        // a zero-width rect for it, so record it at the line's left edge.
        let left = line.runs.iter().map(|r| r.x).fold(f32::INFINITY, f32::min);
        let right = line
            .runs
            .iter()
            .map(|r| r.x + r.width)
            .fold(f32::NEG_INFINITY, f32::max);
        let (left, right) = if left.is_finite() {
            (left, right)
        } else {
            (0.0, 0.0)
        };
        let width = if line.runs.is_empty() { 0.0 } else { snapped };
        let line_x = if block.rtl && !line.runs.is_empty() {
            x + box_width - width
        } else {
            x + left
        };
        let _ = right;
        geo.lines.push(LineGeom {
            block: block_name,
            text: block.text[line.start..line.end].to_string(),
            x: line_x,
            y: y + line.top + half,
            w: width,
            h: ascent + descent,
        });
    }
}

/// The avatar's corner radius, as X's markup asks for it: a normal account is
/// `rounded-full` (20px on the 40px slot), a business/gold account is
/// `rounded-[10%]` (`.rounded-\[10\%\]{border-radius:10%}`, which is *not* in
/// the `@supports (corner-shape:squircle)` selector list, so it stays round and
/// resolves against the box: 4px on 40px). `cards::quote` uses the same 10% for
/// the smaller quote avatar.
pub(crate) fn avatar_radius(square: bool, size: f32) -> f32 {
    if square {
        size * 0.1
    } else {
        size / 2.0
    }
}

fn avatar_item(ctx: &Ctx, x: f32, y: f32) -> Item {
    let size = ctx.avatar_size();
    let radius = avatar_radius(ctx.post.author.square_avatar, size);
    let shape = Shape {
        rect: Rect::new(x, y, size, size),
        radii: Radii::all(radius),
        corner: crate::display::CornerShape::Round,
    };
    // Always an image item: an avatar slot on x.com always has a picture, and
    // the renderer supplies X's default profile image when the post carries no
    // bytes (`render::default_avatar`). A placeholder here would paint a "V…"
    // label x.com never draws (reported by the e2e harness).
    Item::Image {
        shape,
        image: ImageRef::Avatar,
        focus: (0.5, 0.5),
        service: false,
    }
}

/// The attachment slot, or `None` when there is nothing to show.
fn attachment_block(ctx: &Ctx, width: f32, decisions: &Decisions) -> Option<crate::cards::Block> {
    let kind = decisions.attachment.as_ref()?;
    let converted = match kind {
        AttachmentKind::Card { url, card } => crate::cards::AttachmentKind::Card {
            url: url.clone(),
            card: card.clone(),
        },
        AttachmentKind::Media => crate::cards::AttachmentKind::Media,
        AttachmentKind::Quote => crate::cards::AttachmentKind::Quote,
        AttachmentKind::Poll => crate::cards::AttachmentKind::Poll,
    };
    let body_empty =
        decisions.body.iter().all(|s| s.text.trim().is_empty()) && decisions.show_more.is_none();
    let block = crate::cards::layout_with_body(ctx.post, ctx.opts, &converted, width, body_empty);
    if block.items.is_empty() || block.height <= 0.0 {
        return None;
    }
    Some(block)
}

/// Moves a block's items into place.
fn place(block: &crate::cards::Block, x: f32, y: f32, items: &mut Vec<Item>) {
    for mut item in block.items.clone() {
        translate(&mut item, x, y);
        items.push(item);
    }
}

fn translate(item: &mut Item, dx: f32, dy: f32) {
    let shift_shape = |shape: &mut Shape| {
        shape.rect.x += dx;
        shape.rect.y += dy;
    };
    let shift_rect = |rect: &mut Rect| {
        rect.x += dx;
        rect.y += dy;
    };
    match item {
        Item::Fill { shape, .. } | Item::Stroke { shape, .. } | Item::PushClip { shape } => {
            shift_shape(shape)
        }
        Item::Glyphs { x, y, .. } => {
            *x += dx;
            *y += dy;
        }
        Item::Icon { rect, .. } => shift_rect(rect),
        Item::Image { shape, .. } | Item::Placeholder { shape, .. } => shift_shape(shape),
        Item::PopClip => {}
    }
}

fn record_header(geo: &mut Geometry, head: &header::Header, head_y: f32, style: tokens::TypeStyle) {
    geo.put(
        "headerRow_count",
        Rect::new(0.0, head_y, head.row_w, head.height),
    );
    geo.put("nameBlock", head.name_block);
    geo.put("name", head.name_rect);
    if let Some(badge) = head.badge {
        geo.put("badge", badge);
        if let Some(affiliate) = head.affiliate {
            geo.put("affiliate", affiliate);
            geo.put("affiliateImage", affiliate);
        }
    }
    geo.put("handle", head.handle_rect);
    if let Some(group) = head.group_rect {
        geo.put("middotGroup", group);
    }
    if let Some(span) = head.stamp_span {
        geo.put("timestamp", span);
        if let Some(x) = head.stamp_x {
            let w = span.x + span.w - x;
            geo.put("timestampLink", Rect::new(x, span.y, w, style.line_height));
        }
    }
    geo.put("more", head.more_rect);
}

/// The timeline cell: avatar column beside a content column.
fn timeline(
    ctx: &Ctx,
    column: f32,
    decisions: &Decisions,
    body: &TextBlock,
    body_present: bool,
    ios_inline_show_more: bool,
    items: &mut Vec<Item>,
    geo: &mut Geometry,
) -> f32 {
    let pad_x = ctx.pad_x();
    let avatar_size = ctx.avatar_size();
    let content = content_width_for(ctx, column);
    let content_x = pad_x + avatar_size + tokens::AVATAR_GAP;
    let text_w = (content - avatar_size - tokens::AVATAR_GAP).max(1.0);
    let style = ctx.header_style();
    let mut y = tokens::PADDING_Y;

    // The cell's context row (`Pinned`), above everything else, one `gap-1`
    // above the avatar/header row (the article is `flex flex-col gap-1`).
    if let Some(row) = social::context_row(ctx, content_x, y, text_w) {
        geo.put("pinnedRow", row.row);
        geo.put("pinnedIcon", row.icon);
        geo.put("pinnedText", row.label);
        items.extend(row.items);
        y += row.height + tokens::GAP_TIGHT;
    }

    let head = header::layout(ctx, content_x, y, text_w, false);
    let header_h = head.height;
    items.push(avatar_item(ctx, pad_x, y));
    record_header(geo, &head, y, style);
    record_header_lines(geo, ctx, &head, style);
    items.extend(head.items);
    let article_top = y;
    y += header_h;

    let block_show_more = !ios_inline_show_more
        && (matches!(decisions.show_more, Some(ShowMore::Inline)) || body.truncated);
    if body_present || block_show_more {
        y += tokens::GAP_HALF; // `gap-0.5` between the header block and the text
        if body_present {
            items.extend(body_items(body, content_x, y, text_w));
            record_lines(geo, "text", body, content_x, y, text_w);
            if ios_inline_show_more && geo.enabled && !body.spans.is_empty() {
                let last = body.spans.len() - 1;
                for fragment in body.fragments().iter().filter(|f| f.span == last) {
                    let line = &body.lines[fragment.line];
                    geo.put(
                        "showMore",
                        Rect::new(
                            content_x + fragment.x,
                            y + line.top,
                            header::ceil64(fragment.w),
                            line.height,
                        ),
                    );
                }
            }
            geo.put("text", Rect::new(content_x, y, text_w, body.height));
            y += body.height;
        }
        if block_show_more {
            let sm = show_more_block(ctx, text_w);
            items.extend(header::glyph_items(&sm, content_x, y));
            record_lines(geo, "showMore", &sm, content_x, y, text_w);
            geo.put(
                "showMore",
                Rect::new(content_x, y, header::ceil64(sm.width), sm.height),
            );
            y += sm.height;
        }
    }

    if let Some(block) = attachment_block(ctx, text_w, decisions) {
        y += tokens::GAP_BLOCK;
        geo.put("attachment", Rect::new(content_x, y, text_w, block.height));
        place(&block, content_x, y, items);
        y += block.height;
    }

    // On web, the action row is `h-5` with `-mx-2` (8px wider on each side);
    // on iOS, the icons sit flush inside the text column.
    y += tokens::GAP_BLOCK;
    let bleed = if ctx.opts.surface.is_ios() {
        0.0
    } else {
        tokens::ACTION_ROW_BLEED
    };
    let row_x = content_x - bleed;
    let row_w = text_w + 2.0 * bleed;
    let row = actions::layout(ctx, row_x, y, row_w, content);
    geo.put("actionRow", Rect::new(row_x, y, row_w, row.height));
    record_actions(geo, &row);
    items.extend(row.items);
    let row_y = y;
    y += row.height;

    // Column boxes: the avatar column stretches to the article's height.
    let article_h = y - article_top;
    geo.put(
        "avatarColumn",
        Rect::new(pad_x, article_top, avatar_size, article_h),
    );
    geo.put(
        "avatar",
        Rect::new(pad_x, article_top, avatar_size, avatar_size),
    );
    geo.put(
        "avatarShape",
        Rect::new(pad_x, article_top, avatar_size, avatar_size),
    );
    geo.put(
        "avatarImage",
        Rect::new(pad_x, article_top, avatar_size, avatar_size),
    );
    geo.put(
        "contentColumn",
        Rect::new(content_x, article_top, text_w, article_h),
    );
    geo.put(
        "headerRow",
        Rect::new(content_x, article_top, text_w, header_h),
    );
    let _ = row_y;

    y + tokens::PADDING_Y
}

/// The post-detail page: avatar beside a two-row header, then full-width text.
fn detail(
    ctx: &Ctx,
    column: f32,
    decisions: &Decisions,
    body: &TextBlock,
    body_present: bool,
    items: &mut Vec<Item>,
    geo: &mut Geometry,
) -> f32 {
    let pad_x = ctx.pad_x();
    let avatar_size = ctx.avatar_size();
    let content = content_width_for(ctx, column);
    let content_x = pad_x;
    let header_x = content_x + avatar_size + tokens::AVATAR_GAP;
    let header_w = (content - avatar_size - tokens::AVATAR_GAP).max(1.0);
    let style = ctx.body_style();
    let mut y = tokens::PADDING_Y;
    let article_top = y;

    let head = header::layout(ctx, header_x, y, header_w, true);
    let row_h = head.height.max(avatar_size);
    items.push(avatar_item(ctx, content_x, y));
    record_header(geo, &head, y, style);
    record_header_lines(geo, ctx, &head, style);
    items.extend(head.items);
    y += row_h;

    if body_present {
        y += tokens::GAP_BLOCK;
        items.extend(body_items(body, content_x, y, content));
        record_lines(geo, "text", body, content_x, y, content);
        geo.put("text", Rect::new(content_x, y, content, body.height));
        y += body.height;
    }

    if let Some(block) = attachment_block(ctx, content, decisions) {
        y += tokens::GAP_BLOCK;
        geo.put("attachment", Rect::new(content_x, y, content, block.height));
        place(&block, content_x, y, items);
        y += block.height;
    }

    // The absolute timestamp, then the view count: one 20px row under the text.
    y += tokens::GAP_BLOCK;
    let stamp_style = ctx.header_style();
    let mut stamp_w: f32 = 0.0;
    let pieces = header::stamp_pieces(ctx);
    for (i, (text, offset)) in pieces.iter().enumerate() {
        let block = header::one_line(
            text,
            ctx.colors.fg_secondary,
            tokens::WEIGHT_NORMAL,
            stamp_style,
            Wrap::Normal,
            false,
            f32::MAX / 4.0,
        );
        let w = header::ceil64(block.width);
        if *text == "\u{b7}" {
            // The separator between the instant and the view count, like the
            // middot in the header: 4px on each side.
        } else if i == 0 {
            stamp_w = w;
        } else {
            geo.put(
                "timestampViews",
                Rect::new(content_x + offset, y, w, stamp_style.line_height),
            );
        }
        items.extend(header::glyph_items(&block, content_x + offset, y));
    }
    geo.put(
        "timestampRow",
        Rect::new(content_x, y, content, stamp_style.line_height),
    );
    geo.put(
        "timestamp",
        Rect::new(content_x, y, stamp_w, stamp_style.line_height),
    );
    geo.put(
        "timestampLink",
        Rect::new(content_x, y, stamp_w, stamp_style.line_height),
    );
    y += stamp_style.line_height;

    // `gap-2` on the outer column plus `mt-3` on the action row is 20px.
    y += tokens::GAP_ROW + tokens::GAP_BLOCK;
    let bleed = if ctx.opts.surface.is_ios() {
        0.0
    } else {
        tokens::ACTION_ROW_BLEED
    };
    let row_x = content_x - bleed;
    let row_w = content + 2.0 * bleed;
    let row = actions::layout(ctx, row_x, y, row_w, content);
    geo.put("actionRow", Rect::new(row_x, y, row_w, row.height));
    record_actions(geo, &row);
    items.extend(row.items);
    y += row.height;

    let article_h = y - article_top;
    geo.put(
        "avatarColumn",
        Rect::new(content_x, article_top, avatar_size, row_h),
    );
    geo.put(
        "avatar",
        Rect::new(content_x, article_top, avatar_size, avatar_size),
    );
    geo.put(
        "avatarShape",
        Rect::new(content_x, article_top, avatar_size, avatar_size),
    );
    geo.put(
        "avatarImage",
        Rect::new(content_x, article_top, avatar_size, avatar_size),
    );
    geo.put(
        "contentColumn",
        Rect::new(header_x, article_top, header_w, row_h),
    );
    geo.put(
        "headerRow",
        Rect::new(header_x, article_top, header_w, row_h),
    );
    let _ = article_h;

    y + tokens::PADDING_Y
}

/// The name and handle line boxes, as a DOM Range reports them. Chrome keeps
/// the layout of a clamped block's hidden lines, so only the first line of
/// each is meaningful; the parity harness compares first lines only.
fn record_header_lines(
    geo: &mut Geometry,
    ctx: &Ctx,
    head: &header::Header,
    _style: tokens::TypeStyle,
) {
    if !geo.enabled {
        return;
    }
    // The header is text-body on both surfaces, whatever the post text uses.
    let style = tokens::BODY.scaled(ctx.scale);
    if head.name_rect.w > 0.0 {
        let block = header::one_line(
            &ctx.post.author.name,
            ctx.colors.fg_primary,
            tokens::WEIGHT_BOLD,
            style,
            Wrap::BreakAll,
            false,
            head.name_rect.w,
        );
        record_lines(
            geo,
            "name",
            &block,
            head.name_rect.x,
            head.name_rect.y,
            head.name_rect.w,
        );
    }
    let handle = if ctx.post.author.handle.is_empty() {
        String::new()
    } else {
        format!("@{}", ctx.post.author.handle.trim_start_matches('@'))
    };
    if !handle.is_empty() && head.handle_rect.w > 0.0 {
        let block = header::one_line(
            &handle,
            ctx.colors.fg_secondary,
            tokens::WEIGHT_NORMAL,
            style,
            Wrap::Normal,
            true,
            head.handle_rect.w,
        );
        record_lines(
            geo,
            "handle",
            &block,
            head.handle_rect.x,
            head.handle_rect.y,
            head.handle_rect.w,
        );
    }
}

fn record_actions(geo: &mut Geometry, row: &actions::Actions) {
    for (name, rect) in &row.pills {
        let label = match *name {
            "reply" => "replyPill",
            "bookmark" => "bookmark",
            "share" => "share",
            other => {
                geo.put(&format!("{other}Pill"), *rect);
                continue;
            }
        };
        geo.put(label, *rect);
    }
    if let Some(rect) = row.icons.get("reply") {
        geo.put("replyIcon", *rect);
    }
    if let Some(rect) = row.counts.get("reply") {
        geo.put("replyCount", *rect);
    }
}

#[cfg(test)]
mod geometry_is_optional {
    //! `layout` skips the geometry the parity harness reads; that must change
    //! nothing about the display list or the report.

    use crate::model::{Author, Counts, Post, RenderOptions, Surface, Theme, Verified};

    #[test]
    fn layout_without_geometry_matches_layout_debug() {
        let long = "word ".repeat(120);
        let posts = vec![
            Post { text: "We just shipped the thing.".to_string(), ..Post::default() },
            Post {
                text: "Shipping @vercel with #ShipIt and $TSLA, read https://vercel.com/blog/the-thing".to_string(),
                author: Author { name: "Guillermo Rauch".to_string(), handle: "rauchg".to_string(), verified: Verified::Blue, ..Author::default() },
                counts: Some(Counts { replies: 12, reposts: 3400000, likes: 12345678, views: 987654, bookmarks: 4 }),
                ..Post::default()
            },
            Post { text: long.clone(), ..Post::default() },
            Post { text: "blank\n\nlines\n".to_string(), ..Post::default() },
            Post { text: "שלום עולם and mixed direction 2026".to_string(), ..Post::default() },
        ];
        let surfaces = [Surface::WebTimeline, Surface::WebDetail];
        for post in &posts {
            for surface in surfaces {
                for (width, scale) in [(320.0, 1.0), (390.0, 1.0), (600.0, 1.25), (240.0, 1.0)] {
                    let opts = RenderOptions {
                        surface,
                        width,
                        theme: Theme::Dark,
                        dpr: 2.0,
                        font_scale: scale,
                        expanded: false,
                    };
                    let decisions = crate::rules::decide(post, &opts);
                    let (plain, plain_report) = super::layout(post, &opts, &decisions);
                    let (debug, debug_report, geo) = super::layout_debug(post, &opts, &decisions);
                    let what = format!(
                        "{:?} at {width} x{scale}: {:?}",
                        surface,
                        &post.text[..post.text.len().min(24)]
                    );
                    assert_eq!(
                        format!("{plain:?}"),
                        format!("{debug:?}"),
                        "display list changed by geometry: {what}"
                    );
                    assert_eq!(
                        serde_json::to_string(&plain_report).unwrap(),
                        serde_json::to_string(&debug_report).unwrap(),
                        "report changed by geometry: {what}"
                    );
                    assert!(
                        !geo.boxes.is_empty(),
                        "the geometry itself must still be filled: {what}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod paint_probe {
    use crate::color::Rgba;
    use crate::display::{CornerShape, DisplayList, Item, Radii, Rect, Shape};

    #[test]
    fn affiliate_badge_reaches_the_canvas() {
        use crate::model::{
            Affiliate, Author, Counts, Post, RenderOptions, Surface, Theme, Verified,
        };
        let post = Post {
            text: "hello".to_string(),
            author: Author {
                name: "Acme".to_string(),
                handle: "acme".to_string(),
                verified: Verified::Business,
                affiliate: Some(Affiliate {
                    name: "Acme Labs".to_string(),
                    badge: None,
                }),
                ..Author::default()
            },
            counts: Some(Counts::default()),
            ..Post::default()
        };
        let opts = RenderOptions {
            surface: Surface::WebTimeline,
            width: 390.0,
            theme: Theme::Light,
            dpr: 2.0,
            font_scale: 1.0,
            expanded: false,
        };
        let (list, _, geo) = super::layout_debug(&post, &opts, &crate::rules::decide(&post, &opts));
        let aff = geo.boxes.get("affiliate").copied().expect("affiliate box");
        println!("affiliate rect {aff:?}, items {}", list.items.len());
        for (i, item) in list.items.iter().enumerate() {
            if let crate::display::Item::Fill { shape, .. } = item {
                println!("  fill {i} at {:?}", shape.rect);
            }
        }
        let pix = crate::paint::rasterize(&list, 2.0, &crate::paint::NoImages);
        let px = pix
            .pixel(
                ((aff.x + aff.w / 2.0) * 2.0) as u32,
                ((aff.y + aff.h / 2.0) * 2.0) as u32,
            )
            .expect("pixel");
        println!("affiliate center pixel {px:?}");
        assert_ne!(
            px,
            crate::color::Rgba::WHITE,
            "affiliate badge is invisible"
        );
    }

    #[test]
    fn small_squircle_fill_is_painted() {
        let mut list = DisplayList::new(40.0, 40.0, Rgba::WHITE);
        let rect = Rect::new(10.0, 10.0, 15.0, 15.0);
        let shape = Shape {
            rect,
            radii: Radii::all(7.2),
            corner: CornerShape::Squircle,
        };
        list.push(Item::Fill {
            shape,
            color: Rgba::rgb(0xf2, 0xf2, 0xf2),
        });
        let pix = crate::paint::rasterize(&list, 2.0, &crate::paint::NoImages);
        let center = pix.pixel(35, 35).expect("pixel");
        println!("center pixel = {:?}", center);
        assert_eq!(
            center,
            Rgba::rgb(0xf2, 0xf2, 0xf2),
            "squircle fill did not paint"
        );
    }
}

#[cfg(test)]
mod show_more_tests {
    use super::*;
    use crate::model::{Author, Post, RenderOptions, Surface, Theme};

    /// Lays out a long post on the timeline (the surface where the server cut
    /// applies) and returns the boxes.
    fn long_post(text: &str) -> Geometry {
        let post = Post {
            text: text.to_string(),
            author: Author {
                name: "Google DeepMind".into(),
                handle: "GoogleDeepMind".into(),
                ..Author::default()
            },
            long_post: Some(true),
            long_post_body: Some(text.to_string()),
            ..Post::default()
        };
        let opts = RenderOptions {
            surface: Surface::WebTimeline,
            width: 600.0,
            theme: Theme::Light,
            dpr: 2.0,
            font_scale: 1.0,
            expanded: false,
        };
        let decisions = crate::rules::decide(&post, &opts);
        assert_eq!(decisions.show_more, Some(ShowMore::Inline));
        layout_debug(&post, &opts, &decisions).2
    }

    /// On X's logged-in timeline, `[data-testid="tweet-text-show-more-link"]`
    /// is a `display: block` `<button>` (`77.703 × 20` at 15/20, `gap: 0`)
    /// directly beneath `[data-testid="tweetText"]`, even when the last line of
    /// the body is short (`karpathy/2116957135661945283`).
    #[test]
    fn sits_on_its_own_line_below_the_body() {
        for text in [
            "Introducing Gemini 4 Carbon, our latest frontier model. Carbon delivers improved performance across domains and sets a new state-of-the-art.",
            "Weekend hack: HackerTube. I was annoyed with YouTube's recommendations so I had Claude filter down 43M comments over 20 years of HN to 6,454 videos that keep being recommended.\n\n- Excellent",
        ] {
            let geo = long_post(text);
            let text_box = geo.boxes.get("text").expect("text box");
            let button = geo.boxes.get("showMore").expect("button box");
            assert_eq!(button.h, 20.0);
            assert!(
                (button.w - 77.703).abs() < 0.02,
                "button width {}",
                button.w
            );
            assert_eq!(button.x, text_box.x, "flush left with the text column");
            assert!(
                (button.y - (text_box.y + text_box.h)).abs() < 0.01,
                "sits directly below tweetText (gap 0): text {:?} button {:?}",
                text_box,
                button
            );
            let sm_line = geo
                .lines
                .iter()
                .find(|l| l.block == "showMore")
                .expect("showMore line");
            assert_eq!(sm_line.text.trim(), "Show more");
            // `LineGeom.y` is the glyph box (line box + half-leading, 1px at 15/20).
            assert!((sm_line.y - button.y - 1.0).abs() < 0.01);
        }
    }

    #[test]
    fn expanded_renders_the_full_note_without_show_more() {
        let full = format!(
            "{}\n\nAnd here is the rest of the post.",
            "word ".repeat(65)
        );
        let post = Post {
            text: full.clone(),
            author: Author {
                name: "Andrej Karpathy".into(),
                handle: "karpathy".into(),
                ..Author::default()
            },
            long_post: Some(true),
            long_post_body: Some("Cut body.".into()),
            ..Post::default()
        };
        let collapsed = RenderOptions {
            surface: Surface::WebTimeline,
            width: 600.0,
            ..RenderOptions::default()
        };
        let (_, r_col, geo_col) =
            layout_debug(&post, &collapsed, &crate::rules::decide(&post, &collapsed));
        assert!(r_col.text.has_show_more);
        assert_eq!(r_col.text.show_more.as_deref(), Some("inline"));
        assert!(geo_col.boxes.contains_key("showMore"));
        assert_eq!(r_col.text.visible, "Cut body.");

        let expanded = RenderOptions {
            expanded: true,
            ..collapsed
        };
        let (_, r_exp, geo_exp) =
            layout_debug(&post, &expanded, &crate::rules::decide(&post, &expanded));
        assert!(!r_exp.text.has_show_more);
        assert_eq!(r_exp.text.show_more, None);
        assert!(!geo_exp.boxes.contains_key("showMore"));
        assert_eq!(r_exp.text.visible, full);
        assert!(r_exp.size.height > r_col.size.height);
    }

    /// Four real iPhone Duo (`382 pt @3x`) captures: two `note_tweet`s
    /// (`@mihar_22`, `@karpathy`) and two `<= 280`-char multi-paragraph posts
    /// (`@bscholl`, `@GergelyOrosz`). All four clamp at 9 visual rows on
    /// `ios-timeline` with inline `... Show more`.
    #[test]
    fn ios_timeline_matches_real_duo_cutoffs() {
        let cases = [
            (
                "This is so genuinely cool. I love how it's been packaged up. So elegant @shadcn and @shuding! I'll be studying this for a while.\n\n1. createAgent is an \"agent bridge\" which gives a snapshot, act, and query tool. It allows agents to interact with the actual UI components directly\n\n2. Change callbacks know when the agent made the change.",
                Some("This is so genuinely cool. I love how it's been packaged up. So elegant @shadcn and @shuding! I'll be studying this for a while.\n\n1. createAgent is an \"agent bridge\" which gives a snapshot, act, and query tool. It allows agents to interact with the actual UI components directly"),
                "components... Show more",
            ),
            (
                "Sometimes the line between smashing success and abject failure is paper thin.\n\nStartups only die for one reason: the founder gave up. You can always chew more glass. Once, I thought I might have to buy Boom out of bankruptcy.\n\nChoose worthy missions. So you never give up.",
                None,
                "Choose worthy missions.... Show more",
            ),
            (
                "Weekend hack: HackerTube. I was annoyed with YouTube's recommendations so I had Claude filter down 43M comments over 20 years of HN to 6,454 videos that keep being recommended. *Significantly* better viewing experience, ~2 hours with Opus 5.5:\n\n- Excellent recs without any logins (state is kept in local browser session)\n- Thumbnail is a representative frame from middle of the video",
                Some("Weekend hack: HackerTube. I was annoyed with YouTube's recommendations so I had Claude filter down 43M comments over 20 years of HN to 6,454 videos that keep being recommended. *Significantly* better viewing experience, ~2 hours with Opus 5.5:"),
                "- Excellent recs without... Show more",
            ),
            (
                "Holy cow, there\u{2019}s a massive Microsoft outage going on. Outlook .com is down, globally, as I understand. Microsoft employees cannot send/receive email as well (plus everyone else using M365)\n\nThis is very bad given how important email is for comms\n\nPostmortem will interesting",
                None,
                "is for... Show more",
            ),
        ];
        let opts = RenderOptions {
            surface: Surface::IosTimeline,
            width: 382.0,
            dpr: 3.0,
            ..RenderOptions::default()
        };
        for (text, long_body, want_tail) in cases {
            let post = Post {
                text: text.to_string(),
                author: Author {
                    name: "Author".into(),
                    handle: "author".into(),
                    ..Author::default()
                },
                long_post: long_body.map(|_| true),
                long_post_body: long_body.map(str::to_string),
                quote: Some(Box::new(Post {
                    text: "quoted".into(),
                    author: Author {
                        name: "Quoted".into(),
                        handle: "quoted".into(),
                        ..Author::default()
                    },
                    ..Post::default()
                })),
                ..Post::default()
            };
            let (_, report, geo) = layout_debug(&post, &opts, &crate::rules::decide(&post, &opts));
            assert!(report.text.has_show_more, "{want_tail}");
            assert_eq!(report.text.show_more.as_deref(), Some("clamped"));
            assert!(geo.boxes.contains_key("showMore"));
            let last = geo
                .lines
                .iter()
                .rfind(|l| l.block == "text")
                .expect("last text line");
            assert_eq!(last.text.trim(), want_tail, "got {:?}", last.text);
        }
    }
}

#[cfg(test)]
mod avatar_tests {
    use super::avatar_radius;

    /// X: `<div class="x-avatar rounded-[10%] size-10">` for business/gold
    /// accounts, `rounded-full` for everyone else. `10%` of the 40px slot is
    /// 4px, and it is a round corner (`rounded-[10%]` is not in the squircle
    /// `@supports` list).
    #[test]
    fn square_avatairs_are_ten_percent_rounded() {
        assert_eq!(avatar_radius(true, 40.0), 4.0);
        assert_eq!(avatar_radius(false, 40.0), 20.0);
        // The quote's smaller avatar follows the same rule.
        assert_eq!(avatar_radius(true, 24.0), 2.4);
    }

    /// The shape the frame emits is `CornerShape::Round` for both, with the
    /// radius above (a sharp 90° square was the bug).
    #[test]
    fn avatar_shape_is_round_with_the_right_radius() {
        use crate::display::CornerShape;
        use crate::model::{Author, Post, RenderOptions, Surface, Theme};
        for (square, want) in [(true, 4.0_f32), (false, 20.0_f32)] {
            let post = Post {
                text: "hi".into(),
                author: Author {
                    name: "Vercel".into(),
                    handle: "vercel".into(),
                    square_avatar: square,
                    ..Author::default()
                },
                ..Post::default()
            };
            let opts = RenderOptions {
                surface: Surface::WebTimeline,
                width: 600.0,
                theme: Theme::Light,
                dpr: 1.0,
                font_scale: 1.0,
                expanded: false,
            };
            let (list, _report, geo) =
                super::layout_debug(&post, &opts, &crate::rules::decide(&post, &opts));
            let shape = list
                .items
                .iter()
                .find_map(|item| match item {
                    crate::display::Item::Image { shape, .. } if shape.rect.w == 40.0 => {
                        Some(shape)
                    }
                    _ => None,
                })
                .expect("an avatar image item");
            assert_eq!(shape.corner, CornerShape::Round);
            assert_eq!(shape.radii.tl, want, "square={square}");
            assert_eq!(shape.radii.br, want);
            let box_ = geo.boxes.get("avatarShape").expect("avatarShape");
            assert_eq!((box_.x, box_.y, box_.w, box_.h), (16.0, 12.0, 40.0, 40.0));
        }
    }
}

#[cfg(test)]
mod clamp_tests {
    use crate::color::Rgba;
    use crate::inline::{self, BlockStyle, Dir, Span, TextBlock, Wrap};

    /// A clamped block at 15/20, the body size on the timeline.
    fn clamped(text: &str, width: f32, max_lines: usize, dir: Dir) -> TextBlock {
        inline::layout(
            &[Span::new(text, Rgba::BLACK, 400)],
            &BlockStyle {
                size: 15.0,
                line_height: 20.0,
                weight: 400,
                width,
                max_lines: Some(max_lines),
                wrap: Wrap::Anywhere,
                preserve: true,
                dir,
                tab_size: 4.0,
            },
        )
    }

    /// The ellipsis is a glyph on the clamped line, so it is painted; its run
    /// is the last one and starts where the truncated content ends.
    fn ellipsis(line: &inline::Line) -> (f32, f32, usize) {
        let run = line.runs.last().expect("a clamped line has runs");
        (run.x, run.width, run.glyphs.len())
    }

    /// CSS `-webkit-line-clamp: 5` on four lines and a blank paragraph line
    /// puts a lone `…` on the fifth line (the quoted post the lead pointed at,
    /// status 2118152230260092940; `clamp-blank` in the replica).
    #[test]
    fn blank_last_line_gets_the_ellipsis_alone() {
        let block = clamped(
            "One line of text here.\nTwo line of text here.\nThree line of text here.\nFour line of text here.\n\nAnd here is a second paragraph that the clamp has to cut off.",
            240.0,
            5,
            Dir::Auto,
        );
        assert_eq!(block.lines.len(), 5);
        assert!(block.truncated);
        let last = &block.lines[4];
        assert_eq!(last.start, last.end, "the fifth line is the blank one");
        let (x, w, glyphs) = ellipsis(last);
        assert_eq!(glyphs, 1, "just the ellipsis glyph");
        assert_eq!(x, 0.0, "at the line's start");
        assert!((w - 11.04).abs() < 0.1, "ellipsis advance {w}");
        assert!((last.width - w).abs() < 0.01);
    }

    /// A space hanging at a soft break is *interior* once the ellipsis follows
    /// it, so it keeps its advance and the `…` sits one space further right
    /// (Chrome: `clamp-overflow` at 600 ends at 564px on both sides, 500px of
    /// content rather than 497).
    #[test]
    fn a_trailing_space_stays_in_front_of_the_ellipsis() {
        let text = "Here is a longer post that exists purely so the timeline has something to clamp, with enough words in it to run past three lines at every width we test.";
        let block = clamped(text, 240.0, 2, Dir::Auto);
        assert!(block.truncated);
        let last = &block.lines[1];
        let content = &text[last.start..last.end];
        assert!(
            content.ends_with(' ') || content.ends_with('…'),
            "line {content:?}"
        );
        let (x, w, _) = ellipsis(last);
        // The ellipsis starts after the content, spaces included.
        assert!(x > 0.0);
        assert!(x + w <= 240.0 + 0.01, "content + ellipsis fits the column");
        assert!((last.width - (x + w)).abs() < 0.01);
    }

    /// The truncation is at a grapheme boundary, so an emoji is never split and
    /// the ellipsis follows the last whole cluster.
    #[test]
    fn emoji_at_the_cut_is_whole() {
        let text = "one two three four five six 🎉🎉🎉 seven eight nine ten eleven twelve thirteen";
        let block = clamped(text, 150.0, 2, Dir::Auto);
        let last = &block.lines[1];
        let content = &text[last.start..last.end];
        assert!(content.contains('🎉'));
        let mut seen = 0;
        for g in last.runs.iter().flat_map(|r| r.glyphs.iter()) {
            let _ = g;
            seen += 1;
        }
        // Every emoji contributes one glyph here (Twemoji is a paint detail);
        // the cut never lands inside the cluster, so the text ends on a
        // complete emoji or after a whole grapheme, never with a lone surrogate
        // or a variation selector.
        assert!(!content.ends_with('\u{fe0f}'));
        assert!(seen >= last.runs.last().unwrap().glyphs.len());
    }

    /// A right-to-left clamped line puts the ellipsis at its visual end, the
    /// left edge (`clamp-rtl` in the replica: our ink starts within 0.5px of
    /// Chrome's at 320/390/600).
    #[test]
    fn rtl_ellipsis_is_at_the_visual_end() {
        let text = "مرحبا بالعالم هذا نص عربي طويل جدا يحتاج الى اسطر كثيرة للعرض على الشاشة ثم يقطع عند السطر الثاني";
        let block = clamped(text, 240.0, 2, Dir::Auto);
        assert!(block.rtl);
        let last = &block.lines[1];
        let (x, w, glyphs) = ellipsis(last);
        assert_eq!(glyphs, 1);
        // Leftmost run on the line.
        let leftmost = last.runs.iter().map(|r| r.x).fold(f32::INFINITY, f32::min);
        assert!(
            (x - leftmost).abs() < 0.01,
            "ellipsis x {x} vs leftmost {leftmost}"
        );
        assert!(x + w <= 240.0 + 0.01);
    }
}
