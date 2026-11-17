# The painter: display list to pixels

`crates/xeet-core/src/paint/` turns a `display::DisplayList` into RGBA and PNG.
This note records what it does, the measurements behind each choice, and what
is still unverified.

```
display::DisplayList ──► rasterize(dpr, images) ──► Pixmap (RGBA8) ──► encode_png
                                                    └─► to_svg (approximate)
```

| File | Responsibility |
|---|---|
| `mod.rs` | Public API, the item loop, clips, mask blits, PNG/§ limits re-exports |
| `shapes.rs` | Rounded rects, squircle corners, border rings |
| `glyph.rs` | Glyph outlines → cached coverage masks |
| `emoji.rs` | COLRv0 layers → cached color sprites (Twemoji Mozilla) |
| `icons.rs` | X's SVG sprite: path data, arcs, gradients |
| `images.rs` | PNG/JPEG/WebP decoding, X's image-service copy of a card picture, `object-fit: cover`, scaling, placeholders |
| `encode.rs` | PNG encoding |
| `svg.rs` | Vector export for inspection |

## Rasterizer: `tiny-skia`

`tiny-skia` is a pure-Rust port of Skia's raster pipeline, which is the engine
Chrome itself draws with. The alternative (`vello_cpu`) is a GPU-style
compute rasterizer with a different anti-aliasing model and no Skia ancestry,
so `tiny-skia` was the better bet on agreement, and it needed no spike to
justify: the text oracle compared the two approaches' shape through real
glyphs within the first hour.

The canvas inside `rasterize` is a premultiplied `tiny_skia::Pixmap`, as in
Skia. `Pixmap::rgba` (the public type) is **straight alpha**: for an opaque
frame — the normal case, since the display list's background is opaque — the
two are identical and the bytes are handed out without a conversion pass.

## Text: masks, positions, gamma

* Outlines come from `skrifa` using the embedded face bytes, unhinted
  (`DrawSettings::unhinted`), FreeType point order (`PathStyle::FreeType`,
  which is what Chrome rasterizes through), y flipped into device space.
* Chrome places every glyph mask on a **1/4 px grid** in device space. That is
  measurable: `raster-text.mjs` with the subpixel bucket set to 2, 4, 8, 16 and
  64 divides per pixel scores 97.69%, **98.46%**, 98.11%, 98.17% and 98.19%
  respectively, so 1/4 is what Chrome quantizes to and what `glyph::SUBPIXEL`
  uses. The mask bakes the quantized offset in and is blitted at an integer
  position; the cache is keyed by (face, glyph, size, fx, fy).
* Coverage is blended in premultiplied sRGB with `(a·b + 127)/255` rounding,
  matching Skia's blend closely enough that the total ink of a text block
  agrees with Chrome to ~0.1%.
* The cache is bounded by bytes (12 MB), cleared wholesale when it overflows,
  and holds only public glyph coverage — nothing about a post, so it can never
  leak anything from one render into another.
* **No gamma correction is applied on top.** Sweeping `cov → cov^(1/γ)` for
  γ ∈ [1.0, 2.2] against Chrome made the match monotonically worse (γ = 1.0 is
  best), so the resolution is to match coverage, not to compensate for it.

### The macOS gap, measured

Chrome on macOS rasterizes glyphs through CoreText, which **dilates** them
("font smoothing"): the same 15 px text has ~13–17% more ink than a plain
coverage blend, with each edge pixel gaining ~0.2 of coverage and saturating
at 1.0. It is not a gamma curve and not a scale: total ink ratio is 1.14 at
dpr 1, 1.13 at dpr 2 and 1.13 at dpr 8, and the probe at dpr 8 shows
`ours 0.25 → chrome 0.425`, `0.5 → 0.729`, `0.75 → 1.0`.

X's stylesheet ships `.antialiased { -webkit-font-smoothing: antialiased }`;
with that class the dilation is gone and the ink ratio becomes 1.001. The text
oracle therefore always renders with `--smoothing antialiased`, which is also
what Chrome on **Linux** does by default (FreeType has no such dilation). The
Linux gate is the reference; on this macOS machine the text oracle lands
at **98.0%** of pixels within ±8/255 with **zero** differing pixels away from
glyph edges.

