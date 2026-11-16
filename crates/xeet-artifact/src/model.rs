//! The artifact contract, version 0.1.
//!
//! An artifact is one captured *observation* of something, held as several
//! coordinated representations of that same observation:
//!
//! | section        | answers                                                    |
//! |----------------|------------------------------------------------------------|
//! | `subject`      | what was observed                                          |
//! | `provenance`   | where it came from, how, by which tools, from which bytes  |
//! | `views`        | what a human sees (visual projections, e.g. PNG)           |
//! | `semantic`     | what a machine reads (elements, relations, bounds)         |
//! | `state`        | what was true *at capture time*, and on what basis         |
//! | `capabilities` | what could be done, by whom, with which authority          |
//! | `limitations`  | what was omitted, approximated, derived, inferred, unverified |
//! | `coverage`     | which aspects this adapter supports at all                 |
//! | `assets`       | every byte the artifact refers to, content-addressed       |
//!
//! A view is a projection of the artifact, not the artifact. Nothing here is
//! specific to a source or a renderer: source-specific meaning lives in
//! namespaced `role`s and `attrs` (`x.*`, `github.*`), never in new core types.
//!
//! Every field is additive-friendly: consumers must ignore unknown fields, and
//! producers bump `formatVersion`'s minor number when they add one.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The format identifier every artifact carries.
pub const FORMAT: &str = "xeet-artifact";
/// The version of this contract. Major bumps break readers; minor bumps add.
pub const FORMAT_VERSION: &str = "0.1";
/// The only capability policy this version defines.
pub const CAPABILITY_POLICY: &str = "descriptive-only";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub format: String,
    pub format_version: String,
    /// `sha256:<hex>` of the canonical JSON of this document with `id` set to
    /// the empty string. Content-addressed: same observation, same id.
    pub id: String,
    pub subject: Subject,
    pub provenance: Provenance,
    pub views: Vec<View>,
    pub semantic: Semantic,
    pub state: State,
    /// Always [`CAPABILITY_POLICY`]: capabilities describe, they never grant.
    pub capability_policy: String,
    pub capabilities: Vec<Capability>,
    pub limitations: Limitations,
    /// Aspect name → support level. Aspects: `visual`, `semantic`, `bounds`,
    /// `provenance`, `state`, `capabilities`, `reproduction`.
    pub coverage: BTreeMap<String, Support>,
    pub assets: Vec<Asset>,
}

