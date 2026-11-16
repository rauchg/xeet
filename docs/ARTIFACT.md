# Artifacts

An **artifact** is one captured observation of something — a post, a pull
request — held as several coordinated representations of that same
observation, with every byte it refers to content-addressed:

| section        | answers                                                           | for         |
|----------------|-------------------------------------------------------------------|-------------|
| `views`        | what it looked like (PNG, with renderer, config, fidelity claim)  | humans      |
| `semantic`     | what it means: typed elements, relations, bounds into each view   | machines    |
| `provenance`   | where it came from, how, by which tools, from which preserved bytes | both      |
| `state`        | what was true *at capture time*, each fact with its basis         | both        |
| `capabilities` | what could be done, by which executor, with whose authority       | both        |
| `limitations`  | what was omitted, approximated, derived, inferred, unverified     | both        |
| `coverage`     | which aspects this adapter supports at all                        | both        |

The image is one projection of the artifact. It is not the artifact.

```bash
xeet artifact capture x tests/fixtures/posts/links-card.json -o post --width 390
xeet artifact capture github tests/fixtures/github/pr-open-reviewed.json -o pr --method fixture
xeet artifact verify pr          # integrity + byte-for-byte reproduction of the view
xeet artifact inspect pr         # human-readable account
xeet artifact capabilities pr    # what could be done, and with whose authority
xeet artifact compare a b        # what changed between two observations
xeet artifact project pr                         # what a text-only model receives
xeet artifact project pr --consumer vision       # the same, with the image to attach
xeet artifact project pr --focus reviews --max-chars 4000
xeet artifact invoke post retheme --input '{"theme":"dark"}' -o post-dark
xeet artifact annotate post reading.json -o post-read   # model-read content, as inference
xeet engine discover --outcome "understand a screenshot without vision" --accepts text/plain
xeet engine mcp --grant store:read,store:write   # the provider over MCP (stdio)
```

Two committed examples, verifiable offline: [`artifacts/x-post`](artifacts/x-post),
[`artifacts/github-pr`](artifacts/github-pr).

---

## 1. Assessment of xeet (what this was built on)

Checked against the code and tests, not the README.

**Reused directly, unchanged**

- The X renderer: `xeet_core::render::render` (`crates/xeet-core/src/render.rs`).
  The X adapter's view *is* its PNG — a test asserts byte equality
  (`the_x_view_is_the_existing_renderers_bytes`).
- The `Report` (`model.rs`): ruleset id, visible text, entities with UTF-16
  ranges, hidden ranges with reasons, attachment decision, warnings. This is
  already a decision log; the adapter maps it to semantics and limitations.
- The frame's named geometry (`frame::layout_debug`, `Geometry::boxes`): the
  boxes `examples/frame_parity.rs` gates against Chrome at 1/64 px. Until now
  only the oracle harness could reach them.
- Text layout and painting (`inline::layout`, `DisplayList`, `paint::rasterize_png`):
  HarfBuzz shaping, Blink line breaking, embedded fonts, no I/O. The generic
  view renderer is built on these.
- The I/O boundary of `xeet-resolve` (caller-supplied `Fetch`/`Clock`): the
  adapters follow the same rule — the caller supplies bytes and time.
- `xeet resolve` output (`{post, links, media, source, warnings}`): accepted as
  capture input; `source.url`/`source.id` become provenance.

**Changed in xeet-core**: one additive function, `render::frame_boxes`
(14 lines), exposing the existing geometry. Nothing else in the renderer moved.

**What prevented universal use**

- `Post` is the only content model; `Report` is X vocabulary (`entities`,
  `hidden`, `attachment`). Fine as an adapter's input, wrong as a core type.
- The renderer is the X frame. A second source needs either its own renderer
  or a generic one; there was none.
- No provenance beyond `Source {url, id, appVersion}`; no capture time (by
  design: the core has no clock); no notion of basis, state, or capability.
- "MCP" was tool *definitions* served over HTTP (`packages/xeet/src/mcp.js`),
  not a server.

**Guarantees that must not regress, and their status**

