//! Inline layout: spans of styled text in, positioned lines of glyphs out.
//!
//! This reproduces the parts of Chrome's inline layout that a post uses:
//! `white-space: pre-wrap` or `normal`, `overflow-wrap: anywhere`,
//! `word-break: break-all`, `dir="auto"` with bidi reordering, tab stops,
//! fixed line heights, and `-webkit-line-clamp`.

use crate::color::Rgba;
use crate::fonts::{self, db, FaceId};
use crate::linebreak;
use crate::shape::{self, ShapedGlyph};
use std::ops::Range;
use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo};

/// The `dir` attribute of the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// `dir="auto"`: the first strong character decides, LTR if there is none.
    Auto,
    Ltr,
    Rtl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    /// Break only at line-break opportunities; a long word overflows.
    Normal,
    /// `overflow-wrap: anywhere` / `break-word`: break inside a word only if
    /// it cannot fit on a line by itself.
    Anywhere,
    /// `word-break: break-all`: any grapheme boundary is an opportunity.
    BreakAll,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub color: Rgba,
    pub weight: u16,
    /// Chirp's `ss01` stylistic set, which X enables on handles.
    pub ss01: bool,
    /// Tabular figures (`font-variant-numeric: tabular-nums`), which X uses
    /// for counts and video durations.
    pub tnum: bool,
    /// Laid out as one unbreakable box (an inline-block, such as a button).
    pub atomic: bool,
    /// Label carried through to geometry output and the report.
    pub role: &'static str,
}

impl Span {
    pub fn new(text: impl Into<String>, color: Rgba, weight: u16) -> Span {
        Span {
            text: text.into(),
            color,
            weight,
            ss01: false,
            tnum: false,
            atomic: false,
            role: "text",
        }
    }
    pub fn role(mut self, role: &'static str) -> Span {
        self.role = role;
        self
    }
    pub fn atomic(mut self) -> Span {
        self.atomic = true;
        self
    }
    pub fn tnum(mut self) -> Span {
        self.tnum = true;
        self
    }
    pub fn ss01(mut self) -> Span {
        self.ss01 = true;
        self
    }
}

/// A [`BlockStyle::width`] this large is no box at all. Callers pass `f32::MAX`
/// (or a quarter of it) to measure a block's max-content width, and then often
/// draw that block as it is: a name, a handle, a badge label. Such a block is a
/// shrink-to-fit box, as wide as its widest line, so a right-to-left line starts
/// at the block's own left edge. Aligned against the unbounded width instead, it
/// would sit some 10^37 px to the right: an Arabic or Hebrew display name drew
/// nothing at all.
pub const UNBOUNDED: f32 = 1.0e9;

#[derive(Clone, Copy, Debug)]
pub struct BlockStyle {
    pub size: f32,
    pub line_height: f32,
    /// Weight of the block itself; sets the strut and the ellipsis font.
    pub weight: u16,
    /// The box's width. At or above [`UNBOUNDED`] there is no box: the block
    /// is measured at its max-content width and is as wide as its widest line.
    pub width: f32,
    pub max_lines: Option<usize>,
    pub wrap: Wrap,
    /// `white-space: pre-wrap` when true, `normal` (collapsing) when false.
    pub preserve: bool,
    pub dir: Dir,
    /// CSS `tab-size`, in multiples of the space advance. X sets 4.
    pub tab_size: f32,
}

#[derive(Clone, Debug)]
pub struct Glyph {
    pub id: u32,
    /// Where the glyph is drawn relative to the run origin, in CSS pixels:
    /// the pen position plus the shaper's offset `dx`.
    pub x: f32,
    pub y: f32,
    pub adv: f32,
    /// Shaper offset included in `x`. Range rectangles follow the pen
    /// (`x - dx`), not the offset glyph; right-to-left kerning uses offsets.
    pub dx: f32,
    pub span: u16,
    /// Byte offset into [`TextBlock::text`].
    pub cluster: u32,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub face: FaceId,
    /// Left edge on the line, after bidi reordering and alignment.
    pub x: f32,
    pub width: f32,
    /// In visual order (left to right), also for right-to-left runs.
    pub glyphs: Vec<Glyph>,
    pub start: usize,
    pub end: usize,
    /// Resolved bidi embedding level; odd is right to left.
    pub level: u8,
    pub rtl: bool,
    /// Resolved script (ICU4C code); Chrome shapes each script run apart.
    pub script: u16,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub top: f32,
    pub height: f32,
    /// Baseline, measured down from `top`.
    pub baseline: f32,
    /// Content width, not counting spaces that hang at the end of the line.
    pub width: f32,
    pub runs: Vec<Run>,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug)]
