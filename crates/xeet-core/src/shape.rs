//! Text shaping: characters in, positioned glyphs out.
//!
//! Chrome shapes with HarfBuzz on every platform; `harfrust` is its Rust
//! port, so kerning, ligatures and contextual forms agree by construction.

use crate::fonts::Face;
use harfrust::{Buffer, Direction, Feature, ShapeOptions, ShapePlan, ShaperFont, Tag};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::BuildHasherDefault;
use std::rc::Rc;

#[derive(Clone, Copy, Debug)]
pub struct ShapedGlyph {
    pub id: u32,
    /// Byte offset, within the shaped string, of the cluster this glyph belongs to.
    pub cluster: u32,
    /// Advance and offsets in CSS pixels at the requested size.
    pub adv: f32,
    pub dx: f32,
    pub dy: f32,
}

thread_local! {
    static BUFFER: RefCell<Option<Buffer>> = const { RefCell::new(None) };
    static PLANS: RefCell<PlanCache> = RefCell::new(PlanCache::empty());
    /// Scratch glyphs for [`measure`]. Layout measures candidate line ends by
    /// the dozen, and none of the results outlive the call.
    static MEASURE_SCRATCH: RefCell<Vec<ShapedGlyph>> = const { RefCell::new(Vec::new()) };
}

/// What a shape plan depends on. Same input, same plan: the font's tables,
/// the direction, the resolved script and the user features.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PlanKey {
    generation: u32,
    face: u16,
    rtl: bool,
    script: Option<harfrust::Script>,
    ss01: bool,
    tnum: bool,
}

/// Hasher for [`PlanKey`]: a multiply-xor mix in place of SipHash.
///
/// Every shape call looks a plan up, and the key is a handful of small
/// integers, so the hash itself would cost more than the hit saves. Only
/// consistency matters here — equal keys must hash equal — and the map is
/// bounded at [`PlanCache::MAX`] entries.
#[derive(Default)]
struct PlanHasher(u64);