### Coverage: exact area, computed here rather than by tiny-skia

`tiny-skia`'s path anti-aliasing is the ceiling on text parity, and it is a low
one: it supersamples 4× in `y` only (`scan/path_aa.rs`, `SUPERSAMPLE_SHIFT = 2`),
so coverage has 16 levels, and its curve edges are evaluated by forward
differencing. Measured by filling a circle and comparing with dense analytic area
coverage (64×64 samples per pixel, `tmp/aa`): **3.2% of edge pixels more than two
levels off, up to 31**. Handing it a finely flattened polygon instead makes it
*worse* (2.38% → 2.88% on `text-paulg-2118783984616591857`), and raising the
supersampling changes nothing — the error is in where the edge sits, not in how
finely it is sampled.

`paint/coverage.rs` therefore rasterizes glyphs itself:

* every curve is flattened until its control points are within **0.005 device
  px** of the chord (so a polygon, which tiny-skia does handle exactly, is within
  a fraction of a level of the true outline);
* each pixel row is sampled 64 times, and at every sample the outline's spans are
  computed **exactly** — every edge's crossing, paired by winding, with no
  rounding of `x` — and each span adds its exact horizontal overlap;
* coverage is the sum of those overlaps, clamped to a pixel: plain `f32`
  arithmetic on values derived from the path, so the result is identical on every
  platform (wasm included) and independent of the platform's `libm`.

Cost: a glyph mask is cpu-cheap (a 15 px glyph is ~40 edges × 20 rows × 64
samples ≈ 50k operations) and cached per (face, glyph, size, phase), so a text
post still rasterizes in **0.29 ms** and rasterises + encodes PNG in **0.94 ms**
(`paint_bench`, 390 px wide at 3×). Small solid fills (icons, badges, 64×64
device px or less) go through the same rasterizer at 16 samples per row — icons
98.06% → 98.31% within ±8 at dpr 2 — while larger fills keep tiny-skia's scanline
AA, whose cost scales with the shape's height rather than its area (an exact-area
placeholder costs 45 ms instead of 3.6 ms).

With it, the e2e corpus (158 compared posts) moves from p50 1.479% / max 2.888%
to **p50 1.413% / max 2.809%** at tolerance 8 with a one-pixel slack, and 39 posts
land under 1% instead of 33.

### Baselines snap to whole CSS pixels

The glyph *x* is sub-pixel (the 1/4 px bucket above) but the glyph *y* is not:
Blink snaps a text run's baseline to a whole CSS pixel before painting. Measured
in Chrome (`--font-render-hinting=none --disable-lcd-text`) by nudging a line of
text with `position: relative` in steps of 0.1 px:

| dpr | offset 0.4 CSS px | offset 0.5 CSS px |
|---|---|---|
| 1 | no pixel changes | text moves 1 CSS px (1 device row) |
| 2 | no pixel changes | text moves 1 CSS px (2 device rows) |
| 3 | no pixel changes | text moves 1 CSS px (3 device rows) |

At dpr 3 an offset of 0.4 px is 1.2 *device* pixels, so the snap is in CSS space,
not device space. Horizontally the same probe changes pixels at every step, so
only `y` is snapped. `snap_baseline` in `paint/mod.rs` does this, which matters:
a quarter of the glyph runs in the e2e corpus (the 13 px action counts and
timestamps) carry a `.5` baseline, and without the snap they were painted half a
pixel low. It is a correctness fix rather than a big parity win — over 31 real
posts it changed 16 for the better and 2 for the worse, mean ratio 10.19% →
10.15% at tolerance 2 (see [e2e-raster.md](e2e-raster.md)).

## Shapes

* Rounded rects are cubic Béziers with the usual κ = 0.5522848; Chrome's round
  corners are exact circles (verified against the rendered boundary at dpr 8:
  within 0.05 px of an analytic circle).