pub struct SpanRange {
    pub start: usize,
    pub end: usize,
    pub color: Rgba,
    pub weight: u16,
    pub ss01: bool,
    pub tnum: bool,
    pub atomic: bool,
    pub role: &'static str,
}

#[derive(Clone, Debug)]
pub struct TextBlock {
    pub text: String,
    pub spans: Vec<SpanRange>,
    pub lines: Vec<Line>,
    /// Widest line.
    pub width: f32,
    pub height: f32,
    /// True if `max_lines` cut content off.
    pub truncated: bool,
    /// Line count before clamping.
    pub natural_lines: usize,
    pub size: f32,
    pub weight: u16,
    /// Resolved paragraph direction.
    pub rtl: bool,
}

/// One span's extent on one line; what a DOM `Range` rectangle reports.
#[derive(Clone, Debug)]
pub struct Fragment {
    pub text: String,
    pub line: usize,
    pub span: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

struct Ctx<'a> {
    text: &'a str,
    spans: &'a [SpanRange],
    st: &'a BlockStyle,
    /// Bidi embedding level of every byte of `text`, before line rule L1.
    /// Empty for ASCII text, which is all at `base`.
    levels: Vec<u8>,
    /// Resolved script of every byte (see `script.rs`); empty for ASCII text,
    /// which is a single script run.
    scripts: Vec<u16>,
    base: u8,
    /// One tab stop interval, in CSS pixels.
    tab_width: f32,
    space_width: f32,
}

impl Ctx<'_> {
    /// Advance of a tab whose start is `x` px from the line start.
    ///
    /// Blink's rule: advance to the next multiple of `tab-size` spaces, but
    /// never by less than half a space; then it skips to the following stop.
    fn tab_advance(&self, x: f32) -> f32 {
        if self.tab_width <= 0.0 {
            return 0.0;
        }
        let mut d = self.tab_width - x.rem_euclid(self.tab_width);
        if d < self.space_width / 2.0 {
            d += self.tab_width;
        }
        d
    }

    /// Bidi level of the byte at `offset`.
    #[inline]
    fn level_at(&self, offset: usize) -> u8 {
        if self.levels.is_empty() {
            self.base
        } else {
            self.levels[offset]
        }
    }

    fn span_at(&self, offset: usize) -> usize {
        // Spans are contiguous and in order, so their ends never decrease: the
        // first one that ends past the offset is the answer. Binary search
        // once there are more spans than a scan is worth.
        if self.spans.len() > 4 {
            let i = self.spans.partition_point(|s| s.end <= offset);
            return i.min(self.spans.len() - 1);
        }
        for (i, s) in self.spans.iter().enumerate() {
            if offset < s.end {
                return i;
            }
        }
        self.spans.len().saturating_sub(1)
    }
}

