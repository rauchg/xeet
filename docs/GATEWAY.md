# Modality gateway

A model receives information in modalities it may not consume: an image for
a text-only model, audio for a model without audio input. The gateway makes
that a deterministic infrastructure concern. Given the user's instruction, the
inputs, and a profile of what the target model accepts, it

1. inspects each input by content (magic bytes, not filenames),
2. decides per input whether the model accepts it (from the profile),
3. passes accepted inputs through unchanged,
4. for the rest: reuses an existing artifact for the same bytes, or discovers
   providers by contract and modality, ingests, reads (if an authorized reader
   exists), records the reading **as inference**, validates every artifact by
   digest, and projects it as text within the model's context budget,
5. injects those projections **beside** the unchanged instruction,
6. sends through the runtime's model client, and on a *confirmed* modality
   rejection re-prepares once and resends,
7. records every decision, with digests instead of content.

No model decides any of this.

## Architecture decision (Phase A)

This repository owns no model runtime, so the gateway does not pretend to
inject into one. It defines the boundary a runtime plugs into:

| responsibility | owner | code |
|---|---|---|
| execute capabilities | providers, via contracts (`Provider::invoke`) | `xeet-engine` (`Engine` implements `Provider`) |
| what the model accepts | the host's `ModelProfile` | `xeet-gateway/src/model.rs` |
| inspect, decide, coordinate, budget, inject, record | the gateway | `xeet-gateway/src/gateway.rs`, `inspect.rs` |
| talk to the model | the runtime's `ModelClient` | `xeet-gateway/src/request.rs`; one concrete client in `openai.rs` |
| grant authority | the host's `Session` | unchanged from `xeet-engine` |

What changed in existing code, and why:

- `xeet-engine`: a `Provider` trait (`manifest()` + `invoke()`) so the gateway
  calls *any* provider through its contract; `check_path` and `missing_scopes`
  made public so every component applies the same rules; `representation.capture`
  gained an `image` source; `representation.retrieve` gained `inputDigest`
  (reuse through a contract, not by reading the store); `representation.understand`
  now also returns a `delivery` report (what was delivered and omitted).
  Nothing about discovery, sessions or the store's guarantees changed.
- `xeet-capture/image.rs`: identifies PNG/JPEG/GIF/WebP by content, reads
  dimensions from headers, keeps the original bytes as the view
  (`fidelity: original`), and declares the content **visual-only**. Xeet
  does not read pixels and does not claim to; `docs/engine.manifest.json`
  still lists OCR and image interpretation as non-capabilities.
- `xeet-artifact`: `FidelityClaim::Original`; `project::render_with_report`.

The gateway knows contract ids — `representation.retrieve`,
`representation.capture`, `vision.read`, `representation.annotate`,
`representation.understand`, `representation.inspect` — not providers.
Candidates come from `discover` over all providers' manifests (the same
deterministic keyword-and-modality matching, not semantic search), ordered by
discovery and then host order; each is checked against the session before it
is invoked, and invoked again under the provider's own checks. Results are
validated against the provider's declared output schema, and every artifact a
provider claims to have made is fetched and re-hashed.

## Model capability profiles

`model-input-capabilities` 0.1 (`ModelProfile`): provider, model, revision,
per-media-type rules (`image/png`, `image/*`, `*/*`) with support
`supported | unsupported | conditional | unknown` and a source
`declared | provider-reported | configured | verified | observed-rejection`,
size and dimension limits, context and output tokens, a host-configured
characters-per-token estimate, structured-input support. There is no built-in
table of vendor models: support is never inferred from a name. The host's
configuration overrides provider metadata (`with_host_config`). Unknown support
is transformed by default (`UnknownPolicy::Transform`) or sent and left to the
fallback (`Send`). Examples: `tests/fixtures/gateway/profile-*.json`. `from_ai_gateway` builds a
`provider-reported` profile from Vercel AI Gateway's `/v1/models` metadata
(`xeet gateway profile ai-gateway:MODEL`).

## What a text-only model receives

The instruction is the first content item, byte for byte. Each represented
input follows as a text part:

```
[attachment 1 of 1: image/png 480x356 "invoices.png", sha256:15b81b83…]
This model cannot take image/png directly, so the original is not included; it is
kept by the runtime as artifact://sha256/… . Below is a text projection of it (a
reader's output, marked !inferred: read from pixels, may be wrong). Marks: … For more
detail, request artifact://sha256/… with an element id (focus) or a section.
ARTIFACT … OBSERVED … authenticity unverified
ELEMENTS
  image media image.file ^observed [visual-only] @v0(0,0 480x356) …
    reading list vision.table "Invoices table …" !inferred …
      reading.INV-1041 group vision.table-row "INV-1041 · Northwind Traders · $1,200.00 · due 2026-10-01 · status: Overdue" !inferred @v0(16,146 448x34) {"vision.confidence":0.96,…}
      reading.INV-1045 group vision.table-row "… · status: unreadable (cut off: \"Overd…\")" !inferred … {"vision.confidence":0.55,"vision.uncertain":"the status text is cut off …"}
LIMITATIONS
  inferred [reading.INV-1041] "content read from pixels, not supplied by the source" — "read by test-harness:hand-authored-reader 0.0.1 (…) at …"
…
```

Without an authorized reader the same structure arrives with the content
declared unknown (`[visual-only]`) and only what is provable: format,
dimensions, digest, where the original is kept. Nothing is invented. Inputs no
provider can represent (audio today) produce a "Not delivered" part naming the
type, size, digest and reason; they are never silently dropped.

Progressive retrieval: `Gateway::expand(artifact, focus, sections, maxChars)`
(CLI `xeet gateway expand`) returns more of an artifact for a follow-up turn
or a tool call the runtime exposes to the model.

## Rejection fallback

`Classifier` is ordered data. Authentication, quota, rate limit and context
overflow are checked **before** modality; a modality classification requires a
matching phrase (status alone never suffices). Only `unsupported-modality`
triggers a fallback, only if the rejected request actually carried media of
that family, and at most `max_fallbacks` (default 1) times. The rejected media
type is marked `observed-rejection` for the rest of the run only; persisting it
is the host's decision. Success is recorded only when the client returns a
response.

## Evidence

Every run returns a `modality-gateway.record` (id = digest of its canonical
JSON): instruction digest and length (not text), the model profile and its
digest, providers, and per attempt: each input's inspection (detected vs
declared type, size, digest, dimensions), the acceptance rule and its source,
the decision and reason, every step (discovery query, candidates, skipped
candidates with reasons, selected provider and contract version, input and
result digests, errors, declared non-capabilities), the artifact delivered,
whether it was reused, the projection's delivery report (chars, elements
included, omitted elements and sections, inferred and visual-only ids), the
budget arithmetic, a request digest and a content summary (kinds and sizes),
the outcome with classification and a 300-character error excerpt, and the
fallback decision. No image bytes or instruction text are in the record (a
test checks).

## Tests (`crates/xeet-gateway/tests/gateway.rs`)

Stand-ins, labelled as such: a vision reader that returns a hand-authored
reading of `tests/fixtures/gateway/invoices.png` (generated by
`examples/make_invoice_fixture.rs`), a text-only model that rejects media and
answers only from text, a vision model, and failing models.

| scenario | result |
|---|---|
| image + text-only model + reader | answer `Overdue: INV-1041, INV-1043. Unclear: INV-1045.`; instruction unchanged; no media sent; steps reuse→ingest→validate→read→annotate→validate→project |
| vision model | the original bytes are sent; no transformation |
| same image again | reused by input digest; same request digest |
| audio, no provider | "Not delivered" part; task still runs |
| no reader | content declared unknown; nothing invented |
| profile wrongly claims image support | rejected (modality) → fallback → accepted; `observed-rejection` recorded |
| real 429 quota, 429 rate limit, 401, context overflow, 500 | one send each, no fallback |
| model keeps refusing | exactly two sends, then stop |
| small context | budgeted projection, omissions stated, `expand` retrieves an omitted row |
| missing `vision:read` / `store:write` / path outside roots | reader skipped / ingest skipped / refused before reading |
| provider returns content that does not hash to its id | `invalid_artifact_digest`, not delivered |
| declared `image/jpeg`, bytes are PNG | detected as PNG; mismatch reaches the model as unverified |

MCP (`crates/xeet-cli/tests/artifact_cli.rs`): an agent discovers
`modality.adapt` from "the model does not support images" with
`accepts: ["text/plain"]`, invokes it, and receives only text blocks.

## Live runs against real models (Vercel AI Gateway)

With the session's `AI_GATEWAY_API_KEY`, every piece ran for real; records,
answers and the delivered artifact are in
[`evidence/gateway-live-2026-11-16/`](evidence/gateway-live-2026-11-16/).

