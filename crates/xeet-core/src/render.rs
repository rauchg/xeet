//! `rank`: the pipeline, from a `Post` to pixels and a report.
//!
//! ```text
//! Post + RenderOptions  (model.rs)
//!    ├─ xeet-text   parse(text)          entities, weighted length
//!    ├─ rules       decide(post, opts)   what is shown, hidden, truncated
//!    ├─ frame       layout(..)           DisplayList + Report  (calls cards)
//!    └─ paint       rasterize(..)        RGBA → PNG
//! ```
//!
//! This file is the only place that knows the order, the limits and the
//! budget. Every stage is a pure function, so the whole render is a pure
//! function: the same input gives the same bytes on every platform, which is
//! what the determinism gate in CI checks (PLAN.md §11).
//!
//! Three entry points, one per wrapper:
//!
//! - [`explain`] runs rules and layout but no rasterizer, and returns the
//!   report (PLAN.md §6). It is the cheap one: agents call it while drafting.
//! - [`render`] adds the raster and PNG encoding.
//! - [`sheet`] renders several viewports and themes into one labeled image.
//!
//! All three validate their input against [`Limits`] first, charge a
//! deterministic work budget as they go, and return an [`Error`] instead of
//! panicking on anything the caller can get wrong.

use std::fmt;

use crate::color::Rgba;
use crate::display;
use crate::display::{DisplayList, ImageRef, Item, Radii, Rect, Shape};
use crate::inline::{self, BlockStyle, Dir, Span, Wrap};
use crate::limits::{self, Budget, LimitError, Limits, Problem};
use crate::model::{self, Post, RenderOptions, Surface, Theme};

// ---------------------------------------------------------------------------
// The stages, wired.
//
// Post → rules::decide (what is shown, hidden, truncated) → frame::layout
// (which calls cards::layout for the attachment) → paint::rasterize → PNG.
// `lay_out` below is the only place the order is written down.
// ---------------------------------------------------------------------------
use crate::frame;
use crate::paint;
use crate::rules;

/// Everything that can go wrong. Nothing is a panic: the caller gets a code it
/// can branch on, and a message it can show a person.
#[derive(Clone, Debug)]
pub enum Error {
    /// The request is over a hard limit (PLAN.md §13).
    Limit(LimitError),
    /// The input is well-formed JSON but not a valid post.
    Invalid(Vec<Problem>),
    /// The input is not `Post` JSON at all.
    Input(limits::InputError),
    /// A stage failed for a reason the caller cannot fix, e.g. an image that
    /// claims to be a PNG but is not.
    Stage(String),
}

impl Error {
    /// `true` when the caller has to change the request.
    pub fn is_client_error(&self) -> bool {
        match self {
            Error::Limit(e) => e.is_client_error(),
            Error::Invalid(_) | Error::Input(_) => true,
            Error::Stage(_) => false,
        }
    }

    /// Suggested HTTP status for the wrappers.
    pub fn http_status(&self) -> u16 {
        match self {
            Error::Limit(e) if e.code == "work_budget_exceeded" => 503,
            Error::Invalid(_) | Error::Input(_) => 400,
            Error::Limit(_) => 413,
            Error::Stage(_) => 500,
        }
    }