/// Lays out `spans` as one block of text.
pub fn layout(spans: &[Span], st: &BlockStyle) -> TextBlock {
    let mut text = String::new();
    let mut ranges = Vec::with_capacity(spans.len());
    let mut prev_space = true; // collapses leading whitespace when not preserving
    for s in spans {
        let start = text.len();
        if st.preserve {
            // CRLF and lone CR both mean one line break.
            let mut chars = s.text.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\r' {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    text.push('\n');
                } else {
                    text.push(c);
                }
            }
        } else {
            for c in s.text.chars() {
                if matches!(c, ' ' | '\n' | '\t' | '\r') {
                    if !prev_space {
                        text.push(' ');
                    }
                    prev_space = true;
                } else {
                    text.push(c);
                    prev_space = false;
                }
            }
        }
        ranges.push(SpanRange {
            start,
            end: text.len(),
            color: s.color,
            weight: s.weight,
            ss01: s.ss01,
            tnum: s.tnum,
            atomic: s.atomic,
            role: s.role,
        });
    }
    if !st.preserve {
        let trimmed = text.trim_end_matches(' ').len();
        text.truncate(trimmed);
        for r in &mut ranges {
            r.start = r.start.min(trimmed);
            r.end = r.end.min(trimmed);
        }
    }

    // Direction. `dir="auto"` looks at the whole element, so every
    // paragraph (hard line) shares one base level, as in Chrome.
    let rtl = match st.dir {
        Dir::Ltr => false,
        Dir::Rtl => true,
        Dir::Auto => first_strong_is_rtl(&text),
    };
    let base = u8::from(rtl);
    // In a left-to-right paragraph, ASCII cannot be right-to-left and has no
    // directional controls, so every byte is at the base level: Latin copy
    // needs neither the bidi pass nor a level array of its own.
    let plain_ltr = !rtl && text.is_ascii();
    let mut levels = if plain_ltr {
        Vec::new()
    } else {
        vec![base; text.len()]
    };
    if !plain_ltr && (rtl || text.chars().any(is_rtl_or_control)) {
        let mut pos = 0;
        for para in text.split('\n') {
            if !para.is_empty() {
                let info = ParagraphBidiInfo::new(
                    para,
                    Some(if rtl { Level::rtl() } else { Level::ltr() }),
                );
                for (i, l) in info.levels.iter().enumerate() {
                    levels[pos + i] = l.number();
                }
                if !rtl {
                    icu_numbers_after_brackets(para, &mut levels[pos..pos + para.len()]);
                }
            }
            pos += para.len() + 1;
        }
    }

    let space_width = if text.contains('\t') {
        shape::measure(db().primary(st.weight), " ", st.size, false)
    } else {
        0.0
    };
    let scripts = if text.is_ascii() {
        Vec::new()
    } else {
        crate::script::resolve(&text)
    };
    let ctx = Ctx {
        text: &text,
        spans: &ranges,
        st,
        levels,
        scripts,
        base,
        tab_width: st.tab_size * space_width,
        space_width,
    };
    let mut lines: Vec<Line> = Vec::new();
    let mut pos = 0;
    while pos < text.len() {
        let rest = &text[pos..];
        let (content_len, consumed) = match rest.find('\n') {
            Some(i) => (i, i + 1),
            None => (rest.len(), rest.len()),
        };
        layout_hard_line(&ctx, pos..pos + content_len, &mut lines);
        pos += consumed;
    }

    let natural_lines = lines.len();
    let mut truncated = false;
    if let Some(max) = st.max_lines {
        if lines.len() > max {
            lines.truncate(max);
            truncated = true;
            if let Some(last) = lines.last_mut() {
                ellipsize(&ctx, last);
            }
        }
    }

    // A shrink-to-fit block (see [`UNBOUNDED`]): its right edge is its widest
    // line, and every right-to-left line ends there.
    if rtl && st.width >= UNBOUNDED {
        let edge = lines.iter().map(|l| l.width).fold(0.0f32, f32::max);
        for line in &mut lines {
            let total: f32 = line.runs.iter().map(|r| r.width).sum();
            let shift = edge - total;
            for r in &mut line.runs {
                r.x += shift;
            }
        }
    }

    let mut y = 0.0;
    let mut width = 0.0f32;
    for line in &mut lines {
        line.top = y;
        y += line.height;
        width = width.max(line.width);
    }
    TextBlock {
        text,
        spans: ranges,
        lines,
        width,
        height: y,
        truncated,
        natural_lines,
        size: st.size,
        weight: st.weight,
        rtl,
    }
}

/// Where ICU (and so Chrome) differs from the UAX #9 reference that
/// `unicode-bidi` follows: ICU resolves bracket pairs (rule N0) while it
/// scans, and a resolved bracket then counts as the last strong type for
/// numbers after it (rule W7). In an LTR paragraph, "x (עולם) (100%)" thus
/// gets "100%" at level 0, kerned with its parentheses, where the reference
/// gives level 2. Only LTR paragraphs are affected: in RTL ones a number is
/// at level 2 either way.
fn icu_numbers_after_brackets(para: &str, levels: &mut [u8]) {
    use icu_properties::props::{BidiMirroringGlyph, BidiPairedBracketType};
    use icu_properties::CodePointMapData;
    let mirrors = CodePointMapData::<BidiMirroringGlyph>::new();
    let chars: Vec<(usize, char)> = para.char_indices().collect();
    let class = |c: char| unicode_bidi::bidi_class(c);
    // Bracket pairs that N0 resolves: some strong type (EN and AN count as R) inside.
    let mut resolved = vec![false; chars.len()];
    let mut stack: Vec<(char, usize)> = Vec::new();
    for (k, &(_, c)) in chars.iter().enumerate() {
        let m = mirrors.get(c);
        match m.paired_bracket_type {
            BidiPairedBracketType::Open if stack.len() < 63 => {
                stack.push((m.mirroring_glyph.unwrap_or(c), k))
            }
            BidiPairedBracketType::Close => {
                if let Some(d) = stack.iter().rposition(|&(close, _)| close == c) {
                    let open = stack[d].1;
                    stack.truncate(d);
                    let strong_inside = chars[open + 1..k].iter().any(|&(_, x)| {
                        matches!(
                            class(x),
                            BidiClass::L
                                | BidiClass::R
                                | BidiClass::AL
                                | BidiClass::EN
                                | BidiClass::AN
                        )
                    });
                    if strong_inside {
                        resolved[open] = true;
                        resolved[k] = true;
                    }
                }
            }
            _ => {}
        }
    }
    let mut last_l = false;
    let mut k = 0;
    while k < chars.len() {
        let (off, c) = chars[k];
        match class(c) {
            BidiClass::L => last_l = true,
            BidiClass::R | BidiClass::AL => last_l = false,
            BidiClass::EN if last_l && levels[off] == 2 => {
                // The whole number (digits with their separators and signs).
                let mut e = k;
                while e < chars.len() && levels[chars[e].0] == 2 {
                    e += 1;
                }
                let end = chars.get(e).map_or(para.len(), |p| p.0);
                for l in &mut levels[off..end] {
                    *l = 0;
                }
                k = e;
                continue;
            }
            _ if resolved[k] => last_l = levels[off].is_multiple_of(2),
            _ => {}
        }
        k += 1;
    }
}