```bash
xeet gateway run --client ai-gateway \
  --model ai-gateway:deepseek/deepseek-v3.1 \
  --reader ai-gateway:openai/gpt-4o-mini \
  --grant store:read,store:write,fs:read,vision:read --allow-read tests \
  --attach tests/fixtures/gateway/invoices.png \
  --instruction "Look at this screenshot and tell me which invoices are overdue."
```

`--model ai-gateway:…` builds the target profile from the gateway's model
metadata; `--reader ai-gateway:…` adds a real `vision.read` provider, used
only because `--grant` includes `vision:read`.

| run | target model | what happened |
|---|---|---|
| 1 | deepseek/deepseek-v3.1 (text-only, provider-reported) | ingest → read by gpt-4o-mini → annotate → project; answered INV-1041, INV-1043, and INV-1045 noting its status is cut off |
| 2 | same, host config wrongly claims images | real 400 `does not accept image input` → one fallback → reused the reading → answered |
| 3 | openai/gpt-4o-mini (vision) | original image sent; answered directly |
| 4 | meta/llama-3.1-8b, wrong host config | real 400; matched evidence `doesn't support the image content block` → one fallback → answered |
| 5 | amazon/nova-micro (text-only) | reused run 1's reading, no second vision call; answered |

What the live runs taught, and what changed because of them:

- **The classifier was wrong before it saw real errors.** The phrases taken
  from public error texts matched neither real rejection (DeepSeek: "does not
  accept image input"; Nova: "doesn't support the image content block").
  Both are now rules citing their recorded fixtures.
- **The top-level message can hide the cause.** For Llama the gateway's
  top-level error is "Bad Request"; the evidence is in the routed provider
  attempts inside the body (Bedrock refused the image, then Novita returned
  "Bad Request"). The classifier reads the whole body and records the matched
  phrase, so a reviewer can see where the evidence came from. A body with
  only "Bad Request" is still not treated as a modality error.
- **Vision readings are good at text and bad at coordinates.** gpt-4o-mini
  transcribed every line correctly and marked the cut-off "Overd..." as
  uncertain, but placed every row about 85 px too high at self-reported
  confidence 1.0. The reader's contract now says so; boxes and confidences stay
  `!inferred`.
- **Other real non-modality errors** were recorded and are classified without
  fallback: OpenAI 429 `insufficient_quota`, AI Gateway 403 free-tier
  restriction.

Recorded fixtures: `tests/fixtures/gateway/ai-gateway-*.json`,
`openai-429-insufficient-quota.json`. Test fixtures are classified on what
the provider sent only (the `_recorded` notes are stripped first).

## Measurements (optimized build, this container)

| | |
|---|---:|
| screenshot | 80,956 B PNG; 107,944 base64 chars to a vision model |
| text-only, with reader | 6,107 chars injected (~1,526 tokens at 4 chars/token, an estimate) |
| text-only, without reader | 2,633 chars |
| prepare, fresh (reuse miss, ingest, read, annotate, 3 validations, project) | 9.9 ms |
| prepare, reused (lookup, validation, project) | 1.7 ms |
| live run 1, end to end (ingest, real vision read, text model answer) | 18.4 s |
| live runs 2, 4, 5 (reading reused; fallback included in 2 and 4) | 1.8–4.3 s |

## Capture

Screenshots enter through the same derive path (`Gateway::derive`), with local
OCR as a `vision.read` provider; reuse is by input digest, capture context and
reader identity. See [`CAPTURE.md`](CAPTURE.md).

## Not done

- Readers: local OCR (`xeet-ocr`, Tesseract verified; Apple Vision helper unverified) and `ModelReader` over an OpenAI-compatible endpoint. Any other
  provider declaring `vision.read` is used without gateway changes.
- Ingest supports images only; PDFs, audio and video are reported as not
  deliverable (several gateway models accept them natively; the gateway
  passes them through to those models).
- Token counts are estimates from a configured ratio, not a tokenizer.
- No HTTP transport; the gateway is a Rust API, a CLI, and an MCP-discoverable
  contract (`modality.adapt`, which prepares content and never sends it).
- Observed rejections are not persisted to the host profile automatically.
- Live runs are not part of `cargo test` (they need a key, cost money and are
  non-deterministic); their records are committed as evidence instead.