    /// The error as the JSON body the wrappers return. Shape:
    ///
    /// ```json
    /// {"code":"text_too_long","message":"...","fields":[{"path":"post.text","message":"..."}]}
    /// ```
    ///
    /// `fields` lists every validation problem at once, so an agent can fix them
    /// in one round trip.
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "code": self.code(),
            "status": self.http_status(),
            "message": self.to_string(),
        });
        if let Error::Invalid(problems) = self {
            v["fields"] = problems
                .iter()
                .map(|p| serde_json::json!({ "path": p.path, "message": p.message }))
                .collect();
        }
        v
    }

    /// Stable machine-readable code, for the `error.code` field of a JSON body.
    pub fn code(&self) -> &str {
        match self {
            Error::Limit(e) => e.code,
            Error::Invalid(_) => "invalid_post",
            Error::Input(_) => "invalid_json",
            Error::Stage(_) => "render_failed",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Limit(e) => write!(f, "{e}"),
            Error::Invalid(problems) => {
                write!(f, "invalid post:")?;
                for p in problems {
                    write!(f, "\n  {p}")?;
                }
                Ok(())
            }
            Error::Input(e) => write!(f, "{e}"),
            Error::Stage(m) => write!(f, "render failed: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<LimitError> for Error {
    fn from(e: LimitError) -> Error {
        Error::Limit(e)
    }
}

impl From<Vec<Problem>> for Error {
    fn from(p: Vec<Problem>) -> Error {
        Error::Invalid(p)
    }
}

impl From<limits::InputError> for Error {
    fn from(e: limits::InputError) -> Error {
        Error::Input(e)
    }
}

// ---------------------------------------------------------------------------
// explain
// ---------------------------------------------------------------------------

/// The display list and report for a post, without rasterizing it.
///
/// This is the seam the raster oracle uses: it can dump the list, rasterize it
/// with `paint`, or turn it into SVG, and compare all three against Chrome
/// (PLAN.md §10, layer 1). It is also what `--svg` renders.
pub fn display_list(
    post: &Post,
    opts: &RenderOptions,
) -> Result<(DisplayList, model::Report), Error> {
    display_list_with(post, opts, &Limits::standard())
}

pub fn display_list_with(
    post: &Post,
    opts: &RenderOptions,
    limits: &Limits,
) -> Result<(DisplayList, model::Report), Error> {
    let (list, report, _) = plan(post, opts, limits)?;
    Ok((list, report))
}

/// Runs the decisions and the layout, and returns the report without touching
/// the rasterizer (PLAN.md §6). This is the path an agent calls in a loop.
pub fn explain(post: &Post, opts: &RenderOptions) -> Result<model::Report, Error> {
    explain_with(post, opts, &Limits::standard())
}

/// [`explain`] with an explicit limit policy.
pub fn explain_with(
    post: &Post,
    opts: &RenderOptions,
    limits: &Limits,
) -> Result<model::Report, Error> {
    let (_, report, _) = plan(post, opts, limits)?;
    Ok(report)
}

/// Whether `post` would render with a "Show more" affordance under `opts`
/// (evaluated in collapsed state, without rasterizing pixels).
pub fn has_show_more(post: &Post, opts: &RenderOptions) -> Result<bool, Error> {
    has_show_more_with(post, opts, &Limits::standard())
}

/// [`has_show_more`] with an explicit limit policy.
pub fn has_show_more_with(
    post: &Post,
    opts: &RenderOptions,
    limits: &Limits,
) -> Result<bool, Error> {
    let collapsed = RenderOptions {
        expanded: false,
        ..opts.clone()
    };
    let (_, report, _) = plan(post, &collapsed, limits)?;
    Ok(report.text.has_show_more)
}

// ---------------------------------------------------------------------------
// render
// ---------------------------------------------------------------------------

/// A raster plus its report: what callers that do not need PNG take
/// (PLAN.md §12, "callers that do not need PNG can take raw RGBA").
pub fn render_rgba(
    post: &Post,
    opts: &RenderOptions,
) -> Result<(paint::Pixmap, model::Report), Error> {
    render_rgba_with(post, opts, &Limits::standard())
}

pub fn render_rgba_with(
    post: &Post,
    opts: &RenderOptions,
    limits: &Limits,
) -> Result<(paint::Pixmap, model::Report), Error> {
    let (list, mut report, budget) = plan(post, opts, limits)?;
    let images = PostImages::for_list(post, limits, &list);
    let pixmap = rasterize_planned(&list, opts.dpr, &mut report, budget, &images, limits)?;
    Ok((pixmap, report))
}

/// Rasterizes one planned list, charging the work budget before it draws, and
/// folds the image notes into the report. Shared by every entry point so a
/// batch cannot be cheaper or laxer than a single render.
fn rasterize_planned(
    list: &DisplayList,
    dpr: f32,
    report: &mut model::Report,
    mut budget: Budget,
    images: &PostImages<'_>,
    limits: &Limits,
) -> Result<paint::Pixmap, Error> {
    // Charge the raster before doing it: the budget is the guard that keeps a
    // hostile request from pinning a CPU, so it has to be paid up front.
    let device_pixels = (f64::from(list.width) * f64::from(dpr)).ceil().max(0.0)
        * (f64::from(list.height) * f64::from(dpr)).ceil().max(0.0);
    budget.charge(device_pixels as u64 / 8, "rasterizing")?;
    // A card picture wider than its service box is resampled into the box
    // before it is sampled, and that pass walks the source, not the canvas:
    // charge it by source pixels, on the same scale.
    let mut source_pixels: u64 = 0;
    for item in &list.items {
        if let Item::Image {
            shape,
            image,
            service: true,
            ..
        } = item
        {
            let Some(side) = paint::service_box(shape.rect.w, dpr) else {
                continue;
            };
            if let Some(prepared) = images.prepared(image) {
                let d = prepared.decoded();
                if d.width > side || d.height > side {
                    source_pixels += u64::from(d.width) * u64::from(d.height);
                }
            }
        }
    }
    if source_pixels > 0 {
        budget.charge(source_pixels / 8, "resampling card pictures")?;
    }

    let pixmap = paint::rasterize(list, dpr, images);
    limits.check_output_pixels(pixmap.width, pixmap.height)?;
    report.warnings.extend(images.notes());
    Ok(pixmap)
}

/// Renders to PNG. The bytes are byte-identical for identical input on every
/// platform and every wrapper; the determinism gate depends on it.
pub fn render(post: &Post, opts: &RenderOptions) -> Result<(Vec<u8>, model::Report), Error> {
    render_with(post, opts, &Limits::standard())
}

pub fn render_with(
    post: &Post,
    opts: &RenderOptions,
    limits: &Limits,
) -> Result<(Vec<u8>, model::Report), Error> {
    let (pixmap, report) = render_rgba_with(post, opts, limits)?;
    let png = paint::encode_png(&pixmap);
    if png.is_empty() {
        return Err(Error::Stage("PNG encoding produced no bytes".to_string()));
    }
    Ok((png, report))
}

/// Renders the same post several times, sharing the decoded images.
///
/// Every option set gets its own plan, budget and warnings, exactly as
/// [`render`] would produce; the saving is that an image is base64-decoded and
/// decoded once, and its premultiplied copy and mip chain are reused, no matter
/// how many viewports, themes or dprs ask for it.
pub fn render_many(
    post: &Post,
    options: &[RenderOptions],
) -> Result<Vec<(Vec<u8>, model::Report)>, Error> {
    render_many_with(post, options, &Limits::standard())
}

/// [`render_many`] with an explicit limit policy.
pub fn render_many_with(
    post: &Post,
    options: &[RenderOptions],
    limits: &Limits,
) -> Result<Vec<(Vec<u8>, model::Report)>, Error> {
    // Plan every option set before drawing any of them: a request that is over
    // a limit fails whole rather than after half a batch of work.
    let mut planned = Vec::with_capacity(options.len());
    for opts in options {
        let (list, report, budget) = plan(post, opts, limits)?;
        planned.push((opts, list, report, budget));
    }
    let images = PostImages::for_lists(post, limits, planned.iter().map(|(_, list, _, _)| list));
    let mut out = Vec::with_capacity(planned.len());
    for (opts, list, mut report, budget) in planned {
        let pixmap = rasterize_planned(&list, opts.dpr, &mut report, budget, &images, limits)?;
        let png = paint::encode_png(&pixmap);
        if png.is_empty() {
            return Err(Error::Stage("PNG encoding produced no bytes".to_string()));
        }
        out.push((png, report));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// sheet
// ---------------------------------------------------------------------------

/// Renders a grid of viewports × themes into one labeled PNG: the artifact a
/// marketing review wants (PLAN.md §6).
///
/// The label above each cell is `<width> · <theme>`, drawn at X's 11/12
/// subtext size. Cells are top-aligned, so differing heights are easy to
/// compare. One raster, one PNG: the same bytes every time.
pub fn sheet(post: &Post, widths: &[f32], themes: &[Theme], dpr: f32) -> Result<Vec<u8>, Error> {
    sheet_with(post, widths, themes, dpr, &Limits::standard())
}

pub fn sheet_with(
    post: &Post,
    widths: &[f32],
    themes: &[Theme],
    dpr: f32,
    limits: &Limits,
) -> Result<Vec<u8>, Error> {
    if let Err(problems) = limits::validate_sheet(widths, themes, limits) {
        return Err(Error::Invalid(problems));
    }
    if !dpr.is_finite() || dpr <= 0.0 || dpr > 4.0 {
        return Err(Error::Limit(LimitError::new(
            "output_invalid",
            format!("dpr {dpr} is outside 1..=4"),
        )));
    }

    // Plan every cell first, so the canvas can be sized from real heights.
    let mut cells: Vec<(usize, usize, DisplayList, f32)> = Vec::new();
    let mut col_w: Vec<f32> = widths.to_vec();
    let mut row_h: Vec<f32> = vec![0.0; themes.len()];
    for (row, theme) in themes.iter().enumerate() {
        for (col, width) in widths.iter().enumerate() {
            let opts = RenderOptions {
                surface: Surface::WebTimeline,
                width: *width,
                theme: *theme,
                dpr,
                font_scale: 1.0,
                expanded: false,
            };
            let (list, _report, _) = plan(post, &opts, limits)?;
            col_w[col] = col_w[col].max(list.width);
            row_h[row] = row_h[row].max(list.height);
            cells.push((row, col, list, *width));
        }
    }

    const MARGIN: f32 = 16.0;
    const GAP: f32 = 16.0;
    const LABEL_H: f32 = 18.0;
    const BORDER: f32 = 1.0;
    let canvas_w =
        MARGIN * 2.0 + col_w.iter().sum::<f32>() + GAP * (widths.len().saturating_sub(1)) as f32;
    let canvas_h = MARGIN * 2.0 + row_h.iter().map(|h| h + LABEL_H + GAP).sum::<f32>() - GAP;
    if canvas_w > limits.max_sheet_dimension || canvas_h > limits.max_sheet_dimension {
        return Err(Error::Limit(LimitError::new(
            "sheet_too_large",
            format!(
                "the sheet would be {canvas_w}×{canvas_h} CSS pixels; the limit is {} per side",
                limits.max_sheet_dimension
            ),
        )));
    }
    limits.check_output(canvas_w, canvas_h, dpr)?;

    let label_color = Rgba::rgb(0x53, 0x64, 0x71);
    let mut out = DisplayList::new(canvas_w, canvas_h, Rgba::rgb(0xcf, 0xd3, 0xd8));
    let mut y = MARGIN;
    for (row, theme) in themes.iter().enumerate() {
        let mut x = MARGIN;
        for (col, width) in widths.iter().enumerate() {
            let label = format!("{} · {}", css_num(*width), theme_name(*theme));
            let block = inline::layout(
                &[Span::new(label, label_color, 400)],
                &BlockStyle {
                    size: 11.0,
                    line_height: 12.0,
                    weight: 400,
                    width: *width,
                    max_lines: Some(1),
                    wrap: Wrap::Normal,
                    preserve: false,
                    dir: Dir::Auto,
                    tab_size: 4.0,
                },
            );
            out.push_text(&block, x, y);

            // The cell's own background, then its items, translated into place.
            let top = y + LABEL_H;
            if let Some((_, _, list, _)) = cells.iter().find(|(r, c, _, _)| *r == row && *c == col)
            {
                let frame_rect = Rect::new(x, top, list.width, list.height);
                out.push(Item::Fill {
                    shape: Shape::rect(frame_rect),
                    color: list.background,
                });
                for item in &list.items {
                    out.push(item_at(item, x, top));
                }
                out.push(Item::Stroke {
                    shape: Shape {
                        rect: frame_rect,
                        radii: Radii::all(0.0),
                        corner: display_corner(),
                    },
                    color: Rgba::rgb(0x8b, 0x98, 0xa4),
                    width: BORDER,
                });
            }
            x += col_w[col] + GAP;
        }
        y += row_h[row] + LABEL_H + GAP;
    }

    let images = PostImages::for_list(post, limits, &out);
    let pixmap = paint::rasterize(&out, dpr, &images);
    let png = paint::encode_png(&pixmap);
    if png.is_empty() {
        return Err(Error::Stage("PNG encoding produced no bytes".to_string()));
    }
    Ok(png)
}

/// Formats a width the way CSS would print it: `390`, not `390.0`.
fn css_num(v: f32) -> String {
    if v.fract() == 0.0 && v.abs() < 1e7 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn display_corner() -> display::CornerShape {
    display::CornerShape::Round
}

fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::Light => "light",
        Theme::Dark => "dark",
    }
}

/// Copies one item `dx`, `dy` to the right and down, for composing a sheet.
fn item_at(item: &Item, dx: f32, dy: f32) -> Item {
    let at_rect = |r: Rect| Rect::new(r.x + dx, r.y + dy, r.w, r.h);
    let at_shape = |s: Shape| Shape {
        rect: at_rect(s.rect),
        radii: s.radii,
        corner: s.corner,
    };
    match item.clone() {
        Item::Fill { shape, color } => Item::Fill {
            shape: at_shape(shape),
            color,
        },
        Item::Stroke {
            shape,
            color,
            width,
        } => Item::Stroke {
            shape: at_shape(shape),
            color,
            width,
        },
        Item::Glyphs {
            x,
            y,
            face,
            size,
            color,
            glyphs,
        } => Item::Glyphs {
            x: x + dx,
            y: y + dy,
            face,
            size,
            color,
            glyphs,
        },
        Item::Icon { name, rect, color } => Item::Icon {
            name,
            rect: at_rect(rect),
            color,
        },
        Item::Image {
            shape,
            image,
            focus,
            service,
        } => Item::Image {
            shape: at_shape(shape),
            image,
            focus,
            service,
        },
        Item::Placeholder { shape, label } => Item::Placeholder {
            shape: at_shape(shape),
            label,
        },
        Item::PushClip { shape } => Item::PushClip {
            shape: at_shape(shape),
        },
        Item::PopClip => Item::PopClip,
    }
}

// ---------------------------------------------------------------------------
// The shared middle: validate → decide → lay out → check
// ---------------------------------------------------------------------------

/// Runs validation, the rules and the layout, and returns the display list,
/// the report and the remaining work budget. `explain`, `render` and `sheet`
/// all go through here, so they cannot disagree about what a post becomes.
fn plan(
    post: &Post,
    opts: &RenderOptions,
    limits: &Limits,
) -> Result<(DisplayList, model::Report, Budget), Error> {
    limits::validate(post, opts, limits)?;
    let mut budget = limits.budget_for(post, opts, 0.0);
    let chars = post.text.chars().count() as u64;
    budget.charge(chars.saturating_mul(4), "parsing and deciding")?;

    let (mut list, mut report) = lay_out(post, opts);
    budget.charge(64u64.saturating_mul(list.items.len() as u64), "laying out")?;

    // The pipeline, not the stage, owns the size fields: a stage that forgets
    // to fill them should not produce a report with a zero size.
    // Written as explicit positivity checks: NaN is "not a size", so it must
    // reach this error rather than a clamp.
    if list.width.is_nan() || list.height.is_nan() || list.width <= 0.0 || list.height <= 0.0 {
        return Err(Error::Stage(format!(
            "layout produced an empty frame ({}×{})",
            list.width, list.height
        )));
    }
    list.width = list.width.ceil();
    list.height = list.height.ceil();
    report.size = model::Size {
        width: list.width,
        height: list.height,
    };
    if report.ruleset.is_empty() {
        report.ruleset = ruleset_id();
    }
    if report.verified_for.is_empty() {
        report.verified_for = limits::default_surfaces();
    }
    limits.check_output(list.width, list.height, opts.dpr)?;
    budget.charge(
        16u64.saturating_mul((list.width * list.height) as u64),
        "layout area",
    )?;
    Ok((list, report, budget))
}

/// The one place the pipeline picks its stages: decisions first, then layout.
fn lay_out(post: &Post, opts: &RenderOptions) -> (DisplayList, model::Report) {
    let decisions = rules::decide(post, opts);
    frame::layout(post, opts, &decisions)
}

/// The frame's own line boxes, as it laid them out — not re-derived from the
/// report's text. This is what the geometry oracle compares against Chrome: a
/// line is a decision the frame made (where the affordance sits, how a block
/// broke), and re-laying the visible string elsewhere would measure a different
/// layout than the one we render.
pub fn frame_lines(post: &Post, opts: &RenderOptions) -> Vec<FrameLine> {
    let decisions = rules::decide(post, opts);
    let (_, _, geometry) = frame::layout_debug(post, opts, &decisions);
    geometry
        .lines
        .iter()
        .map(|l| FrameLine {
            block: l.block.to_string(),
            text: l.text.clone(),
            x: l.x,
            y: l.y,
            w: l.w,
        })
        .collect()
}

/// One line box of one text block, for the geometry oracle.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FrameLine {
    /// `"text"`, `"name"` or `"handle"`.
    pub block: String,
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// The ruleset id a report carries: `x-web@` plus the first eight characters
/// of the upstream app version this binary embedded (`assets/upstream.lock.json`,
/// via `build.rs`). PLAN.md §6 and §11 both key freshness off it.
pub fn ruleset_id() -> String {
    // The rules stage owns the id, because it owns the rules: it is
    // "x-web@" plus the upstream app version the ruleset was derived from.
    rules::spec::ruleset_id().to_string()
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// Decodes the post's base64 images on demand and hands them to the painter.
///
/// The images a display list references, decoded once, before rasterizing.
///
/// Two reasons for the order:
///
/// - **`explain` stays free.** Nothing is decoded unless a list is actually
///   rasterized, and only the images *this* list draws are decoded: a media
///   attachment that lost to a poll, or a card whose image is hidden, costs
///   nothing.
/// - **No copies.** The painter borrows through `ImageSource::prepared_ref`
///   (falling back to `rgba_ref`), so a decoded image (5.8 MB for a 1600×900
///   photo) is never cloned per draw call, and its premultiplied copy and mip
///   chain are built once here and reused by every draw and every list that
///   asks for it. The map is immutable from construction to the end of the
///   raster, which is also why this needs no `RefCell`.
///
/// One `PostImages` serves every cell of a sheet, so an image is decoded once
/// per request even when it appears at four viewport widths.
struct PostImages<'a> {
    post: &'a Post,
    limits: &'a Limits,
    decoded: Vec<(ImageRef, std::sync::Arc<paint::PreparedImage>)>,
    notes: Vec<String>,
}

impl<'a> PostImages<'a> {
    /// Decodes every visible image `list` draws. Order follows first appearance,
    /// so the notes read in reading order.
    fn for_list(post: &'a Post, limits: &'a Limits, list: &DisplayList) -> PostImages<'a> {
        Self::for_lists(post, limits, std::iter::once(list))
    }

    /// Decodes every visible image drawn across `lists`, once. Slides scrolled
    /// past a carousel's clip box (`intersect(shape.rect, visible).is_empty()`)
    /// never paint a pixel, so their JPEGs are skipped.
    fn for_lists<'l>(
        post: &'a Post,
        limits: &'a Limits,
        lists: impl IntoIterator<Item = &'l DisplayList>,
    ) -> PostImages<'a> {
        fn intersect(a: crate::display::Rect, b: crate::display::Rect) -> crate::display::Rect {
            let x0 = a.x.max(b.x);
            let y0 = a.y.max(b.y);
            let x1 = (a.x + a.w).min(b.x + b.w);
            let y1 = (a.y + a.h).min(b.y + b.h);
            crate::display::Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
        }

        let mut images = PostImages {
            post,
            limits,
            decoded: Vec::new(),
            notes: Vec::new(),
        };
        for list in lists {
            let canvas = crate::display::Rect::new(0.0, 0.0, list.width, list.height);
            let mut clips: Vec<crate::display::Rect> = Vec::new();
            for item in &list.items {
                match item {
                    Item::PushClip { shape } => {
                        let next = clips
                            .last()
                            .map_or(shape.rect, |prev| intersect(*prev, shape.rect));
                        clips.push(next);
                    }
                    Item::PopClip => {
                        clips.pop();
                    }
                    Item::Image { shape, image, .. } => {
                        let visible = clips.last().map_or(canvas, |c| intersect(*c, canvas));
                        let clipped = intersect(shape.rect, visible);
                        if !(clipped.w > 0.0 && clipped.h > 0.0) {
                            continue;
                        }
                        if images.decoded.iter().any(|(r, _)| r == image) {
                            continue;
                        }
                        if let Some(decoded) = images.decode(image) {
                            images.decoded.push((image.clone(), decoded));
                        }
                    }
                    _ => {}
                }
            }
        }
        images
    }

    fn notes(&self) -> Vec<String> {
        self.notes.clone()
    }

    /// The prepared image for a reference, by reference.
    ///
    /// An avatar with no usable bytes borrows X's default profile image instead,
    /// so the painter never draws the "V…" placeholder in an avatar slot. The
    /// decoded post image wins whenever there is one.
    fn prepared(&self, image: &ImageRef) -> Option<&paint::PreparedImage> {
        if let Some((_, prepared)) = self.decoded.iter().find(|(r, _)| r == image) {
            return Some(&**prepared);
        }
        if is_avatar(image) {
            return default_avatar();
        }
        None
    }

    /// The base64 payload for an image reference, if the caller supplied one.
    ///
    /// `ImageRef::Quote` nests once per level of quoting, and the post tree
    /// nests with it: `Quote(Quote(Media(0)))` is the second-quoted post's first
    /// image. So walk both at the same time — down the `quote` links of the
    /// model and through the `Quote` wrappers of the reference — and resolve the
    /// leaf against whichever `Post` the walk stopped at. A reference deeper than
    /// the model (a renderer bug) returns `None` rather than panicking.
    fn payload(&self, image: &ImageRef) -> Option<&'a str> {
        let mut post: &'a Post = self.post;
        let mut node: &ImageRef = image;
        while let ImageRef::Quote(inner) = node {
            post = post.quote.as_deref()?;
            node = inner;
        }
        match node {
            ImageRef::Avatar => post.author.avatar.as_deref(),
            ImageRef::AffiliateBadge => post
                .author
                .affiliate
                .as_ref()
                .and_then(|a| a.badge.as_deref()),
            ImageRef::Media(i) => post.media.get(*i).and_then(|m| m.data.as_deref()),
            ImageRef::MediaSourceAvatar(i) => post
                .media
                .get(*i)
                .and_then(|m| m.source_author.as_ref())
                .and_then(|a| a.avatar.as_deref()),
            ImageRef::Card(url) => post
                .links
                .get(url)
                .and_then(|l| l.image.as_ref())
                .and_then(|i| i.data.as_deref()),
            // Unreachable: the loop above consumes every `Quote` wrapper.
            ImageRef::Quote(_) => None,
        }
    }

    /// What a failed avatar becomes: X's default image, not a placeholder.
    fn fallback_word(image: &ImageRef) -> &'static str {
        if is_avatar(image) {
            "drawing X's default avatar"
        } else {
            "drawing a placeholder"
        }
    }

    fn describe(image: &ImageRef) -> String {
        let mut depth = 0;
        let mut node = image;
        while let ImageRef::Quote(inner) = node {
            depth += 1;
            node = inner;
        }
        let prefix = match depth {
            0 => "",
            1 => "the quoted post's ",
            2 => "the twice-quoted post's ",
            _ => "a deeply quoted post's ",
        };
        let what = match node {
            ImageRef::Avatar => "avatar".to_string(),
            ImageRef::AffiliateBadge => "affiliation badge".to_string(),
            ImageRef::Media(i) => format!("media[{i}]"),
            ImageRef::MediaSourceAvatar(i) => format!("media[{i}] source avatar"),
            ImageRef::Card(url) => format!("card image for {url}"),
            ImageRef::Quote(_) => "image".to_string(),
        };
        format!("{prefix}{what}")
    }

    fn decode(&mut self, image: &ImageRef) -> Option<std::sync::Arc<paint::PreparedImage>> {
        let payload = self.payload(image)?;
        let what = PostImages::describe(image);
        let limits = paint_limits(self.limits);
        // The cache is keyed by the payload bytes and the decode policy, so a
        // second list — another viewport, another theme, another request in the
        // process — reuses the decode, its premultiplied copy and its mip chain.
        let key = ImageKey::new(payload, limits);
        if let Some(hit) = image_cache_get(&key) {
            return Some(hit);
        }
        let bytes = match base64_decode(payload) {
            Ok(b) => b,
            Err(e) => {
                self.notes.push(format!(
                    "{what} is not valid base64 ({e}); {}",
                    PostImages::fallback_word(image)
                ));
                return None;
            }
        };
        if let Err(e) = self.limits.check_image_bytes(bytes.len(), &what) {
            self.notes
                .push(format!("{e}; {}", PostImages::fallback_word(image)));
            return None;
        }
        match paint::decode_image(&bytes, &limits) {
            Ok(img) => {
                let prepared = std::sync::Arc::new(paint::PreparedImage::new(img));
                image_cache_put(key, prepared.clone());
                Some(prepared)
            }
            Err(e) => {
                self.notes.push(format!(
                    "{what} could not be decoded ({e}); {}",
                    PostImages::fallback_word(image)
                ));
                None
            }
        }
    }
}