/// HTML `dir="auto"`: the first character with a strong direction decides.
fn first_strong_is_rtl(text: &str) -> bool {
    for c in text.chars() {
        match unicode_bidi::bidi_class(c) {
            BidiClass::L => return false,
            BidiClass::R | BidiClass::AL => return true,
            _ => {}
        }
    }
    false
}

fn is_rtl_or_control(c: char) -> bool {
    (c as u32) >= 0x0590
        && matches!(
            unicode_bidi::bidi_class(c),
            BidiClass::R
                | BidiClass::AL
                | BidiClass::AN
                | BidiClass::RLE
                | BidiClass::RLO
                | BidiClass::RLI
                | BidiClass::LRE
                | BidiClass::LRO
                | BidiClass::LRI
                | BidiClass::FSI
                | BidiClass::PDF
                | BidiClass::PDI
        )
}

/// Item key: face, weight, (ss01, tnum), atomic span, bidi level, script.
type ItemKey = (FaceId, u16, (bool, bool), Option<usize>, u8, u16);

/// Splits `s` into items that share a face, style and bidi level, and
/// shapes each. `abs` is the offset of `s` within the block text. Bytes from
/// `trail` on (absolute) are the line's trailing whitespace, which bidi rule
/// L1 puts back at the paragraph level. The segment is assumed to start a
/// line, which is where tab stops are measured from.
fn shape_segment(ctx: &Ctx, abs: usize, s: &str, gbs: &[usize], trail: usize) -> Vec<Run> {
    let fonts = db();
    let mut runs: Vec<Run> = Vec::new();
    let mut scratch: Vec<ShapedGlyph> = Vec::new();
    // Graphemes given their emoji advance, one bitset reused across runs.
    let mut seen: Vec<u64> = Vec::new();

    // Key of the item being built.
    let mut item_start = 0usize;
    let mut cur: Option<ItemKey> = None;

    let mut flush = |runs: &mut Vec<Run>, from: usize, to: usize, key: ItemKey| {
        if from == to {
            return;
        }
        let face = fonts.face(key.0);
        let rtl = key.4 % 2 == 1;
        scratch.clear();
        shape::shape_with(
            face,
            &s[from..to],
            ctx.st.size,
            key.2 .0,
            key.2 .1,
            rtl,
            crate::script::hb_script(key.5),
            &mut scratch,
        );
        let mut glyphs = Vec::with_capacity(scratch.len());
        // Emoji take the advance of the platform emoji font, once per
        // grapheme, whatever the bundled artwork has: a sequence the bundled
        // font cannot ligate, or an emoji newer than it, is still one emoji
        // on the devices people read X on.
        let emoji_adv =
            (face.kind == fonts::FaceKind::Emoji).then(|| fonts::emoji_advance(ctx.st.size));
        // The graphemes already given their emoji advance. Glyphs arrive in
        // visual order, which reverses the clusters of a right-to-left run, so
        // this is a set rather than a "last seen" index; it is a reused bitset
        // so the check neither allocates nor scans.
        seen.clear();
        for g in &scratch {
            let rel = from + g.cluster as usize;
            let adv = match emoji_adv {
                Some(a) => {
                    let gi = gbs.partition_point(|&o| o <= rel);
                    let word = gi / 64;
                    if word >= seen.len() {
                        seen.resize(word + 1, 0);
                    }
                    let bit = 1u64 << (gi % 64);
                    if seen[word] & bit != 0 {
                        0.0
                    } else {
                        seen[word] |= bit;
                        a
                    }
                }
                None => g.adv,
            };
            let cluster = abs + rel;
            // `x` holds only the offset until positions are assigned below.
            glyphs.push(Glyph {
                id: g.id,
                x: g.dx,
                y: g.dy,
                adv,
                dx: g.dx,
                span: ctx.span_at(cluster) as u16,
                cluster: cluster as u32,
            });
        }
        runs.push(Run {
            face: key.0,
            x: 0.0,
            width: 0.0,
            glyphs,
            start: abs + from,
            end: abs + to,
            level: key.4,
            rtl,
            script: key.5,
        });
    };

    for w in gbs.windows(2) {
        let (a, b) = (w[0], w[1]);
        let cluster = &s[a..b];
        let span_idx = ctx.span_at(abs + a);
        let span = &ctx.spans[span_idx];
        let first = cluster.chars().next().unwrap_or(' ');
        // `emoji` is an explicit request (VS16, keycap, skin tone, ZWJ); a
        // character Unicode draws as emoji by default is handled by
        // `select_with`, after Chirp, as in X's stack.
        let emoji = !cluster.is_ascii() && fonts::cluster_requests_emoji(cluster);
        let face = fonts.select_with(first, span.weight, emoji, cluster.contains('\u{FE0E}'));
        // L1: tabs, and trailing whitespace, take the paragraph level.
        let level = if first == '\t' || abs + a >= trail {
            ctx.base
        } else {
            ctx.level_at(abs + a)
        };
        let script = ctx.scripts.get(abs + a).copied().unwrap_or(0);
        let key = (
            face,
            span.weight,
            (span.ss01, span.tnum),
            span.atomic.then_some(span_idx),
            level,
            script,
        );
        match cur {
            Some(k) if k == key => {}
            Some(k) => {
                flush(&mut runs, item_start, a, k);
                item_start = a;
                cur = Some(key);
            }
            None => {
                item_start = a;
                cur = Some(key);
            }
        }
    }
    if let Some(k) = cur {
        flush(&mut runs, item_start, s.len(), k);
    }

    // Tabs advance to the next stop, measured in logical order from the line start.
    if s.contains('\t') {
        let mut pen = 0.0f32;
        for r in &mut runs {
            let n = r.glyphs.len();
            for k in 0..n {
                let g = &mut r.glyphs[if r.rtl { n - 1 - k } else { k }];
                if ctx.text.as_bytes()[g.cluster as usize] == b'\t' {
                    g.adv = ctx.tab_advance(pen);
                }
                pen += g.adv;
            }
        }
    }
    for r in &mut runs {
        let mut pen = 0.0f32;
        for g in &mut r.glyphs {
            g.x += pen;
            pen += g.adv;
        }
        r.width = pen;
    }
    let mut x = 0.0;
    for r in &mut runs {
        r.x = x;
        x += r.width;
    }
    runs
}

