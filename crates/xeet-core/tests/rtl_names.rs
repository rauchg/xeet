//! Account names written right to left.
//!
//! On x.com (logged out), the name in a post's header, in a quote's header and
//! in a video's "From" credit is `<div class="block truncate" dir="auto">`, a
//! box that shrinks to the name inside a left-to-right row: an Arabic or Hebrew
//! name starts at the box's left edge and its badge follows it. The media tag
//! line has no `dir` and takes the page's `ltr`.
//!
//! The first two tests are @mrwnh's, from #11, which found and reproduced the
//! bug: a right-to-left name was aligned against the far edge of the unbounded
//! box it is measured in, and drew nowhere near its badge.

use xeet_core::display::Item;
use xeet_core::inline::{self, BlockStyle, Dir, Span, Wrap};
use xeet_core::limits;
use xeet_core::model::{Post, RenderOptions, Surface, Theme};
use xeet_core::render;

const NAMES: &[&str] = &["المنتخب السعودي", "שלום עולם"];

const SURFACES: &[Surface] = &[
    Surface::WebTimeline,
    Surface::WebDetail,
    Surface::IosTimeline,
    Surface::IosDetail,
];

struct Run {
    start: f32,
    end: f32,
    baseline: f32,
}

/// Each glyph run's first and last glyph origin, and the badge icons.
fn draw(post: &Post, surface: Surface) -> (Vec<Run>, Vec<(f32, f32)>) {
    let opts = RenderOptions {
        surface,
        width: 390.0,
        theme: Theme::Dark,
        ..RenderOptions::default()
    };
    let (list, _) = render::display_list(post, &opts).expect("renders");
    let mut runs = Vec::new();
    let mut badges = Vec::new();
    for item in &list.items {
        match item {
            Item::Glyphs { x, y, glyphs, .. } if !glyphs.is_empty() => {
                let xs = glyphs.iter().map(|g| x + g.x);
                runs.push(Run {
                    start: xs.clone().fold(f32::INFINITY, f32::min),
                    end: xs.fold(f32::NEG_INFINITY, f32::max),
                    baseline: *y,
                });
            }
            Item::Icon { name, rect, .. } if name.starts_with("icon-verified") => {
                badges.push((rect.x, rect.y + rect.h / 2.0));
            }
            _ => {}
        }
    }
    (runs, badges)
}

/// Asserts the name is drawn on the badge's row, inside the canvas, ending
/// before the badge.
fn assert_name_before_badge(runs: &[Run], badge: (f32, f32), what: &str) {
    for run in runs {
        assert!(
            run.start >= 0.0 && run.end <= 390.0,
            "{what}: a glyph run is drawn outside the canvas, at {}..{}",
            run.start,
            run.end
        );
    }
    let (badge_x, badge_mid) = badge;
    let name = runs
        .iter()
        .find(|r| (r.baseline - badge_mid).abs() < 12.0 && r.end < badge_x);
    assert!(
        name.is_some(),
        "{what}: no glyphs before the badge at x {badge_x} on its row"
    );
}

#[test]
fn a_right_to_left_name_is_drawn_before_its_badge() {
    for name in NAMES {
        let post = limits::parse_post(&format!(
            r#"{{"text":"hi","author":{{"name":"{name}","handle":"saudint","verified":"business"}}}}"#
        ))
        .unwrap();
        for &surface in SURFACES {
            let (runs, badges) = draw(&post, surface);
            let what = format!("{name} on {surface:?}");
            assert_eq!(badges.len(), 1, "{what}: one badge");
            assert_name_before_badge(&runs, badges[0], &what);
        }
    }
}

#[test]
fn a_quoted_right_to_left_name_is_drawn_before_its_badge() {
    for name in NAMES {
        let post = limits::parse_post(&format!(
            r#"{{"text":"What a season.","author":{{"name":"Emaar","handle":"emaar"}},
                "quote":{{"text":"hi","author":{{"name":"{name}","handle":"alhilal","verified":"business"}}}}}}"#
        ))
        .unwrap();
        for &surface in &[Surface::WebTimeline, Surface::WebDetail] {
            let (runs, badges) = draw(&post, surface);
            let what = format!("quoted {name} on {surface:?}");
            assert_eq!(badges.len(), 1, "{what}: one badge, the quote's");
            assert_name_before_badge(&runs, badges[0], &what);
        }
    }
}

/// A name too long for its row is cut inside its own box: still before the
/// badge, on both the plain and the quoted header.
#[test]
fn a_truncated_right_to_left_name_stays_in_its_box() {
    let long = ["المنتخب السعودي"; 6].join(" ");
    let plain = limits::parse_post(&format!(
        r#"{{"text":"hi","author":{{"name":"{long}","handle":"saudint","verified":"business"}}}}"#
    ))
    .unwrap();
    let quoted = limits::parse_post(&format!(
        r#"{{"text":"hi","author":{{"name":"Emaar","handle":"emaar"}},
            "quote":{{"text":"hi","author":{{"name":"{long}","handle":"alhilal","verified":"business"}}}}}}"#
    ))
    .unwrap();
    for (post, what) in [(&plain, "header"), (&quoted, "quote header")] {
        for &surface in &[Surface::WebTimeline, Surface::WebDetail] {
            let (runs, badges) = draw(post, surface);
            assert_eq!(badges.len(), 1);
            assert_name_before_badge(&runs, badges[0], &format!("long name, {what}, {surface:?}"));
        }
    }
}