impl paint::ImageSource for PostImages<'_> {
    /// The fast path the painter uses: the prepared image, by reference.
    fn prepared_ref(&self, image: &ImageRef) -> Option<&paint::PreparedImage> {
        self.prepared(image)
    }

    fn rgba_ref(&self, image: &ImageRef) -> Option<&paint::DecodedImage> {
        self.prepared(image).map(|p| p.decoded())
    }

    /// Only for callers that ask for ownership; the painter does not, so this
    /// clone is not on any render path.
    fn rgba(&self, image: &ImageRef) -> Option<paint::DecodedImage> {
        self.prepared(image).map(|p| p.decoded()).cloned()
    }
}

// ---------------------------------------------------------------------------
// The decoded-image cache
// ---------------------------------------------------------------------------

/// The most decoded image data the process keeps at once: enough for a handful
/// of photos (a 1600×900 source is 5.8 MB, its premultiplied copy and mip chain
/// about as much again), bounded so a stream of distinct renders cannot grow
/// without limit.
const IMAGE_CACHE_BYTES: usize = 96 * 1024 * 1024;

/// A decoded image's identity: a 128-bit hash of its payload plus the decode
/// policy that produced it. Two independent 64-bit mixes over 8-byte words make
/// a collision as unlikely as the payloads being equal is likely; the cache can
/// only ever hand back pixels that some earlier decode of those bytes produced,
/// and decoding is a pure function, so even a collision cannot change output.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ImageKey {
    h1: u64,
    h2: u64,
    len: usize,
    max_dimension: u32,
    max_pixels: u64,
    max_bytes: usize,
}