/// Advance of each grapheme in a segment, from its shaped runs.
fn grapheme_advances(runs: &[Run], abs: usize, gbs: &[usize]) -> Vec<f32> {
    let n = gbs.len() - 1;
    let mut adv = vec![0.0f32; n];
    // Advance summed per shaping cluster, in logical order.
    let mut clusters: Vec<(usize, f32)> = Vec::new();
    for run in runs {
        clusters.clear();
        let mut push = |g: &Glyph| match clusters.last_mut() {
            Some((c, sum)) if *c == g.cluster as usize => *sum += g.adv,
            _ => clusters.push((g.cluster as usize, g.adv)),
        };
        // Right-to-left runs list glyphs in visual order.
        if run.rtl {
            run.glyphs.iter().rev().for_each(&mut push);
        } else {
            run.glyphs.iter().for_each(&mut push);
        }
        for ci in 0..clusters.len() {
            let (c, sum) = clusters[ci];
            let next = clusters.get(ci + 1).map_or(run.end, |p| p.0);
            // A shaping cluster can cover several graphemes (a ligature).
            let g0 = gbs.partition_point(|&o| abs + o < c).min(n - 1);
            let g1 = gbs.partition_point(|&o| abs + o < next).max(g0 + 1).min(n);
            let share = sum / (g1 - g0).max(1) as f32;
            for a in adv.iter_mut().take(g1).skip(g0) {
                *a += share;
            }
        }
    }
    adv
}

/// Rounds up to Chrome's layout unit, 1/64 px (`LayoutUnit::FromFloatCeil`).
#[inline]
fn ceil64(w: f32) -> f32 {
    (w * 64.0).ceil() / 64.0
}

/// Whether content of `width` fits in `avail`, by Chrome's rule.
///
/// `width` is the line's width shaped on its own (Chrome re-shapes the line
/// end, so kerning across the break is not part of it), already snapped to
/// 1/64 px per item (see `snapped_width`). It must not exceed `avail`, with
/// no tolerance, except that a line ending at a space break gets exactly one
/// 1/64 px of slack: Chrome's bookkeeping for the hanging trailing space
/// leaves it. So a 240.015625 px line fits a 240 px box when it ends before a
/// space, and not when it ends after a hyphen. Measured over 600 probes with
/// tools/oracle/fit-threshold.mjs.
#[inline]
fn fits(width: f32, avail: f32, at_space: bool) -> bool {
    const UNIT: f32 = 1.0 / 64.0;
    ceil64(width) <= avail + if at_space { UNIT } else { 0.0 }
}