impl std::hash::Hasher for PlanHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (words, tail) = bytes.as_chunks::<8>();
        for &w in words {
            self.write_u64(u64::from_ne_bytes(w));
        }
        for &b in tail {
            self.write_u64(b as u64);
        }
    }

    #[inline]
    fn write_u8(&mut self, v: u8) {
        self.write_u64(v as u64);
    }

    #[inline]
    fn write_u16(&mut self, v: u16) {
        self.write_u64(v as u64);
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

/// Compiled shape plans, one per face, direction, script and feature set.
///
/// HarfBuzz builds a plan (`hb_shape_plan_t`) once per shape operation unless
/// the caller caches it; building one means walking every script's feature
/// list and compiling the lookup map, which costs about the same as shaping
/// thirty glyphs. Every layout of a post makes a dozen or two shape calls, so
/// the cache is the difference between paying that per call and per process.
/// Plans depend on the font's own tables and never on the string, so an entry
/// is immutable and can be shared.
///
/// The cache is per thread (no lock, and a plan is only ever used on the
/// thread that compiled it) and bounded: at capacity the oldest plan is
/// evicted, which frees it unless a shape call still holds it. A long-lived
/// process that saw an unbounded variety of scripts and features therefore
/// keeps a fixed amount of memory, not a growing one.
struct PlanCache {
    plans: HashMap<PlanKey, Rc<ShapePlan>, BuildHasherDefault<PlanHasher>>,
    /// Insertion order, for evicting the oldest plan at capacity.
    order: VecDeque<PlanKey>,
}

impl PlanCache {
    /// Enough for every face, script and feature combination a post can hold.
    const MAX: usize = 512;

    fn empty() -> Self {
        PlanCache {
            plans: HashMap::default(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, key: &PlanKey) -> Option<Rc<ShapePlan>> {
        self.plans.get(key).map(Rc::clone)
    }

    fn insert(&mut self, key: PlanKey, plan: Rc<ShapePlan>) {
        while self.plans.len() >= Self::MAX {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.plans.remove(&oldest);
                }
                None => break,
            }
        }
        if self.plans.insert(key, plan).is_none() {
            self.order.push_back(key);
        }
    }
}

/// A shape plan for `face` under the key, from the cache or newly compiled.
fn plan_for(
    face: &Face,
    rtl: bool,
    script: Option<harfrust::Script>,
    ss01: bool,
    tnum: bool,
) -> Rc<ShapePlan> {
    let key = PlanKey {
        generation: crate::fonts::generation(),
        face: face.id,
        rtl,
        script,
        ss01,
        tnum,
    };
    PLANS.with(|plans| {
        if let Some(plan) = plans.borrow().get(&key) {
            return plan;
        }
        let features = [
            Feature::new(Tag::new(b"ss01"), 1, ..),
            Feature::new(Tag::new(b"tnum"), 1, ..),
        ];
        let enabled = match (ss01, tnum) {
            (true, true) => &features[..],
            (true, false) => &features[..1],
            (false, true) => &features[1..],
            (false, false) => &[],
        };
        let plan = Rc::new(ShapePlan::new(
            &face.font,
            if rtl {
                Direction::RightToLeft
            } else {
                Direction::LeftToRight
            },
            script,
            None,
            enabled,
        ));
        plans.borrow_mut().insert(key, Rc::clone(&plan));
        plan
    })
}

/// Shapes `text` with one face. Appends to `out`.
///
/// `script` is the run's resolved script (see `script.rs`). Chrome always
/// hands HarfBuzz the script of the run, so digits inside Hebrew text are
/// shaped as Hebrew, which selects a font's script-specific kerning. Without
/// it the script is guessed from the text alone.
pub fn shape(
    face: &Face,
    text: &str,
    size: f32,
    ss01: bool,
    rtl: bool,
    script: Option<harfrust::Script>,
    out: &mut Vec<ShapedGlyph>,
) {
    shape_with(face, text, size, ss01, false, rtl, script, out);
}

/// [`shape`] with `tnum` (tabular figures) as well as `ss01`.
///
/// X puts `font-variant-numeric: tabular-nums` on engagement counts, and
/// tabular digits are wider than the default proportional ones: "1.2K" is
/// 28.25px at 13px with `tnum` against 25.17px without, which is the width of
/// the count in the captured markup. Nothing else in a post uses it.
#[allow(clippy::too_many_arguments)]
pub fn shape_with(
    face: &Face,
    text: &str,
    size: f32,
    ss01: bool,
    tnum: bool,
    rtl: bool,
    script: Option<harfrust::Script>,
    out: &mut Vec<ShapedGlyph>,
) {
    if text.is_empty() {
        return;
    }
    let mut buffer = BUFFER.with(|b| b.borrow_mut().take()).unwrap_or_default();
    buffer.clear();
    buffer.push_str(text);
    // The run's script when the caller knows it, otherwise the one the buffer
    // guesses from the text. The guess also sets a direction and no language,
    // and the direction is set below either way, so handing the script over
    // directly spares the buffer scan without changing the plan or the glyphs.
    match script {
        Some(s) => buffer.set_script(Some(s)),
        None => buffer.guess_segment_properties(),
    }
    buffer.set_direction(if rtl {
        Direction::RightToLeft
    } else {
        Direction::LeftToRight
    });

    let shaper = ShaperFont::new(&face.font);
    let features = [
        Feature::new(Tag::new(b"ss01"), 1, ..),
        Feature::new(Tag::new(b"tnum"), 1, ..),
    ];
    let enabled = match (ss01, tnum) {
        (true, true) => &features[..],
        (true, false) => &features[..1],
        (false, true) => &features[1..],
        (false, false) => &[],
    };
    // The plan must be compiled for the buffer's script, which is the guessed
    // one when the caller does not know it: `harfrust::shape` builds its own
    // plan from exactly these properties, so this is the same plan, reused.
    let plan = plan_for(face, rtl, buffer.script(), ss01, tnum);
    let options = ShapeOptions::new().features(enabled).plan(Some(&*plan));
    if harfrust::shape(&shaper, &mut buffer, options).is_ok() {
        // No scale is set on the shaper, so positions are in font units. We
        // convert them the way Chrome does, in 16.16 fixed point:
        //   - a glyph's own advance comes from the font backend as a float
        //     and is truncated to 1/65536 px;
        //   - positioning adjustments (kerning) are scaled by HarfBuzz with
        //     rounding.
        // The difference from plain float math is tiny, but it decides lines
        // whose width lands within 1/65536 px of the box edge.
        let upem = face.upem as i64;
        let x_scale = (size * 65536.0) as i64;
        let hb_scale = |v: i64| -> i64 {
            let scaled = v * x_scale;
            (scaled + if scaled >= 0 { upem / 2 } else { -upem / 2 }) / upem
        };
        let infos = buffer.glyph_infos();
        let positions = buffer.glyph_positions();
        out.reserve(infos.len());
        for (info, pos) in infos.iter().zip(positions) {
            let own = shaper.glyph_h_advance(harfrust::GlyphId::new(info.glyph_id)) as i64;
            let own_px = (own as f64 * size as f64 / face.upem as f64) as f32;
            let own_fixed = (own_px * 65536.0) as i64;
            let adjust_fixed = hb_scale(pos.x_advance as i64 - own);
            out.push(ShapedGlyph {
                id: info.glyph_id,
                cluster: info.cluster,
                adv: (own_fixed + adjust_fixed) as f32 / 65536.0,
                dx: hb_scale(pos.x_offset as i64) as f32 / 65536.0,
                dy: hb_scale(pos.y_offset as i64) as f32 / 65536.0,
            });
        }
    }
    BUFFER.with(|b| *b.borrow_mut() = Some(buffer));
}

/// Width in CSS pixels of `text` shaped with one face.
pub fn measure(face: &Face, text: &str, size: f32, ss01: bool) -> f32 {
    MEASURE_SCRATCH.with(|scratch| {
        let mut glyphs = scratch.borrow_mut();
        glyphs.clear();
        shape(face, text, size, ss01, false, None, &mut glyphs);
        glyphs.iter().map(|g| g.adv).sum()
    })
}