/// What was observed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Subject {
    /// Namespaced source type, e.g. `x.post`, `github.pull_request`.
    #[serde(rename = "type")]
    pub kind: String,
    /// A stable reference to the original, when one is known (a URL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// A short human label. Derived from the content; not authoritative.
    pub title: String,
    /// The semantic element that represents the subject itself.
    pub element: String,
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    pub version: String,
    /// A behavior version below the tool version, e.g. a ruleset id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruleset: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub source: SourceRef,
    pub capture: Capture,
    /// The adapter that mapped source data into this artifact.
    pub adapter: Tool,
    /// The source bytes the artifact was produced from, kept as assets so the
    /// artifact stays useful (and re-derivable) after the source is gone.
    pub inputs: Vec<AssetUse>,
    /// Every step from inputs to representations, in order.
    pub transformations: Vec<Transformation>,
    /// Whether the source data is known to be authentic. This format cannot
    /// establish authenticity on its own: semantic correctness is not proof of
    /// origin. Producers say `unverified` unless they hold real evidence.
    pub authenticity: Authenticity,
    /// The artifact this one was derived from (e.g. by a local projection).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    /// `x`, `github`, …
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The source's own version marker (an X client build, an API version).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Timestamps the source itself supplied (`createdAt`, `updatedAt`, …).
    pub timestamps: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    /// When the observation was made. Supplied by the caller: producers have
    /// no clock of their own, which keeps capture reproducible.
    pub captured_at: String,
    /// How the source data was obtained: `draft`, `source-payload`,
    /// `api-response`, `fixture`, …
    pub method: String,
    pub captured_by: Tool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssetUse {
    /// `sha256:<hex>` of an entry in `assets`.
    pub asset: String,
    pub role: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Transformation {
    pub step: String,
    pub tool: Tool,
    /// Asset hashes or section names consumed.
    pub inputs: Vec<String>,
    /// Asset hashes or section names produced.
    pub outputs: Vec<String>,
    /// True when re-running the step on the same inputs gives the same bytes.
    pub deterministic: bool,
    /// When the step ran, for steps after the observation (an annotation).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub at: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Authenticity {
    #[default]
    Unverified,
    /// The producer verified a signature or equivalent; evidence must be
    /// referenced in `limitations` or an input asset.
    Verified,
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// One visual projection of the observation.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: String,
    /// `sha256:<hex>` of the image asset.
    pub asset: String,
    pub media_type: String,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Device pixels per layout unit. Semantic bounds are in layout units.
    pub scale: f32,
    pub layout_width: f32,
    pub layout_height: f32,
    pub renderer: Tool,
    /// The exact renderer configuration, enough to reproduce the bytes.
    pub config: Value,
    pub fidelity: Fidelity,
}

/// What a view claims to look like, and how strongly.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Fidelity {
    /// What the pixels are meant to match.
    pub target: String,
    pub claim: FidelityClaim,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FidelityClaim {
    /// Checked against an oracle of the target (e.g. headless Chrome).
    OracleVerified,
    /// A faithful layout of the content, not a replica of any product UI.
    Generic,
    /// The view is the source's own bytes (an image file), unchanged.
    Original,
    #[default]
    None,
}

// ---------------------------------------------------------------------------
// Semantic
// ---------------------------------------------------------------------------

/// The universal element vocabulary. Anything else must be namespaced
/// (`x.something`), and verification reports it as an extension.
pub const ELEMENT_KINDS: &[&str] = &[
    "record",     // a whole observed object: a post, a pull request, an invoice
    "identity",   // a person, account or organization
    "text",       // a run of prose or a label's value
    "heading",    // a title
    "timestamp",  // a point in time (value: ISO 8601)
    "link",       // a navigable reference (value: URL)
    "reference",  // a non-navigable pointer to another object (a ref, an id)
    "media",      // an image, video or other embedded content
    "quantity",   // a number with an optional unit
    "status",     // an enumerated state (open, merged, approved…)
    "label",      // a tag or category
    "list",       // an ordered collection of children
    "group",      // a region grouping children
    "message",    // an authored contribution inside a record (comment, review)
    "affordance", // a visible control (a button) — what is *shown*, not what is allowed
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Semantic {
    pub root: String,
    pub elements: Vec<Element>,
    pub relations: Vec<Relation>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Element {
    pub id: String,
    pub kind: String,
    /// Namespaced, source-specific meaning: `x.body`, `github.pr.title`.
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Namespaced source-specific attributes.
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub attrs: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<String>,
    /// Where this element appears in each view, in that view's layout units.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub bounds: Vec<Bounds>,
    /// Whether the element is visible in the views (it may be in the source
    /// data but hidden or cut, like the tail of a truncated post).
    pub visible: bool,
    pub basis: Basis,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub view: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Where a piece of information came from. Never silently promote one basis
/// to another: an inference is not a fact.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Basis {
    /// Supplied by the source data as-is.
    Source,
    /// Measured by the capturing tool (e.g. the layout engine placed it here).
    Observed,
    /// Computed deterministically from source or observed data by a declared
    /// rule; re-running the rule gives the same answer.
    Derived,
    /// Produced by a non-deterministic or heuristic process (a model, OCR).
    Inferred,
    /// Not known. The value, if any, must be null.
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub from: String,
    pub to: String,
    /// `authoredBy`, `references`, `quotes`, `repliesTo`, `attachedTo`,
    /// `reviews`, `mergesInto`, `describes`, …
    pub kind: String,
    pub basis: Basis,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What was true at capture time. Never current state.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// Equal to `provenance.capture.capturedAt`. Every fact is "as of" this.
    pub as_of: String,
    /// Always `historical-observation` in this version.
    pub temporal: String,
    pub facts: Vec<Fact>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    /// The element the fact is about.
    pub subject: String,
    pub key: String,
    /// `null` exactly when `basis` is `unknown`.
    pub value: Value,
    pub basis: Basis,
    /// When the *source* says this value was true, if it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_time: Option<String>,
    /// True when the value can change after capture (counts, open/closed).
    pub volatile: bool,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// Something a human or agent could potentially do. A description, never a
/// grant: executing it needs authority from the system named in `executor`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub id: String,
    pub kind: CapabilityKind,
    pub title: String,
    /// The element the capability acts on.
    pub target: String,
    /// JSON Schema of the input the executor expects.
    pub input_schema: Value,
    pub effects: Vec<String>,
    pub authority: Authority,
    /// Conditions that must hold at execution time. Facts here were true at
    /// capture; they may not be now.
    pub preconditions: Vec<String>,
    pub executor: Executor,
    /// How to confirm the effect happened.
    pub verification: String,
    pub availability: Availability,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityKind {
    /// Go somewhere (open a URL). Changes nothing.
    #[default]
    Navigate,
    /// Produce another projection of the same observation (expand, re-theme).
    /// Changes nothing at the source.
    Projection,
    /// Change something at the source (merge, like, approve).
    Mutation,
    /// Send something to someone (reply, comment).
    Communication,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Authority {
    /// Scopes the caller must hold, as the executor names them. Empty means no
    /// authority beyond the ability to reach the executor.
    pub required: Vec<String>,
    /// Who checks it: `none` (no check needed), or the executor's authority.
    pub checked_by: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Executor {
    pub kind: ExecutorKind,
    /// A URL, an API operation (`PUT /repos/{o}/{r}/pulls/{n}/merge`) or a
    /// local tool name.
    pub reference: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutorKind {
    /// Runs locally and deterministically against this artifact's own inputs.
    Local,
    /// The originating system (or another explicitly authorized provider).
    #[default]
    Origin,
    /// A user agent opening a reference (a browser following a link).
    UserAgent,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    /// A local executor can run it from the artifact alone.
    LocalExecutable,
    /// Only the executor can run it, with the required authority.
    #[default]
    Delegated,
    /// Described for completeness; no executor is known to accept it.
    NotExecutable,
}

// ---------------------------------------------------------------------------
// Limitations, coverage, assets
// ---------------------------------------------------------------------------

/// The producer's own account of what this artifact is not.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Limitations {
    pub omitted: Vec<Note>,
    pub approximated: Vec<Note>,
    pub derived: Vec<Note>,
    pub inferred: Vec<Note>,
    pub unverified: Vec<Note>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub what: String,
    pub why: String,
}

impl Note {
    pub fn new(subject: Option<&str>, what: impl Into<String>, why: impl Into<String>) -> Note {
        Note {
            subject: subject.map(str::to_string),
            what: what.into(),
            why: why.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Support {
    Supported,
    Partial,
    NotSupported,
    #[default]
    Unknown,
    NotApplicable,
}

/// The coverage aspects every adapter declares.
pub const COVERAGE_ASPECTS: &[&str] = &[
    "visual",
    "semantic",
    "bounds",
    "provenance",
    "state",
    "capabilities",
    "reproduction",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    /// `sha256:<hex>` of the bytes.
    pub sha256: String,
    pub media_type: String,
    pub bytes: u64,
    /// Path inside the package, derived from the hash.
    pub path: String,
}