/// Width of a line as Chrome adds it up: every inline item (a bidi run, and
/// each tab, which Chrome shapes on its own) is snapped up to 1/64 px and
/// the snapped widths are summed. A tab advances to its stop from the
/// snapped position, so it absorbs the rounding of the text before it.
/// `adv` and `levels` describe the line's graphemes.
fn snapped_width(ctx: &Ctx, adv: &[f32], levels: &[u8], is_tab: &[bool]) -> f32 {
    let mut total = 0.0f32;
    let mut item = 0.0f32;
    for g in 0..adv.len() {
        let boundary = g > 0 && (levels[g] != levels[g - 1] || is_tab[g] || is_tab[g - 1]);
        if boundary {
            total += ceil64(item);
            item = 0.0;
        }
        item += if is_tab[g] {
            ctx.tab_advance(total)
        } else {
            adv[g]
        };
    }
    total + ceil64(item)
}

fn layout_hard_line(ctx: &Ctx, range: Range<usize>, out: &mut Vec<Line>) {
    let st = ctx.st;
    if range.is_empty() {
        out.push(make_line(ctx, Vec::new(), range.start, range.end, 0.0));
        return;
    }
    let s = &ctx.text[range.clone()];
    let abs = range.start;
    let gbs = linebreak::grapheme_boundaries(s);
    let n = gbs.len() - 1;

    // Preserved whitespace that hangs at the end of a line under pre-wrap:
    // spaces, tabs, and (as Chrome does) the ideographic space. One pass over
    // the graphemes fills all three, since every later decision reads them.
    let mut is_space = vec![false; n];
    let mut is_tab = vec![false; n];
    let mut has_tab = false;
    for i in 0..n {
        match &s[gbs[i]..gbs[i + 1]] {
            " " | "\u{3000}" => is_space[i] = true,
            "\t" => {
                is_space[i] = true;
                is_tab[i] = true;
                has_tab = true;
            }
            _ => {}
        }
    }
    // Start of the whitespace that ends graphemes ls..le, which hangs.
    let trail_of = |ls: usize, le: usize| {
        let mut t = le;
        while t > ls && is_space[t - 1] {
            t -= 1;
        }
        t
    };

    let runs = shape_segment(ctx, abs, s, &gbs, abs + gbs[trail_of(0, n)]);
    let adv = grapheme_advances(&runs, abs, &gbs);
    let mut xs = Vec::with_capacity(n + 1);
    let mut acc = 0.0f32;
    xs.push(0.0);
    for a in &adv {
        acc += a;
        xs.push(acc);
    }
    // Width of graphemes ls..le as the start of a line. Tab advances
    // depend on where the line starts, so lines with tabs are re-walked.
    let width_from = |ls: usize, le: usize| -> f32 {
        if !has_tab {
            return xs[le] - xs[ls];
        }
        let mut x = 0.0f32;
        for g in ls..le {
            x += if is_tab[g] {
                ctx.tab_advance(x)
            } else {
                adv[g]
            };
        }
        x
    };
    let trimmed_width = |ls: usize, le: usize| width_from(ls, trail_of(ls, le));
    // Bidi level of each grapheme, for item snapping. Tabs take the base level.
    let glev: Vec<u8> = (0..n)
        .map(|g| {
            if is_tab[g] {
                ctx.base
            } else {
                ctx.level_at(abs + gbs[g])
            }
        })
        .collect();
    let multi_item = has_tab || glev.iter().any(|&l| l != glev[0]);
    let fit_width = |ls: usize, le: usize| -> f32 {
        let t = trail_of(ls, le);
        if multi_item {
            snapped_width(ctx, &adv[ls..t], &glev[ls..t], &is_tab[ls..t])
        } else {
            xs[t] - xs[ls]
        }
    };
    let at_space = |ls: usize, le: usize| trail_of(ls, le) < le;

    // Fast path: the whole hard line fits.
    if fits(fit_width(0, n), st.width, at_space(0, n)) {
        let w = trimmed_width(0, n);
        out.push(make_line(ctx, runs, range.start, range.end, w));
        return;
    }

    // Break opportunities as grapheme indices.
    let in_atomic = |offset: usize| {
        ctx.spans
            .iter()
            .any(|sp| sp.atomic && abs + offset > sp.start && abs + offset < sp.end)
    };
    let opps: Vec<usize> = match st.wrap {
        Wrap::BreakAll => (1..n).filter(|&g| !in_atomic(gbs[g])).collect(),
        _ => {
            let mut v: Vec<usize> = linebreak::line_breaks(s)
                .into_iter()
                .filter(|&o| !in_atomic(o))
                .filter_map(|o| gbs.binary_search(&o).ok())
                .collect();
            // The edges of an atomic box are always opportunities.
            for sp in ctx.spans.iter().filter(|sp| sp.atomic) {
                for edge in [sp.start, sp.end] {
                    if edge > abs && edge < range.end {
                        if let Ok(g) = gbs.binary_search(&(edge - abs)) {
                            v.push(g);
                        }
                    }
                }
            }
            v.sort_unstable();
            v.dedup();
            v
        }
    };

    // Shapes graphemes ls..le as a line of their own and returns its width
    // without hanging spaces. Kerning across the break is gone, so this can
    // differ from the width measured inside the whole paragraph.
    let shape_line = |ls: usize, le: usize| -> (Vec<Run>, f32, f32) {
        let sub = &s[gbs[ls]..gbs[le]];
        let sub_gbs: Vec<usize> = gbs[ls..=le].iter().map(|o| o - gbs[ls]).collect();
        let t = trail_of(ls, le);
        let line_runs = shape_segment(ctx, abs + gbs[ls], sub, &sub_gbs, abs + gbs[t]);
        let sub_adv = grapheme_advances(&line_runs, abs + gbs[ls], &sub_gbs);
        let w: f32 = sub_adv[..t - ls].iter().sum();
        let fw = if multi_item {
            snapped_width(ctx, &sub_adv[..t - ls], &glev[ls..t], &is_tab[ls..t])
        } else {
            w
        };
        (line_runs, w, fw)
    };

    let mut ls = 0usize;
    while ls < n {
        let first = opps.partition_point(|&o| o <= ls);
        let cand = |k: usize| opps.get(first + k).copied().unwrap_or(n);
        let cand_count = opps.len() - first + 1;

        // Furthest opportunity that fits, measured within the paragraph.
        let mut best: Option<usize> = None;
        for k in 0..cand_count {
            if fits(fit_width(ls, cand(k)), st.width, at_space(ls, cand(k))) {
                best = Some(k);
            } else {
                break;
            }
        }
        // Chrome then re-shapes the line by itself and, if it no longer fits,
        // steps back one opportunity at a time.
        let mut chosen: Option<(usize, Vec<Run>, f32)> = None;
        if let Some(mut k) = best {
            loop {
                let le = cand(k);
                let (runs, w, fw) = shape_line(ls, le);
                if fits(fw, st.width, at_space(ls, le)) {
                    chosen = Some((le, runs, w));
                    break;
                }
                if k == 0 {
                    break;
                }
                k -= 1;
            }
        }
        let (le, runs, w) = match chosen {
            Some(c) => c,
            None if st.wrap == Wrap::Normal => {
                let le = cand(0);
                let (runs, w, _) = shape_line(ls, le);
                (le, runs, w)
            }
            None => {
                // No opportunity fits: break inside the word, keeping at
                // least one grapheme so layout always advances.
                let mut g = ls + 1;
                while g < n && (width_from(ls, g + 1) * 64.0).floor() / 64.0 <= st.width {
                    g += 1;
                }
                // The scan uses paragraph positions, which include kerning with
                // the next grapheme, like Chrome's OffsetToFit on the paragraph's
                // shape result: "source xxx…x" is cut one "x" before the width
                // of the re-shaped line alone would allow. Positions are taken
                // on the 1/64 px grid, rounded down: a 280.005 px prefix fits
                // a 280 px box.
                loop {
                    // Never split an atomic box.
                    while g > ls + 1 && in_atomic(gbs[g]) {
                        g -= 1;
                    }
                    while g < n && in_atomic(gbs[g]) {
                        g += 1;
                    }
                    let (runs, w, fw) = shape_line(ls, g);
                    if fits(fw, st.width, false) || g <= ls + 1 || in_atomic(gbs[g - 1] + 1) {
                        break (g, runs, w);
                    }
                    g -= 1;
                }
            }
        };
        out.push(make_line(ctx, runs, abs + gbs[ls], abs + gbs[le], w));
        ls = le;
    }
}