impl ImageKey {
    fn new(payload: &str, limits: paint::Limits) -> ImageKey {
        let bytes = payload.as_bytes();
        let (h1, h2) = hash128(bytes);
        ImageKey {
            h1,
            h2,
            len: bytes.len(),
            max_dimension: limits.max_dimension,
            max_pixels: limits.max_pixels,
            max_bytes: limits.max_bytes,
        }
    }
}

struct CacheEntry {
    image: std::sync::Arc<paint::PreparedImage>,
    bytes: usize,
    /// Tick of the last use, for eviction.
    used: u64,
}

struct ImageCache {
    entries: std::collections::HashMap<ImageKey, CacheEntry>,
    bytes: usize,
    tick: u64,
}

fn image_cache() -> &'static std::sync::Mutex<ImageCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<ImageCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(ImageCache {
            entries: std::collections::HashMap::new(),
            bytes: 0,
            tick: 0,
        })
    })
}

fn image_cache_get(key: &ImageKey) -> Option<std::sync::Arc<paint::PreparedImage>> {
    let mut cache = image_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.tick += 1;
    let tick = cache.tick;
    let entry = cache.entries.get_mut(key)?;
    entry.used = tick;
    Some(entry.image.clone())
}

fn image_cache_put(key: ImageKey, image: std::sync::Arc<paint::PreparedImage>) {
    let size = image.bytes();
    // An image bigger than the whole cache is drawn once and dropped, rather
    // than evicting everything else for it.
    if size > IMAGE_CACHE_BYTES {
        return;
    }
    let mut cache = image_cache().lock().unwrap_or_else(|e| e.into_inner());
    // Make room first, least recently used first, until the new image fits.
    while cache.bytes + size > IMAGE_CACHE_BYTES && !cache.entries.is_empty() {
        let victim = cache
            .entries
            .iter()
            .min_by_key(|(_, e)| e.used)
            .map(|(k, _)| *k);
        let Some(victim) = victim else {
            break;
        };
        if let Some(old) = cache.entries.remove(&victim) {
            cache.bytes = cache.bytes.saturating_sub(old.bytes);
        }
    }
    cache.tick += 1;
    let used = cache.tick;
    cache.bytes += size;
    // A concurrent insert of the same payload wins nothing either way; both
    // copies are the same pixels, and the replaced one is accounted for.
    if let Some(old) = cache.entries.insert(
        key,
        CacheEntry {
            image,
            bytes: size,
            used,
        },
    ) {
        cache.bytes = cache.bytes.saturating_sub(old.bytes);
    }
}