* `CornerShape::Squircle` is CSS `corner-shape: squircle`, i.e.
  `superellipse(2)` in the CSS Borders 4 parameterisation, whose equation is
  `|x|^(2K) + |y|^(2K) = 1` with K = 2 → exponent 4. **Blink does not draw the
  superellipse itself**: it approximates each convex half-corner with a single
  cubic whose control points come from a symbolic regression (Chrome's
  "corner cases of implementing CSS corner-shape"), with
  `H = 0.5^(1/k)`, `P0 = (0,1)`, `P3 = (H,H)`, `P1 = (a,1)`,
  `P2 = (H−b, H+b)`, and `a`, `b` computed from
  `s = log2(k)` by the published fit (see `shapes::blink_controls`).
  Implementing the same construction, in the same orientation, took squircle
  parity from 32% (own sampling) and 91% (Bézier in the wrong corner
  orientation) to **99.86%** at radius 16 and 99.74% at radius 29.
* A border, inset hairline or inset shadow is the ring between the shape and
  the shape offset inward. Blink renders that as a **stroke clipped to the
  box**, and for a squircle the inner curve is a true parallel offset rather
  than another superellipse, so the painter does the same: it strokes the
  outline at twice the width into a scratch pixmap masked by the shape, then
  composites. That is also exactly the CSS rule for round corners
  (inner radius = r − width).
* Clips: a stack of shapes. All-rectangle stacks clamp glyph blits and images
  (cheap); anything else becomes a coverage mask, intersected on push.

## Color emoji

Twemoji Mozilla is COLRv0 (layered glyphs + CPAL), composed source-over into
one sprite per (glyph, size, subpixel) and cached.

Layout gives every emoji cluster **Apple's 1.25 em advance**
(`fonts::emoji_advance`), because posts are read on iOS and macOS, so line
breaking has to agree with them. Twemoji's own advance is 1 em, so the artwork
is **centered in that advance** instead of left-aligned: at 15 px the layout
advance is 19 and the artwork 15, so it starts 2 px in. The emoji oracle
confirms the artwork size and the centering against Chrome (which it feeds the
same Twemoji file): art widths are identical (15 px, 13.5 px for skin-tone
sequences) and the offset matches `(advance − art)/2` exactly.

Emoji outlines are saturated colour against white, so a single edge pixel can
differ by up to 178 levels (blue 77 → 255) even when the geometry is exactly
right, which is why a raw percentage reads 87–95%. The honest measures: ink
boxes identical (60×61 px at 4×), interior pixels identical, total ink within
0.2–1.9%, and **zero** differing pixels away from edges for seven of the eight
comparable sequences (52 pixels for 👍🏽, where the skin-tone layer's rim
differs slightly). 🩷 is reported as missing artwork: Chrome draws Apple's
pink heart, xeet draws the neutral box, and the oracle flags the size
mismatch instead of pretending the pixels are comparable.

Emoji newer than the bundled artwork (Emoji 15+, e.g. 🩷) have no glyph. The
text engine still lays them out; the painter draws a **neutral rounded box**
the size of the em square, centered in the advance, tinted from the text color
so it reads as "missing" in either theme. Twemoji's own `.notdef` is a narrow
vertical rectangle, which looks like a broken character rather than an emoji,
so it is not used. The emoji oracle lists every such codepoint it finds.

## Icons

`assets/vendor/icons.json` is X's own sprite (26 symbols, 24×24). The parser
covers what the sprite actually contains: `path` data with all commands
including elliptical arcs (converted to cubics), `circle`, `rect`, `fill`
(`currentColor`, hex or a `linearGradient` reference), `fill-rule`, and
`linearGradient`/`stop` in either `userSpaceOnUse` or `objectBoundingBox`
units. Anything else is ignored rather than guessed at, so a new icon type
renders as "missing" rather than wrong. Monochrome icons take the item's
color; the verified badges keep their own blue/gold.

## Images

Decoding is PNG (`png`), JPEG (`zune-jpeg`) and WebP (`image-webp`), all pure
Rust, all bounded by `Limits` **before** allocating (defaults: 8192 px per
side, 16.7 M pixels, 64 MB), with `DecodeError::{Unsupported, Malformed,
TooLarge}` and no panics on hostile input.