/// `dir="auto"` is kept, and with it Chrome's handling of the two things it
/// decides: a cut right-to-left name ends in an ellipsis on its *left*, and a
/// name that fits starts at the left edge of its shrink-to-fit box.
#[test]
fn name_boxes_keep_dir_auto() {
    let style = |width: f32| BlockStyle {
        size: 15.0,
        line_height: 20.0,
        weight: 700,
        width,
        max_lines: Some(1),
        wrap: Wrap::BreakAll,
        preserve: false,
        dir: Dir::Auto,
        tab_size: 4.0,
    };
    let span = |t: &str| Span::new(t, xeet_core::color::Rgba::BLACK, 700);

    // Measured with no box: the block is as wide as the name and starts at 0.
    let fit = inline::layout(&[span("المنتخب السعودي")], &style(f32::MAX / 4.0));
    assert!(fit.rtl);
    let line = &fit.lines[0];
    let left = line.runs.iter().map(|r| r.x).fold(f32::INFINITY, f32::min);
    let right = line
        .runs
        .iter()
        .map(|r| r.x + r.width)
        .fold(0.0f32, f32::max);
    assert!(
        left.abs() < 0.01,
        "starts at {left}, not at the box's left edge"
    );
    assert!(
        (right - fit.width).abs() < 0.01,
        "ends at {right}, block is {}",
        fit.width
    );

    // Cut at 80 px: the ellipsis (the run that covers no text) is the leftmost.
    let cut = inline::layout(&[span(&["المنتخب السعودي"; 4].join(" "))], &style(80.0));
    assert!(cut.truncated);
    let runs = &cut.lines[0].runs;
    let ellipsis = runs
        .iter()
        .find(|r| r.start == r.end)
        .expect("an ellipsis run");
    assert!(
        runs.iter().all(|r| r.x >= ellipsis.x),
        "the ellipsis is not at the left end of a right-to-left line"
    );
}

/// The tag line has no `dir`: a label that starts with a right-to-left name
/// still starts at the content's left edge, like the post text above it.
#[test]
fn a_tag_line_starts_at_the_left_edge() {
    let post = limits::parse_post(
        r#"{"text":"hi","author":{"name":"Emaar","handle":"emaar"},
            "media":[{"kind":"image","width":1200,"height":800}],
            "mediaTags":["المنتخب السعودي","Second Person"]}"#,
    )
    .unwrap();
    let (runs, _) = draw(&post, Surface::WebTimeline);
    let text_left = runs.iter().map(|r| r.start).fold(f32::INFINITY, f32::min);
    // The tag line is the lowest row of text; it is several runs (the name's
    // words, then " and Second Person"), and its left edge is the leftmost.
    let lowest = runs
        .iter()
        .map(|r| r.baseline)
        .fold(f32::NEG_INFINITY, f32::max);
    let tag_left = runs
        .iter()
        .filter(|r| (r.baseline - lowest).abs() < 0.5)
        .map(|r| r.start)
        .fold(f32::INFINITY, f32::min);
    assert!(
        (tag_left - text_left).abs() < 1.0,
        "the tag line starts at {tag_left}, the text at {text_left}"
    );
}

/// The video "From" credit is the same `dir="auto"` box: a right-to-left name
/// is drawn inside the canvas, not 10^37 px to the right.
#[test]
fn a_right_to_left_video_credit_is_drawn() {
    for name in NAMES {
        let post = limits::parse_post(&format!(
            r#"{{"text":"hi","author":{{"name":"Emaar","handle":"emaar"}},
                "media":[{{"kind":"video","width":1280,"height":720,"duration":11,
                           "sourceAuthor":{{"name":"{name}","handle":"source"}}}}]}}"#
        ))
        .unwrap();
        let (runs, _) = draw(&post, Surface::WebTimeline);
        let lowest = runs
            .iter()
            .map(|r| r.baseline)
            .fold(f32::NEG_INFINITY, f32::max);
        // The duration and the credit share the badge's row at the bottom.
        let on_badge: Vec<&Run> = runs
            .iter()
            .filter(|r| (r.baseline - lowest).abs() < 2.0)
            .collect();
        assert!(
            on_badge.len() >= 2,
            "{name}: the credit is missing from the badge"
        );
        for run in on_badge {
            assert!(
                run.start >= 0.0 && run.end <= 390.0,
                "{name}: a credit run is drawn at {}..{}",
                run.start,
                run.end
            );
        }
    }
}