/// A 128-bit content hash, in two independently mixed 64-bit halves.
fn hash128(bytes: &[u8]) -> (u64, u64) {
    const PRIME_A: u64 = 0x9E37_79B9_7F4A_7C15;
    const PRIME_B: u64 = 0xC2B2_AE3D_27D4_EB4F;
    let mut a = 0xCBF2_9CE4_8422_2325u64;
    let mut b = 0x6A09_E667_F3BC_C909u64;
    for chunk in bytes.chunks(8) {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        let w = u64::from_le_bytes(word);
        a = mix64(a ^ w).wrapping_add(PRIME_A);
        b = mix64(b.rotate_left(17) ^ w.wrapping_mul(PRIME_B));
    }
    let len = bytes.len() as u64;
    (mix64(a ^ len), mix64(b ^ len.rotate_left(32)))
}

fn mix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x = x.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    x ^ (x >> 33)
}

/// X's default profile image, decoded once per process.
///
/// x.com always draws an image in the avatar slot: an account without an avatar
/// gets X's own default (the same file the web client loads from
/// `abs.twimg.com/sticky/default_profile_images/`). A post that carries no
/// `avatar` bytes — or whose bytes cannot be decoded — therefore draws this
/// rather than a labelled placeholder, on every target: the file is embedded at
/// build time (PLAN.md §13 keeps the cores free of I/O), so native, the HTTP
/// function and WASM all draw the same pixels.
///
/// Decoding happens once (`OnceLock`), not per request: it is the same image for
/// every post, and a render must not pay 2.8 kB of inflate for it.
fn default_avatar() -> Option<&'static paint::PreparedImage> {
    use crate::fonts::DEFAULT_AVATAR_PNG;
    static PREPARED: std::sync::OnceLock<Option<paint::PreparedImage>> = std::sync::OnceLock::new();
    PREPARED
        .get_or_init(|| {
            if DEFAULT_AVATAR_PNG.is_empty() {
                return None;
            }
            let limits = paint_limits(&Limits::standard());
            let decoded = paint::decode_image(DEFAULT_AVATAR_PNG, &limits).ok()?;
            // Held at the size an avatar is drawn at, not the size X ships: the
            // painter builds a mip pyramid per draw, and a 400×400 source costs
            // ~15× what a 128×128 one does (185 ms versus 12 ms for a 40 px
            // avatar in a debug build). 128 covers a 40 px slot at dpr 3, which
            // is the largest avatar either surface draws.
            Some(paint::PreparedImage::new(downscale(
                &decoded,
                DEFAULT_AVATAR_CACHE,
            )))
        })
        .as_ref()
}

/// The cached fallback's longest side, in pixels: `AVATAR_SIZE` × the largest
/// dpr the renderer accepts (3), rounded up to a power of two.
const DEFAULT_AVATAR_CACHE: u32 = 128;