JPEG decodes into a buffer we allocate (`decode_headers` to size it, then
`decode_into`), which is ~5% faster than `decode()` and one 5.8 MB allocation
lighter. `DecoderOptions::new_fast()` was measured and rejected: 6% (~1 ms per
photo) in exchange for zune's internal SIMD/`unsafe` paths and a different
result at the pixel level. Plain `decode()` is not a win either. Decode is
~9–13 ms for a 1600×900 photo on this machine and is the dominant cost of a
real-media frame; zune-jpeg has no reduced-size DCT decode, so that is the
floor unless the decoder changes.

Scaling reproduces Chrome's `object-fit: cover` with a focal point:

* **Downscale** (device pixels per source pixel < 1): bilinear from a
  box-filtered mip level, `floor(log2(1/scale))` — Skia's `kLow` filter, which
  is what Chrome uses. Exact 2× and 4× ratios match Chrome **bit for bit**
  (100%); at a 0.75× device ratio this scores 97.5% within ±8, against 90.5%
  for an exact area average (which is the tempting but wrong answer here — the
  two were measured both ways to be sure).
* **Upscale**: bilinear from level 0. Chrome is slightly softer than bilinear
  on fine detail (it damps a one-pixel checkerboard ~10% more); bicubic
  (Catmull-Rom) and B-spline were both *worse* (80% and 81% overall), so
  bilinear stays.
* **Both cases run the same loop.** The mapping is an axis-aligned scale plus
  a translation, so the source coordinate and the two bilinear weights depend
  only on the destination column (or row) index; they are computed once per
  draw and the inner loop is integer taps with no floats, divisions or bounds
  checks. Levels are built lazily and only as far as needed, and for an opaque
  source — every JPEG — level 0 is the source itself, so a 1600×900 photo
  scaled into a 177 px cell pays for no pyramid at all. Opaque sources also
  skip premultiplication (straight alpha *is* premultiplied there); the
  opacity test reads eight bytes at a time.
* **One pass onto the canvas.** With no clip the sampled pixels are written
  straight into the pixmap (a `memcpy` of four bytes when the source is
  opaque); only a masked clip needs the intermediate sprite.
* The corner coverage mask is rasterized once per image and multiplied in as
  it is sampled.
* Cover crop: `s = max(dw/sw, dh/sh)`, offset `(sw·s − dw)·focus.x`, matching
  CSS `object-position: x%`. Corners are applied as coverage from the shape
  path.

## PNG

`encode_png` is `Compression::Fast` (fdeflate) with the `Sub` row filter;
`encode_png_best` trades time for size (`High` + `Adaptive`).

Measured on this machine, fast profile, 390 CSS px wide at 3× (1170×1236):

| Case | Median (fast profile) |
|---|---|
| text post: raster + PNG | **0.8 ms** |
| text post: raster only | 0.3 ms |
| text post: PNG only | 0.5 ms |
| 4 placeholder media: raster + PNG | 2.8 ms |
| 4 images 240×160: raster + PNG | 14.1 ms |
| 4 images: raster only | 6.9 ms |
| SVG export | 1.3 ms |

Real media, `paint_bench --jpeg <1600×900 photo>`, one frame with four photos
at 3× (cells 531 px wide). This machine is shared with other agents, so the
spread between median and best is large; the best-of-30 numbers are the least
disturbed and the medians were taken at load average 8–15:

| Case | fast profile | dev profile |
|---|---|---|
| decode, 4 × 1600×900 JPEG | 51–67 ms | 735 ms |
| raster only (scale + composite) | **11.5–18 ms** | 166 ms |
| raster + PNG | 22–24 ms | 304 ms |
| raster only, sources pre-scaled to the cell | 6.4–10.8 ms | 125 ms |
| PNG of that frame (2.8 MB of photo) | 9–24 ms | 390 ms |

Before this round the same bench measured 101 ms of raster for the four
photos in the fast profile and 8.1 s in dev: the per-pixel float bilinear,
a full-size premultiply pass and a double composite were the cost. Scaling is
now memory-bound (11 ms to sample four 5.8 MB sources into 1.1 M destination
pixels), and decode dominates the frame.