| guarantee | evidence after this change |
|---|---|
| Rust test suite | `cargo test --workspace`: 372 pass, 0 fail (baseline 348; +24 new) |
| No `unsafe` | `node tools/ship/check-unsafe.mjs`: new crates report none; both `#![forbid(unsafe_code)]` |
| Model/schema/limits/MCP contract | `node tools/ship/check-contract.mjs`: passes |
| Native = WASM bytes | renderer untouched; no workspace-wide dependency feature added (see §6); `cargo check -p xeet-wasm --target wasm32-unknown-unknown` passes. The wasm-pack determinism gate itself was **not** run here. |
| Chrome oracle gates | not run here (need Playwright Chrome + upstream assets); renderer code unchanged |

---

## 2. The contract (format `xeet-artifact`, version `0.1`)

Types: [`crates/xeet-artifact/src/model.rs`](../crates/xeet-artifact/src/model.rs).

### Package

```
artifact.json                  the document (pretty JSON, sorted keys)
assets/<sha256-hex>.<ext>      every view and every preserved input
```

Asset paths are derived from content (`assets/<hex>.png|json|txt|bin`). An
artifact's `id` is `sha256:` of the canonical document with `id` set to `""`.
Canonical = compact JSON, keys sorted by UTF-8 bytes at every level, **number
tokens kept exactly as written** (`canon::document_id_from_text`), so identity
never depends on a float parser. Same observation, same id: the debug and
optimized builds produce the same id for the same inputs.

### Basis — never promote an inference to a fact

Every element, relation and fact carries one of:

| basis | meaning |
|---|---|
| `source` | supplied by the source data as-is |
| `observed` | measured by the capturing tool |
| `derived` | computed by a declared deterministic rule (re-running gives the same answer) |
| `inferred` | non-deterministic or heuristic (a model, OCR) — must be declared, by subject, in `limitations.inferred` or verification fails |
| `unknown` | not known; the value must be `null`, and `null` only goes with `unknown` |

Neither adapter emits `inferred` anything. Verification enforces the rules.

### Semantic model

Elements use a closed universal vocabulary of kinds — `record identity text
heading timestamp link reference media quantity status label list group
message affordance` — plus namespaced extensions (`x.something`), reported as
such. Source meaning lives in namespaced `role`s and `attrs` (`x.body`,
`github.pr.title`, `x.range`), never in new core types. Elements form a tree
(`children`, one parent each, reachable from `root`); `relations` carry the
rest (`authoredBy`, `quotes`, `mergesInto`, `reviews`, `attachedTo`, …).
`bounds` place an element in a view, in that view's layout units, quantized
to 1/64. `visible: false` marks things in the data but not in the view (a URL
replaced by its card, the 13th file of a PR). Adapters choose element ids that
are stable across captures (`action.like`, `file:docs/CI.md`), which is what
makes `compare` meaningful.

### State

`state.asOf` equals `provenance.capture.capturedAt`; `state.temporal` is
always `historical-observation`. Each fact has a subject, key, value, basis,
optional `sourceTime`, and `volatile` (may have changed since). The format has
no way to say "current", on purpose.

### Capabilities — descriptions, never grants

`capabilityPolicy` is always `descriptive-only`. Each capability names its
kind (`navigate`, `projection`, `mutation`, `communication`), target element,
input JSON Schema, effects, required authority and who checks it,
preconditions, executor (`local`, `origin`, `user-agent`), verification
method, and availability. Verification rejects:

- a mutation or communication that is local-executable or has a local executor;
- a mutation or communication that names no authority, or `checkedBy: none`;
- anything local-executable that is not a local projection.

The only things that run from an artifact alone are **local projections**:
re-rendering the same preserved input differently (`expand`, `retheme`). The
result is a new artifact whose `provenance.derivedFrom` names its parent.
`xeet artifact invoke post like` refuses. The GitHub `merge` capability pins
`sha` to the observed head commit, so the origin refuses it if the PR moved
after the observation.

### Verification (consumer side)

`verify::verify` runs named checks — `format schema identity assets views
semantic basis state capabilities provenance coverage reproduction` — with no
network, clock, renderer or model, except reproduction, which takes an optional
`Reproducer` that re-derives each view from the preserved inputs and compares
hashes. That is the link between the visual and semantic representations: both
are outputs of declared deterministic steps over the same input bytes, and a
third party can re-run them. Every report also lists what verification
**cannot** establish: source authenticity, current truth, executability.

### Compatibility