/// Area-average `src` down so its longest side is at most `max`.
///
/// Separable and premultiplied: averaging straight RGBA would drag the colour of
/// mostly-transparent edge pixels toward the image's rgb, and X's default avatar
/// has antialiased edges.
fn downscale(src: &paint::DecodedImage, max: u32) -> paint::DecodedImage {
    let longest = src.width.max(src.height);
    if longest <= max || max == 0 {
        return src.clone();
    }
    let scale = f64::from(max) / f64::from(longest);
    let dw = ((f64::from(src.width) * scale).round() as u32).max(1);
    let dh = ((f64::from(src.height) * scale).round() as u32).max(1);

    // One row of source, premultiplied, is averaged into one row of the target
    // at a time; columns are averaged first so the vertical pass is cache-local.
    let (sw, sh) = (src.width as usize, src.height as usize);
    let premultiplied: Vec<[f32; 4]> = src
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = f32::from(p[3]) / 255.0;
            [
                f32::from(p[0]) * a,
                f32::from(p[1]) * a,
                f32::from(p[2]) * a,
                f32::from(p[3]),
            ]
        })
        .collect();

    // Horizontal pass: (dw × sh).
    let dwu = dw as usize;
    let mut mid = vec![[0f32; 4]; dwu * sh];
    for y in 0..sh {
        for (x, out) in mid[y * dwu..(y + 1) * dwu].iter_mut().enumerate() {
            let (lo, hi) = span(x, dw, sw);
            let mut acc = [0f32; 4];
            for sx in lo..hi {
                let p = premultiplied[y * sw + sx];
                for k in 0..4 {
                    acc[k] += p[k];
                }
            }
            let n = (hi - lo).max(1) as f32;
            *out = [acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n];
        }
    }

    // Vertical pass: (dw × dh), then back to straight RGBA8.
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    for y in 0..dh as usize {
        let (lo, hi) = span(y, dh, sh);
        let n = (hi - lo).max(1) as f32;
        for x in 0..dw as usize {
            let mut acc = [0f32; 4];
            for sy in lo..hi {
                let p = mid[sy * dwu + x];
                for k in 0..4 {
                    acc[k] += p[k];
                }
            }
            let a = acc[3] / n;
            let i = (y * dw as usize + x) * 4;
            let un = if a > 0.0 { 255.0 / a } else { 0.0 };
            out[i] = (acc[0] / n * un).clamp(0.0, 255.0) as u8;
            out[i + 1] = (acc[1] / n * un).clamp(0.0, 255.0) as u8;
            out[i + 2] = (acc[2] / n * un).clamp(0.0, 255.0) as u8;
            out[i + 3] = a.clamp(0.0, 255.0) as u8;
        }
    }
    paint::DecodedImage {
        width: dw,
        height: dh,
        rgba: out,
    }
}

/// The source range `[lo, hi)` that target index `i` of `n` covers in a source
/// axis of `src` pixels.
fn span(i: usize, n: u32, src: usize) -> (usize, usize) {
    let lo = (i as f64 * f64::from(src as u32) / f64::from(n)) as usize;
    let hi = (((i + 1) as f64 * f64::from(src as u32) / f64::from(n)).ceil()) as usize;
    (lo.min(src.saturating_sub(1)), hi.clamp(lo + 1, src))
}

/// Whether an image reference is an account's avatar (its own, a quoted post's,
/// or a twice-quoted post's) — the one slot that has a documented fallback on
/// x.com, so every depth has to reach it.
fn is_avatar(image: &ImageRef) -> bool {
    match image {
        ImageRef::Avatar | ImageRef::MediaSourceAvatar(_) => true,
        ImageRef::Quote(inner) => is_avatar(inner),
        _ => false,
    }
}

/// The painter carries its own, smaller decode policy (`paint::Limits`); this
/// is the single place the two are related, so the decoder cannot end up with
/// looser limits than the request policy (PLAN.md §13).
fn paint_limits(l: &Limits) -> paint::Limits {
    paint::Limits {
        max_dimension: l.max_image_dimension,
        max_pixels: l.max_image_pixels,
        max_bytes: l
            .max_image_bytes
            .saturating_mul(4)
            .min(usize::MAX as u64 as usize),
    }
}