**Decode once, and never copy.** `ImageSource` has an optional
`rgba_ref` method; when a source implements it, the painter borrows the
decoded image instead of taking ownership of a copy (5.8 MB per photo per
draw). `render.rs` currently clones from its cache — filling the cache before
rasterizing and returning references would remove four 5.8 MB copies per
frame.

The budgets ask for < 5 ms for the text post (met), < 5 ms with four
placeholders (met) and 10–30 ms for four real images decoded and scaled. The
last one is decode-bound: four 1600×900 JPEGs cost ~50 ms to decode on their
own, so the target holds for media up to roughly 800×600 per photo and not for
1600×900 originals.

`paint_bench` prints median, best and p95 — use **best** on a shared machine.
Pass `--jpeg <file>` to add the real-media cases:

```
# any 1600x900 JPEG works; this is how the one used above was made
node -e '...'                       # or: sips -s format jpeg in.png --out photo.jpg
cargo run --profile fast --example paint_bench -- --jpeg photo.jpg --iters 30
```

Benchmark with `--profile fast` (or `release`). In a **dev** build the paint
loop runs at opt-level 0 and a single 1600×900 JPEG decode takes ~161 ms
against ~13 ms optimized, so a debug build measures the compiler, not the
painter — that is where a ~687 ms "four photos" reading comes from.

## Oracles

All five live in `tools/oracle/raster*.mjs` and compare against real Chrome
through `browser.mjs` (X's stylesheet, X's fonts, hinting off, LCD off):

```
cd tools && node oracle/raster-all.mjs --dpr 2
```

| Oracle | What it covers | Result on macOS (this machine, 2026-11-10) |
|---|---|---|
| `raster-text` | glyph rasterization at 2×, 30 corpus + 30 fuzz samples | 98.02% within ±8, **0** px off glyph edges |
| `raster-shapes` | fill, radii, squircle, border, hairline, clips | 99.79% |
| `raster-icons` | all 26 sprite icons at 24 px, 4× | 99.08% |
| `raster-emoji` | 9 sequences: artwork size, centering, colors | boxes identical, ink mass within 0.2–1.9%, 52 px off-edge across 8 sequences |
| `raster-images` | cover, focus, 2×/4×/0.75× downscale, upscale, radii | 93.92% |

The remaining differences are anti-aliasing on edges: our masks and Chrome's
disagree by a few levels per channel where curves meet pixels, because Chrome
rasterizes with FreeType/CoreText and we rasterize with Skia's rasterizer.
No pixel away from an edge differs in any oracle.

Those oracles all compare *our* geometry against Chrome's, so they answer "does
the painter rasterize like Chrome". A different question — "do the line boxes
and boxes `frame`/`inline` produce match Chrome's layout" — is what
`tools/ship/e2e.mjs` measures, and its `raster` bucket is analysed post by post
in [e2e-raster.md](e2e-raster.md): 26 of 27 posts there are CoreText dilation,
sub-pixel text placement or capture artifacts, none of them the painter.

While several streams edit `crates/xeet-core` at once, the crate sometimes does
not compile for reasons outside `paint/`. `tmp/paint-iter/` is a scratch crate
that symlinks the painter plus the modules it needs (fonts, display, inline, …)
and leaves out the rest, so the painter can still be built and measured:

```
cd tmp/paint-iter && CARGO_TARGET_DIR=../../target/agent-paint-iter \
  cargo build --profile fast --examples
XEET_PAINT_TEXT=../../target/agent-paint-iter/fast/examples/paint_text \
XEET_PAINT_FIXTURE=../../target/agent-paint-iter/fast/examples/paint_fixture \
  node tools/oracle/raster-all.mjs --dpr 2
```

**Not verified here:** the Linux gate. The reference is Chrome on Linux
at 2× with hinting off, and this machine has no Linux Chrome (the Docker
daemon is not running). The oracles are platform-agnostic — they launch
Playwright's Chromium, so the same commands run unchanged in CI — and on
macOS Chrome the text path needs `.antialiased` to remove CoreText's dilation
(as above). Expect the Linux numbers to be at least as good: FreeType has no
dilation and both rasterizers then compute plain area coverage.