Readers ignore unknown fields; identity covers them (it hashes the text as
written). Minor version = additive. A major bump changes the `format` check.
`docs/artifacts/*` are the compatibility fixtures, verified in CI by
`committed_examples_stay_readable_and_reproducible`.

---

## 3. Model-independent consumption

Vision is an optional consumer capability, not a prerequisite for
understanding an artifact. `project::render` (`crates/xeet-artifact/src/project.rs`)
turns one artifact into a deterministic projection for a **text-only** or a
**vision-capable** consumer, in a line-oriented text form or as typed JSON.
No model produces it and none is needed to read it.

What a text-only consumer receives (`xeet artifact project docs/artifacts/x-post`):

```
ARTIFACT sha256:ffa6… (xeet-artifact.projection 0.1) for a text-only consumer
SUBJECT x.post "@rauchg: Two links, one card: vercel.com/blog/the-thing and"
OBSERVED 2026-11-16T12:00:00Z via fixture by xeet-capture/x 0.1.0; source x; authenticity unverified
VIEW v0 image/png 390x314 layout units …; image not included; nothing below requires it unless marked [visual-only]
ELEMENTS
  post record x.post "Two links, one card: …" @v0(0,0 390x313.25) …
      author.name text x.author.name "Guillermo Rauch" @v0(64,12 119.56x20)
      author.avatar media x.author.avatar [visual-only] @v0(16,12 40x40) …
    body text x.body "Two links, one card: …" ~derived @v0(64,34 310x40) …
      entity.1 link x.url "github.com/vercel/next.js" = "https://…" ~derived [not in view] …
LAYOUT rows top to bottom, items left to right
  v0 y 12-52: author.avatar | author.name | author.badge | author.handle
  v0 y 273.25-309.25: action.reply | action.repost | action.like | action.views | action.bookmark | action.share
STATE as of 2026-11-16T12:00:00Z
  post.likeCount = 2100 (may have changed)
  body.showMore = "none" ~derived
CAPABILITIES (descriptions, not grants) …
LIMITATIONS …
CONSUMERS
  text-only: 14 of 17 elements carry their meaning as text and 2 are structural; 1 exists only as pixels …
```

How the requirements are met:

| requirement | mechanism | test |
|---|---|---|
| vision optional; no OCR or second model | every element's text, value, unit, attributes, bounds and basis are in the projection | `the_text_projection_carries_every_meaning_without_the_image` |
| vision consumers get both | `--consumer vision` (MCP: image block + the same text) | `text_and_vision_consumers_get_the_same_artifact_differently`, CLI MCP test |
| same artifact, stable ids | projections are functions of the artifact; ids are the artifact's | same tests; text and vision forms differ only in the view line |
| inferred never passes as verified | basis mark on every element, relation and fact; `annotate` accepts **only** `inferred` content, names who/how/when, and produces a new artifact `derivedFrom` the original | `pixel_only_content_is_named_and_can_only_be_added_as_inference` |
| spatial organization without pixels | bounds per element; `LAYOUT` rows (vertical overlap ≥ ½ the shorter extent, left to right) | `layout_rows_preserve_spatial_organization` |
| deterministic, constrained-context serialization | text form; `maxText` per string, `maxChars` total; budget fill by fixed priority: header → top-level elements → state → capabilities (full, then compact) → limitations → relations → layout → consumers → deeper elements; stops at the first section that does not fit, so a larger budget never keeps fewer sections | `projections_are_deterministic_in_both_forms`, `a_character_budget_is_met_by_priority_and_never_silently` |
| partial requests keep provenance | `focus` (with ancestors as context), `depth`, `sections`; the context header is never dropped; everything left out is listed by id or section, with the reason | `a_focused_projection_keeps_context_and_says_what_it_left_out` |
| compare consumer capabilities | `consumer_coverage`: elements whose meaning is text, structural, or visual-only, and which are inferred | coverage assertions in the tests above |
| no LLM in the path | pure Rust over the artifact | — |

**Visual-only means unknown, not guessed.** An element whose content exists
only as pixels (an image without alt text; a chart without its data) is marked
`[visual-only]`. A text consumer is told it cannot know it. A vision consumer
is told anything it reads is inference. If a vision model or OCR does read it,
`annotate` records the reading as `!inferred` elements and facts, declared in
`limitations.inferred` with tool, method and time, in a new artifact whose
views and inputs are byte-identical to the original (so reproduction still
proves the pixels were not touched). An annotation cannot edit or override
anything observed.