/// Standard base64 (RFC 4648) with or without a `data:` prefix and padding.
/// Kept here rather than in `limits` so the parser can stay allocation-free.
fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    let data = s
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(',').map(|(_, d)| d))
        .unwrap_or(s);
    let data = data.as_bytes();
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let mut pos = 0usize;
    // Four alphabet bytes are three output bytes, so a clean chunk needs no
    // bit bookkeeping at all. Padding, whitespace, the tail and anything
    // invalid stop the fast loop and hand the rest to the byte-at-a-time loop
    // below, which owns the error messages and the offsets they quote.
    while pos + 4 <= data.len() {
        let v = [
            B64[data[pos] as usize],
            B64[data[pos + 1] as usize],
            B64[data[pos + 2] as usize],
            B64[data[pos + 3] as usize],
        ];
        if (v[0] | v[1] | v[2] | v[3]) >= 64 {
            break;
        }
        let n = (u32::from(v[0]) << 18)
            | (u32::from(v[1]) << 12)
            | (u32::from(v[2]) << 6)
            | u32::from(v[3]);
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
        pos += 4;
    }
    // A whole number of input groups leaves the bit accumulator empty, so the
    // slow loop starts from a clean state.
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut pad = 0;
    for (i, &b) in data.iter().enumerate().skip(pos) {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return Err(format!("unexpected byte {:#04x} at offset {i}", b)),
        };
        if pad > 0 {
            return Err("data after padding".to_string());
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// The alphabet as a byte → 6-bit value table; [`B64_INVALID`] marks a byte
/// that is not in it (padding, whitespace and anything else).
const B64_INVALID: u8 = 0xFF;

const B64: [u8; 256] = b64_table();

const fn b64_table() -> [u8; 256] {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [B64_INVALID; 256];
    let mut i = 0;
    while i < alphabet.len() {
        table[alphabet[i] as usize] = i as u8;
        i += 1;
    }
    table
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Parses and validates `Post` JSON in one step, with errors that name the
/// field. The CLI, the HTTP function and the WASM package all start here.
/// The second value is the parse notes (unknown or misspelled fields).
pub fn load_post(json: &str, opts: &RenderOptions, limits: &Limits) -> Result<Post, Error> {
    let post = limits::parse_post(json)?;
    limits::validate(&post, opts, limits)?;
    Ok(post)
}

/// Convenience for wrappers: JSON in, report JSON out.
pub fn explain_json(post_json: &str, opts_json: Option<&str>) -> Result<String, Error> {
    explain_json_with(post_json, opts_json, &Limits::standard())
}

pub fn explain_json_with(
    post_json: &str,
    opts_json: Option<&str>,
    limits: &Limits,
) -> Result<String, Error> {
    let opts = match opts_json {
        Some(j) if !j.trim().is_empty() => limits::parse_options(j)?,
        _ => RenderOptions::default(),
    };
    let post = load_post(post_json, &opts, limits)?;
    let report = explain_with(&post, &opts, limits)?;
    serde_json::to_string(&report)
        .map_err(|e| Error::Stage(format!("could not serialize the report: {e}")))
}

/// The SVG of the same display list the PNG would be rasterized from, for
/// review pages and diffing (PLAN.md §7: "display list → RGBA → PNG,
/// optionally SVG").
pub fn svg(post: &Post, opts: &RenderOptions) -> Result<(String, model::Report), Error> {
    svg_with(post, opts, &Limits::standard())
}

pub fn svg_with(
    post: &Post,
    opts: &RenderOptions,
    limits: &Limits,
) -> Result<(String, model::Report), Error> {
    let (list, report, _) = plan(post, opts, limits)?;
    Ok((paint::to_svg(&list), report))
}

/// Convenience for wrappers: JSON in, PNG bytes and report out.
pub fn render_json(
    post_json: &str,
    opts_json: Option<&str>,
) -> Result<(Vec<u8>, model::Report), Error> {
    render_json_with(post_json, opts_json, &Limits::standard())
}

pub fn render_json_with(
    post_json: &str,
    opts_json: Option<&str>,
    limits: &Limits,
) -> Result<(Vec<u8>, model::Report), Error> {
    let opts = match opts_json {
        Some(j) if !j.trim().is_empty() => limits::parse_options(j)?,
        _ => RenderOptions::default(),
    };
    let post = load_post(post_json, &opts, limits)?;
    let (png, report) = render_with(&post, &opts, limits)?;
    Ok((png, report))
}

/// Renders a sheet from JSON: `{"post": {...}, "widths": [320,390], "themes": ["light","dark"], "dpr": 2}`.
pub fn sheet_json(request_json: &str) -> Result<Vec<u8>, Error> {
    sheet_json_with(request_json, &Limits::standard())
}

pub fn sheet_json_with(request_json: &str, limits: &Limits) -> Result<Vec<u8>, Error> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    struct Request {
        post: Post,
        widths: Vec<f32>,
        themes: Vec<Theme>,
        dpr: f32,
    }
    impl Default for Request {
        fn default() -> Request {
            Request {
                post: Post::default(),
                widths: vec![320.0, 390.0, 600.0],
                themes: vec![Theme::Light, Theme::Dark],
                dpr: 2.0,
            }
        }
    }
    let req: Request = serde_json::from_str(request_json).map_err(|e| {
        Error::Input(limits::InputError {
            what: "sheet JSON",
            message: e.to_string(),
        })
    })?;
    sheet_with(&req.post, &req.widths, &req.themes, req.dpr, limits)
}

/// The `sheet` cell count a caller may ask for, for wrapper help text.
pub fn sheet_cells(widths: usize, themes: usize) -> usize {
    widths.saturating_mul(themes)
}

// ---------------------------------------------------------------------------
// Stubs. Delete each one as the real module lands; see the integration
// boundary at the top of this file.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Author;

    fn post(text: &str) -> Post {
        Post {
            text: text.to_string(),
            author: Author {
                name: "Vercel".into(),
                handle: "vercel".into(),
                ..Author::default()
            },
            ..Post::default()
        }
    }

    /// A one-item display list, so a test can decode one image without
    /// laying out a post.
    fn one_image_list<'a>(post: &'a Post, limits: &'a Limits, image: ImageRef) -> PostImages<'a> {
        let mut list = DisplayList::new(100.0, 100.0, Rgba::WHITE);
        list.push(Item::Image {
            shape: Shape::rect(Rect::new(0.0, 0.0, 100.0, 100.0)),
            image,
            focus: (0.5, 0.5),
            service: false,
        });
        PostImages::for_list(post, limits, &list)
    }

    fn opts() -> RenderOptions {
        RenderOptions {
            surface: Surface::WebTimeline,
            width: 390.0,
            theme: Theme::Light,
            dpr: 2.0,
            font_scale: 1.0,
            expanded: false,
        }
    }

    #[test]
    fn explain_reports_size_and_ruleset() {
        let r = explain(&post("We just shipped the thing."), &opts()).unwrap();
        assert!(r.size.width > 0.0 && r.size.height > 0.0);
        assert!(!r.ruleset.is_empty());
        assert_eq!(r.size.width, 390.0);
    }

    #[test]
    fn render_produces_a_png() {
        let (png, report) = render(&post("Hello"), &opts()).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(png.len() > 100);
        assert!(report.size.height > 0.0);
    }

    #[test]
    fn render_is_deterministic() {
        let p = post("Same bytes every time, on every platform.");
        let a = render(&p, &opts()).unwrap().0;
        let b = render(&p, &opts()).unwrap().0;
        assert_eq!(a, b);
    }

    #[test]
    fn sheet_composes_labeled_cells() {
        let png = sheet(
            &post("Sheet"),
            &[320.0, 390.0],
            &[Theme::Light, Theme::Dark],
            1.0,
        )
        .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // Four cells side by side are wider than one 390px render.
        let (one, _) = render(&post("Sheet"), &opts()).unwrap();
        assert!(png.len() > one.len() / 2);
    }

    #[test]
    fn sheet_rejects_an_empty_grid() {
        assert!(matches!(
            sheet(&post("x"), &[], &[Theme::Light], 1.0),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn invalid_input_is_an_error_not_a_panic() {
        let mut p = post("x");
        p.media = (0..21)
            .map(|_| model::Media {
                width: 10,
                height: 10,
                ..model::Media::default()
            })
            .collect();
        assert!(matches!(explain(&p, &opts()), Err(Error::Invalid(_))));
        assert_eq!(explain(&p, &opts()).unwrap_err().http_status(), 400);
    }

    #[test]
    fn explain_json_round_trips() {
        let json = r#"{"text":"hi","author":{"name":"A","handle":"a"}}"#;
        let out = explain_json(json, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["size"]["height"].as_f64().unwrap() > 0.0);
        assert!(explain_json("{oops", None).is_err());
        // A mistyped field is an error that names the field and its spelling.
        let err = explain_json(r#"{"text":"hi","auther":{}}"#, None).unwrap_err();
        assert!(
            err.to_string().contains("auther") && err.to_string().contains("author"),
            "{err}"
        );
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn base64_decode_handles_padding_and_prefixes() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("data:image/png;base64,aGk=").unwrap(), b"hi");
        assert!(base64_decode("a!b=").is_err());
        assert_eq!(base64_decode("").unwrap(), Vec::<u8>::new());
        // Every byte value round-trips through the alphabet.
        let all: Vec<u8> = (0..=255u8).collect();
        let enc = {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for chunk in all.chunks(3) {
                let b = [
                    chunk[0],
                    *chunk.get(1).unwrap_or(&0),
                    *chunk.get(2).unwrap_or(&0),
                ];
                let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                for i in 0..4 {
                    if i <= chunk.len() {
                        s.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
                    } else {
                        s.push('=');
                    }
                }
            }
            s
        };
        assert_eq!(base64_decode(&enc).unwrap(), all);
    }

    #[test]
    fn decoded_images_are_borrowed_not_cloned() {
        let limits = Limits::standard();
        let mut p = post("one image");
        p.media = vec![model::Media {
            width: 4,
            height: 4,
            data: Some(tiny_png_base64()),
            ..model::Media::default()
        }];
        let images = one_image_list(&p, &limits, ImageRef::Media(0));
        let borrowed = paint::ImageSource::rgba_ref(&images, &ImageRef::Media(0));
        assert!(
            borrowed.is_some(),
            "the painter's zero-copy path must have the pixels"
        );
        // The owned accessor is the same image, but it is not what a render uses.
        assert_eq!(
            paint::ImageSource::rgba(&images, &ImageRef::Media(0)).map(|d| (d.width, d.height)),
            borrowed.map(|d| (d.width, d.height))
        );
        // An image the list does not draw is never decoded (and is not the
        // avatar: an avatar slot always has a fallback, by design).
        assert!(paint::ImageSource::rgba_ref(&images, &ImageRef::Media(1)).is_none());
    }

    /// An avatar slot always has something in it: X's default profile image is
    /// embedded, decoded once, and borrowed by every avatar with no usable bytes
    /// — the post's own and a quoted post's alike. Media has no such fallback.
    #[test]
    fn an_avatar_without_bytes_draws_xs_default_image() {
        let limits = Limits::standard();

        let bare = post("no avatar bytes");
        let own = one_image_list(&bare, &limits, ImageRef::Avatar);
        let a = paint::ImageSource::rgba_ref(&own, &ImageRef::Avatar).expect("default avatar");
        assert_eq!(
            (a.width, a.height),
            (DEFAULT_AVATAR_CACHE, DEFAULT_AVATAR_CACHE),
            "the fallback is cached at avatar size"
        );
        assert!(
            a.rgba.iter().any(|b| *b != 0),
            "the fallback must be real pixels, not a transparent buffer"
        );

        // Quoted posts take the same path.
        let mut quoting = post("quoting something");
        quoting.quote = Some(Box::new(post("the quoted post")));
        let quoted = one_image_list(
            &quoting,
            &limits,
            ImageRef::Quote(Box::new(ImageRef::Avatar)),
        );
        let q = paint::ImageSource::rgba_ref(&quoted, &ImageRef::Quote(Box::new(ImageRef::Avatar)))
            .expect("the quoted post's avatar falls back too");
        assert_eq!((q.width, q.height), (a.width, a.height));
        assert_eq!(q.rgba, a.rgba, "one image, decoded once");

        // Media keeps its placeholder: X has no default for a photograph.
        let mut with_media = post("media without bytes");
        with_media.media = vec![model::Media {
            width: 10,
            height: 10,
            ..model::Media::default()
        }];
        let media = one_image_list(&with_media, &limits, ImageRef::Media(0));
        assert!(paint::ImageSource::rgba_ref(&media, &ImageRef::Media(0)).is_none());
    }

    /// A quote of a quote reaches the right post: `Quote(Quote(Media(0)))` is
    /// the twice-quoted post's first image, not the root's and not a placeholder.
    #[test]
    fn a_twice_quoted_post_resolves_its_own_images() {
        let limits = Limits::standard();
        let ref2 = ImageRef::Quote(Box::new(ImageRef::Quote(Box::new(ImageRef::Media(0)))));

        let mut middle = post("the quoted post");
        middle.media = vec![model::Media {
            width: 3,
            height: 3,
            data: Some(tiny_png_base64_n(3)),
            ..model::Media::default()
        }];
        let mut deepest = post("the twice-quoted post");
        deepest.media = vec![model::Media {
            width: 5,
            height: 5,
            data: Some(tiny_png_base64_n(5)),
            ..model::Media::default()
        }];
        middle.quote = Some(Box::new(deepest));
        let mut root = post("the outer post");
        root.media = vec![model::Media {
            width: 2,
            height: 2,
            data: Some(tiny_png_base64_n(2)),
            ..model::Media::default()
        }];
        root.quote = Some(Box::new(middle));

        let images = one_image_list(&root, &limits, ref2.clone());
        let found = paint::ImageSource::rgba_ref(&images, &ref2).expect("the twice-quoted image");
        assert_eq!(
            (found.width, found.height),
            (5, 5),
            "each level resolves against its own post, not the root's or the middle one's"
        );
        // One level up is the middle post's image, still distinct. (Each list
        // decodes only what it draws, so this one gets its own.)
        let once = ImageRef::Quote(Box::new(ImageRef::Media(0)));
        let once_images = one_image_list(&root, &limits, once.clone());
        let middle_image =
            paint::ImageSource::rgba_ref(&once_images, &once).expect("the quoted post's image");
        assert_eq!(
            (middle_image.width, middle_image.height),
            (3, 3),
            "the depth-1 reference must not resolve against the twice-quoted post"
        );
    }

    /// A twice-quoted account with no avatar bytes still draws X's default image
    /// rather than the "V…" placeholder.
    #[test]
    fn a_twice_quoted_avatar_falls_back_to_xs_default_image() {
        let limits = Limits::standard();
        let ref2 = ImageRef::Quote(Box::new(ImageRef::Quote(Box::new(ImageRef::Avatar))));

        // With bytes: the twice-quoted account's own avatar.
        let mut deepest = post("the twice-quoted post");
        deepest.author.avatar = Some(tiny_png_base64_n(7));
        let mut middle = post("the quoted post");
        middle.quote = Some(Box::new(deepest));
        let mut root = post("the outer post");
        root.quote = Some(Box::new(middle));
        let images = one_image_list(&root, &limits, ref2.clone());
        let found = paint::ImageSource::rgba_ref(&images, &ref2).expect("the twice-quoted avatar");
        assert_eq!((found.width, found.height), (7, 7));

        // Without bytes: X's default avatar, exactly as at depth 1.
        let mut deepest = post("the twice-quoted post");
        deepest.author.avatar = None;
        let mut middle = post("the quoted post");
        middle.quote = Some(Box::new(deepest));
        let mut root = post("the outer post");
        root.quote = Some(Box::new(middle));
        let images = one_image_list(&root, &limits, ref2.clone());
        let fallback = paint::ImageSource::rgba_ref(&images, &ref2)
            .expect("a twice-quoted avatar with no bytes borrows X's default image");
        assert_eq!(
            (fallback.width, fallback.height),
            (DEFAULT_AVATAR_CACHE, DEFAULT_AVATAR_CACHE)
        );
        assert!(images.notes().is_empty(), "{:?}", images.notes());
    }

    /// An `n × n` PNG, so a test can tell one image from another by its size.
    fn tiny_png_base64_n(n: u32) -> String {
        let mut pm = paint::Pixmap::new(n, n);
        for i in (0..pm.rgba.len()).step_by(4) {
            pm.rgba[i..i + 4].copy_from_slice(&[0, 128, 255, 255]);
        }
        base64_encode(&paint::encode_png(&pm))
    }

    /// A 4×4 PNG, so a test needs no binary fixture.
    fn tiny_png_base64() -> String {
        let mut pm = paint::Pixmap::new(4, 4);
        for i in (0..pm.rgba.len()).step_by(4) {
            pm.rgba[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
        base64_encode(&paint::encode_png(&pm))
    }

    /// Standard base64 with padding, for test fixtures.
    fn base64_encode(bytes: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                out.push(if i <= chunk.len() {
                    A[((n >> (18 - 6 * i)) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        out
    }

    #[test]
    fn images_are_decoded_lazily_and_a_broken_image_is_a_warning() {
        let limits = Limits::standard();
        let mut p = post("with media");
        p.media = vec![model::Media {
            width: 100,
            height: 100,
            data: Some("bm90YW5pbWFnZQ==".into()),
            ..model::Media::default()
        }];
        let images = one_image_list(&p, &limits, ImageRef::Media(0));
        assert!(paint::ImageSource::rgba(&images, &ImageRef::Media(0)).is_none());
        assert!(
            images.notes().iter().any(|n| n.contains("media[0]")),
            "{:?}",
            images.notes()
        );
        // An avatar slot always draws something: with no bytes (and with bytes
        // that cannot be decoded) it is X's own default profile image, decoded
        // once for the process.
        let bare = post("no media");
        let bare_images = one_image_list(&bare, &limits, ImageRef::Avatar);
        let fallback = paint::ImageSource::rgba_ref(&bare_images, &ImageRef::Avatar)
            .expect("an avatar without bytes must borrow X's default image");
        // Cached at the size an avatar is drawn at, not X's 400×400 original.
        assert_eq!(
            (fallback.width, fallback.height),
            (DEFAULT_AVATAR_CACHE, DEFAULT_AVATAR_CACHE)
        );
        assert!(
            bare_images.notes().is_empty(),
            "the default avatar is not a decode failure: {:?}",
            bare_images.notes()
        );
        // Invalid base64 is reported, not a panic — and still draws the default.
        let mut bad = post("bad");
        bad.author.avatar = Some("!!not base64!!".into());
        let images = one_image_list(&bad, &limits, ImageRef::Avatar);
        assert_eq!(
            paint::ImageSource::rgba(&images, &ImageRef::Avatar).map(|d| (d.width, d.height)),
            Some((DEFAULT_AVATAR_CACHE, DEFAULT_AVATAR_CACHE))
        );
        assert!(
            images.notes().iter().any(|n| n.contains("avatar")),
            "{:?}",
            images.notes()
        );
    }
}