fn make_line(ctx: &Ctx, mut runs: Vec<Run>, start: usize, end: usize, width: f32) -> Line {
    let st = ctx.st;
    // Bidi rule L2: reverse runs by level, then place them left to right.
    if runs.iter().any(|r| r.level > 0) {
        let levels: Vec<Level> = runs
            .iter()
            .map(|r| Level::new(r.level).unwrap_or(Level::ltr()))
            .collect();
        let order = ParagraphBidiInfo::reorder_visual(&levels);
        let mut x = 0.0;
        for &i in &order {
            runs[i].x = x;
            x += runs[i].width;
        }
    }
    // `text-align: start`. A right-to-left line sits against the right edge;
    // its trailing spaces, at the paragraph level, hang off the left. With no
    // box, the edge is the block's widest line, which `layout` aligns to once
    // every line is known (shifting by an unbounded width here would also
    // round every position away in f32).
    if ctx.base % 2 == 1 && st.width < UNBOUNDED {
        let total: f32 = runs.iter().map(|r| r.width).sum();
        let shift = st.width - total;
        for r in &mut runs {
            r.x += shift;
        }
    }
    // With a fixed line-height, Chrome sizes every line from the block's
    // primary font alone; fallback and emoji fonts do not change the line box.
    let (a, d) = db().primary(st.weight).rounded_metrics(st.size);
    let leading = st.line_height - (a + d);
    let half = (leading / 2.0).floor();
    Line {
        top: 0.0,
        height: st.line_height,
        baseline: a + half,
        width,
        runs,
        start,
        end,
    }
}