Measured (examples in `docs/artifacts`, optimized build):

| | X post | GitHub PR |
|---|---:|---:|
| view PNG (scale 1) | 37.8 KB | 221 KB |
| `artifact.json` | 18.0 KB | 27.4 KB |
| text projection, complete | 6,134 chars | 10,890 chars |
| JSON projection, complete | 15,642 chars | 26,060 chars |
| elements with meaning in text / structural / visual-only | 14 / 2 / 1 | 27 / 0 / 0 |
| `project` wall time (CLI, incl. process start) | — | 3.8 ms |

Budget behavior on the PR (text form, `--max-text 80`): 1,800 chars → header
and 3 elements; 2,500 → 11 elements; 6,000 → state, compact capabilities,
limitations, relations and 18 elements; ~9,000 → everything with full
capability schemas. Below ~1,600 chars even the minimal projection does not
fit, and it says so (`budgetExceeded`) rather than truncating silently.

What this does **not** do yet: it has not been measured with actual models
(e.g. question-answering accuracy of a text-only model on the projection vs a
vision model on the PNG). That is the experiment that would quantify the
"capability equalizer" claim; the deterministic coverage report is the
precondition for running it fairly.

## 4. The engine as a provider: discovery and agent interoperability

`crates/xeet-engine` exposes the representation engine as a provider whose
capabilities are **contracts, not tools**. A contract is data
([`contract.rs`](../crates/xeet-engine/src/contract.rs)): id and version,
summary, the outcomes it serves (in an agent's words), input modalities,
output **modes** (which modalities come back, and which input fields select
them), input and output JSON Schemas, effects, required authority,
preconditions, guarantees, limitations, constraints and error codes. The
manifest ([`engine.manifest.json`](engine.manifest.json), checked against the
build by a test) adds provider identity, interfaces, compatibility, the
authority model, supported sources with their adapters, coverage and
fidelity, and **non-capabilities**: outcomes the provider is asked for but
does not provide, with why and the honest alternative.

Capabilities: `representation.capture`, `ingest`, `inspect`, `understand`,
`render`, `derive`, `verify`, `compare`, `retrieve`, `annotate`.

**One execution path.** `Engine::invoke(capability, input, session)` checks,
in order: input size, input schema (errors carry JSON pointers), the
session's authority, then runs the capability, then validates its own result
against the declared output schema. It returns an `Outcome` with
`inputDigest`, `resultDigest`, the artifacts read or created, and the scopes
checked. Anything that creates an artifact verifies it (with byte-for-byte
reproduction) before storing it in a content-addressed store
(`<store>/<hex>/`, URIs `artifact://sha256/<hex>`).

**Three separations, enforced in code:**

- *Discovery is not invocation.* `discover`/`describe` are pure functions of
  manifests; they cannot call anything and take no session.
- *Invocation is not authorization.* Scopes (`store:read`, `store:write`,
  `fs:read` limited to host-given read roots) come only from the host's
  `Session` (`--grant`, `--allow-read`); the default is `store:read`. Every
  call checks its own scopes and names the missing ones. Discovering a
  capability, or a provider being available, grants nothing.
- *No capability acts at a source.* Nothing modifies or deletes; the only
  effect is adding a verified artifact. Source actions (merge, reply, like)
  are described inside artifacts with their authority, and `derive` refuses
  them.

**Discovery and negotiation.** An agent asks by outcome and by the modalities
it can consume: `--outcome "understand a screenshot without vision" --accepts
text/plain` returns `representation.understand` with mode
`{consumer: text, format: text}`, and reports `representation.render` as
excluded by modality (it only yields `image/png`). Matching is deterministic
keyword overlap with declared outcomes — not semantic search, and the
manifest says so. When a query hits a declared non-capability ("OCR text from
a photo", "merge a pull request"), discovery returns an explicit
`notProvided` with the alternative, and its note warns that remaining matches
only share words. Several manifests can be searched together (`--peer`); the
same contract id from two providers is listed per provider (tested with a
hypothetical peer fixture, `tests/fixtures/engine/peer-vision.manifest.json`).

**Progressive disclosure.** The MCP server lists three tools by default —
`engine_discover`, `engine_describe`, `engine_invoke` — or every capability as
its own tool with `--tools all`. Measured: initial `tools/list` 1.8 KB
(progressive) vs 8.4 KB (all); a top-3 discovery 1.8 KB; one full contract
5.5 KB; the full manifest 39 KB.

**MCP specifics.** Artifacts are resources (`resources/list`; templates for
the document, `…/projection/{text|vision}` and `…/view/{id}`). Results carry
`structuredContent` (the outcome) and text content; image blocks appear only
when the caller asked for pixels (`render`, or `understand` with
`consumer: vision`). A text-only client never receives an image. Errors come
back as `isError` with the outcome's error code, message and details.

**Transport independence.** MCP and the CLI (`xeet engine call ID --input
JSON`) are layers over `Engine::invoke`; a test shows the CLI and MCP return
the same `resultDigest` for the same input. HTTP, AppPort or another agent
protocol would be another layer; none is implemented yet.

What is not done: no HTTP transport; discovery of the provider itself relies
on the host configuring the MCP server or reading the manifest (no registry
or network announcement); grants are coarse scopes per session, not
per-artifact or per-user; there is no rate limiting beyond the input-size
limit and the renderer's own budgets.

## 5. Architecture

```
          source bytes + capture time (caller-supplied; adapters never fetch)
                 │
   ┌─────────────┴──────────────┐
   │ xeet-capture (adapters)    │   x.rs       → xeet-core X renderer (unchanged)
   │                            │   github.rs  → view.rs (generic document renderer)
   └─────────────┬──────────────┘
                 │ Package (artifact.json + assets/)
   ┌─────────────┴──────────────┐
   │ xeet-artifact (universal)  │   model · canon · verify · compare · inspect
   │ deps: serde, serde_json,   │   knows no source and no renderer
   │       sha2                 │
   └─────────────┬──────────────┘
                 │
   ┌─────────────┴──────────────┐
   │ xeet-engine (provider)     │   contracts · manifest · discover · invoke
   │                            │   authority per session · content-addressed store
   └─────────────┬──────────────┘
                 │
   CLI `xeet engine …` / `xeet artifact …` · MCP `xeet engine mcp` · Rust library
```

| layer | lines | source-specific? |
|---|---:|---|
| `xeet-artifact` (format, canonical identity, package I/O, verify, compare, inspect, projections, annotation) | 3,134 | no |
| `xeet-capture/view.rs` (generic block-document renderer) | 270 | no |
| `xeet-capture/lib.rs` (shared helpers, reproducer dispatch) | 109 | no |
| `xeet-capture/x.rs` | 627 | X |
| `xeet-capture/github.rs` | 663 | GitHub |
| `xeet-engine` (contracts, manifest, discovery, schema validation, store, invoke) | 1381 | no |
| CLI + MCP transports (`commands/artifact.rs`, `commands/engine.rs`) | 749 | no |
| `xeet-core` change | 14 | — |

**Adding a source** = one adapter module that maps source bytes to elements,
facts, capabilities and either view blocks (generic renderer) or its own
renderer, plus a `Reproducer` arm. The GitHub adapter was added without any
change to `xeet-artifact`; the core's checks apply to it unchanged
(`both_sources_share_one_contract`).

---

## 6. Evidence

Tests (`crates/xeet-capture/tests/artifacts.rs`, `crates/xeet-cli/tests/artifact_cli.rs`,
`crates/xeet-artifact/src/canon.rs`):

- every X fixture × 2 themes: capture → disk → read → verify with reproduction;
- the X view equals `xeet render`'s bytes; body bounds equal the frame's `text` box;
- capture is deterministic (same inputs → same id), for both sources;
- the PR artifact passes the same verifier; painted elements have bounds,
  in-sentence tokens do not; `reviewDecision` is `derived`, null `mergeable`
  is `unknown`; `merge` is delegated, authority-bearing, head-pinned;
- tampering is caught: a flipped pixel byte (assets), an edited word (identity),
  a swapped source input that is re-sealed consistently (reproduction);
- an artifact that marks `merge` local-executable fails; undeclared inference fails;
- `expand` produces a verified derived artifact; `like` is refused;
- `compare` reports state changes (open → merged) and removed capabilities, and
  a re-theme as different pixels with **no** semantic change;
- CLI round trip and exit codes; MCP initialize / tools/list / tools/call / errors;
- committed examples stay readable and reproducible.

Measurements (CLI wall time, optimized `fast` profile, n=20, includes ~3 ms
process start, this container):

| operation | p50 |
|---|---:|
| `xeet render` (existing; pizzahut, 390 px @3×) | 26.8 ms |
| `artifact capture x` (same post) | 28.2 ms |
| `artifact capture github` (fixture, 600 px @2×) | 44.8 ms |
| `artifact verify` x, with reproduction | 27.5 ms |
| `artifact verify` x, integrity only | 4.2 ms |
| `artifact inspect` | 3.6 ms |

Sizes: the X artifact document is 17.6 KB beside a 281 KB view; the PR
document 27.4 KB beside a 561 KB view (@2×). The structured representation is
~3–6 % of the package; the pixels dominate.

Semantic coverage of the two examples: X post — 16 elements, 14 with bounds,
14 facts (8 source, 4 derived, 2 unknown), entity bounds absent (declared).
PR — 27 elements, 23 with bounds, 9 facts, 7 capabilities (4 need GitHub
authority).

---

## 7. What is not done, and known limits

- **The GitHub fixture is constructed**, in REST API shape; it is not a recorded
  response (it says so in `_fixture`). This session had no API access to a
  repository with pull requests. Capturing a real PR is one `curl` per endpoint
  into a bundle; nothing in the adapter changes.
- **Live X resolution was not exercised** here: this environment's TLS proxy is
  not in `xeet-resolve-net`'s root store. The `xeet resolve` → capture path was
  tested offline (`--no-links`).
- **No WASM/JS surface yet.** The artifact crates are pure Rust with no I/O in
  the core and should compile to WASM, but `packages/xeet` does not expose them.
- **Entity bounds** (X) and token bounds inside a sentence (GitHub summary) are
  not computed; both are declared as omitted.
- **The GitHub view is generic**: markdown is shown as text; github.com's UI is
  not replicated (`fidelity.claim: generic`). Inline review threads, checks and
  statuses are not read.
- **Authenticity is always `unverified`.** Nothing signs source data or artifacts.
- The package is a directory; there is no single-file transport yet.
- Reproduction depends on the renderer version: an artifact made by an older
  ruleset reproduces only with that ruleset. Integrity checks do not.

## 8. One decision worth recording

Identity first failed a disk round trip: `serde_json`'s default float parser is
one ulp off on some 17-digit values. Turning on `serde_json/float_roundtrip`
fixed it but was rejected: Cargo unifies features across the workspace, so the
native build would parse floats differently from the WASM build — a quiet
threat to xeet's native/WASM byte identity. Instead, identity is computed from
number tokens as written, and geometry is quantized to 1/64 px (Blink's
LayoutUnit, and the parity gate's tolerance).

## 9. Integration boundaries (no dependencies in the core)

- **Capability host (AppPort-style):** the manifest and contracts are the
  integration surface — versioned, transport-free, with schemas, modalities,
  authority and non-capabilities. A host maps its own grants onto the
  session scopes and calls `Engine::invoke` (or the CLI); nothing is redefined.
- **Durable store (FeltDB-style):** the package is already content-addressed
  (`id`, asset hashes), versioned (`formatVersion`), linked (`derivedFrom`),
  and temporal (`capturedAt`, `state.asOf`). A store keys artifacts by `id`,
  assets by hash, and history by `subject.reference` + `capturedAt`.
- **Controlled execution (AppBoundry-style):** adapters and renderers are pure
  functions of (bytes, time, options); they need no network and no filesystem
  beyond what the host hands them.
- **Retrieval/assistant (AnywhereAide-style):** the semantic graph, facts and
  `compare` give an agent structure to search and reason over; the views give
  people the evidence. Neither needs a model to be read or checked.

## 10. Next highest-value step

Capture **real** observations over time: a small fetcher (outside the core,
like `xeet-resolve-net`) that turns a PR URL into a REST bundle with the
caller's token, then re-capture the same PR and X post periodically and use
`compare` as a change log. That turns the format from "proven on fixtures" into
"proven on reality", and exercises the parts fixtures cannot: volatile state,
authority-pinned capabilities (`merge` with `sha`), and long-lived reproduction.