/// Appends "…" to a clamped line, dropping trailing graphemes until it fits.
fn ellipsize(ctx: &Ctx, line: &mut Line) {
    let st = ctx.st;
    let fonts = db();
    let face = fonts.primary(st.weight);
    let mut ell = Vec::new();
    shape::shape(face, "\u{2026}", st.size, false, false, None, &mut ell);
    let ell_w: f32 = ell.iter().map(|g| g.adv).sum();

    let mut end = line.end;
    // A space at the end of a soft-wrapped line hangs past the line's width,
    // but the ellipsis comes after it, which makes it interior: Chrome keeps
    // it and draws the `…` one space further right (`clamp-overflow` at 600:
    // 500px of content against our 497). It is dropped only when the line has
    // to give up clusters, which is what `trim` does below.
    let trim = |end: usize| line.start + ctx.text[line.start..end].trim_end_matches(' ').len();
    let (mut runs, mut width);
    loop {
        let sub = &ctx.text[line.start..end];
        let gbs = linebreak::grapheme_boundaries(sub);
        runs = shape_segment(ctx, line.start, sub, &gbs, usize::MAX);
        width = runs.iter().map(|r| r.width).sum::<f32>();
        if fits(width + ell_w, st.width, false) || end == line.start {
            break;
        }
        // Drop one grapheme and retry.
        let prev = gbs[gbs.len().saturating_sub(2)];
        end = trim(line.start + prev);
    }
    let span = if end > line.start {
        ctx.span_at(end - 1)
    } else {
        ctx.span_at(line.start)
    } as u16;
    let mut pen = 0.0;
    let glyphs = ell
        .iter()
        .map(|g| {
            let gl = Glyph {
                id: g.id,
                x: pen + g.dx,
                y: g.dy,
                adv: g.adv,
                dx: g.dx,
                span,
                cluster: end as u32,
            };
            pen += g.adv;
            gl
        })
        .collect();
    runs.push(Run {
        face: face.id,
        x: width,
        width: ell_w,
        glyphs,
        start: end,
        end,
        level: ctx.base,
        rtl: ctx.base % 2 == 1,
        script: 0,
    });
    let rebuilt = make_line(ctx, runs, line.start, end, width + ell_w);
    *line = rebuilt;
}

impl TextBlock {
    /// Per-line, per-span rectangles, comparable with DOM range rectangles.
    pub fn fragments(&self) -> Vec<Fragment> {
        let mut out: Vec<Fragment> = Vec::new();
        for (li, line) in self.lines.iter().enumerate() {
            let first_of_line = out.len();
            // A text box is as tall as the primary font's ascent plus descent,
            // which is the line box minus its leading.
            let (a, d) = db().primary(self.weight).rounded_metrics(self.size);
            let top = line.top + line.baseline - a;
            let bottom = line.top + line.baseline + d;
            for run in &line.runs {
                for g in &run.glyphs {
                    let gx = run.x + g.x - g.dx;
                    match out[first_of_line..]
                        .iter_mut()
                        .find(|f| f.span == g.span as usize)
                    {
                        Some(f) => {
                            let right = (f.x + f.w).max(gx + g.adv);
                            f.x = f.x.min(gx);
                            f.w = right - f.x;
                            let b = (f.y + f.h).max(bottom);
                            f.y = f.y.min(top);
                            f.h = b - f.y;
                        }
                        None => out.push(Fragment {
                            text: String::new(),
                            line: li,
                            span: g.span as usize,
                            x: gx,
                            y: top,
                            w: g.adv,
                            h: bottom - top,
                        }),
                    }
                }
            }
            // Text of each fragment: the part of its span that lies on this line.
            for f in &mut out[first_of_line..] {
                let sp = &self.spans[f.span];
                let s = sp.start.max(line.start);
                let e = sp.end.min(line.end);
                if s < e {
                    f.text = self.text[s..e].to_string();
                }
            }
        }
        out
    }

    /// The text that is visible after clamping.
    pub fn visible_text(&self) -> &str {
        match (self.lines.first(), self.lines.last()) {
            (Some(a), Some(b)) => &self.text[a.start..b.end],
            _ => "",
        }
    }
}
