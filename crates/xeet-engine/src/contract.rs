//! Capability contracts and the provider manifest.
//!
//! A contract is data: what a capability is for, what it takes and returns,
//! in which modalities, with which effects, under which authority and limits,
//! and what it guarantees. Contract ids (`representation.understand`) name
//! the capability, not this provider: another provider can implement the same
//! id, and discovery tells them apart by provider identity.
//!
//! Nothing here depends on MCP or any other transport.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MANIFEST_FORMAT: &str = "representation-engine.manifest";
pub const MANIFEST_VERSION: &str = "0.1";
pub const CONTRACT_VERSION: &str = "0.1";

/// Media-type-like modality names.
pub mod modality {
    pub const TEXT: &str = "text/plain";
    pub const JSON: &str = "application/json";
    pub const PNG: &str = "image/png";
    /// A reference to a stored artifact (`artifact://sha256/<hex>`).
    pub const ARTIFACT: &str = "application/vnd.xeet-artifact+ref";
    /// An artifact package directory.
    pub const PACKAGE: &str = "application/vnd.xeet-artifact+dir";
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: String,
    pub version: String,
    pub provider: Provider,
    /// How this provider can be reached. Contracts do not depend on any of these.
    pub interfaces: Vec<Interface>,
    pub compatibility: Compatibility,
    pub authority: AuthorityModel,
    pub sources: Vec<SourceSupport>,
    pub capabilities: Vec<Contract>,
    /// Outcomes this provider is asked for but does not provide, with why and
    /// what to do instead. Discovery reports these instead of a false match.
    pub non_capabilities: Vec<NonCapability>,
    pub limitations: Vec<String>,
    pub constraints: Constraints,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Interface {
    pub kind: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Compatibility {
    pub artifact_format: String,
    pub artifact_versions_read: String,
    pub artifact_version_written: String,
    pub projection_format: String,
    pub contract_version: String,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityModel {
    pub summary: String,
    pub scopes: Vec<Scope>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub id: String,
    pub meaning: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceSupport {
    #[serde(rename = "type")]
    pub kind: String,
    pub adapter: String,
    pub adapter_version: String,
    pub input: String,
    pub visual_fidelity: String,
    /// Aspect → support level, as the adapter declares in every artifact.
    pub coverage: Value,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Contract {
    pub id: String,
    pub version: String,
    pub title: String,
    /// One line. Discovery returns this before anything else.
    pub summary: String,
    pub description: String,
    /// Outcomes an agent might ask for, in its own words. Discovery matches these.
    pub outcomes: Vec<String>,
    pub input_modalities: Vec<String>,
    /// Ways to receive the result. An agent negotiates by the modalities it accepts.
    pub modes: Vec<Mode>,
    pub input_schema: Value,
    pub output_schema: Value,
    /// `none` (reads only) or `creates-artifact` (adds to the store). No
    /// capability of this provider modifies or deletes anything, here or at a source.
    pub effects: String,
    pub consequential: bool,
    pub authority: Vec<String>,
    pub preconditions: Vec<String>,
    pub guarantees: Vec<String>,
    pub limitations: Vec<String>,
    pub constraints: Constraints,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Mode {
    /// Modalities the caller receives in this mode.
    pub output: Vec<String>,
    /// Input fields that select this mode.
    pub select: Value,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Constraints {
    pub network: String,
    pub deterministic: bool,
    pub max_input_bytes: u64,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NonCapability {
    pub outcomes: Vec<String>,
    pub why: String,
    pub alternative: String,
}

pub const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

fn constraints(deterministic: bool, notes: &[&str]) -> Constraints {
    Constraints {
        network: "none".into(),
        deterministic,
        max_input_bytes: MAX_INPUT_BYTES,
        notes: notes.iter().map(|s| s.to_string()).collect(),
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn artifact_ref() -> Value {
    json!({"type": "string", "minLength": 1, "description": "An artifact id (sha256:<hex>), its URI (artifact://sha256/<hex>), or a package directory path inside a read root the host allows (needs fs:read)."})
}

fn verification_summary() -> Value {
    json!({"type": "object", "required": ["ok", "failed"], "properties": {
        "ok": {"type": "boolean"},
        "failed": {"type": "array", "items": {"type": "string"}},
        "skipped": {"type": "array", "items": {"type": "string"}}}})
}

fn created() -> Value {
    json!({"type": "object", "required": ["artifact", "id", "verification"], "properties": {
        "artifact": {"type": "string", "description": "artifact://sha256/<hex>"},
        "id": {"type": "string"},
        "subject": {"type": "object"},
        "derivedFrom": {"type": ["string", "null"]},
        "verification": verification_summary()}})
}

const ERRORS_READ: &[&str] = &[
    "invalid_input",
    "not_found",
    "unauthorized",
    "limit_exceeded",
];
const ERRORS_WRITE: &[&str] = &[
    "invalid_input",
    "not_found",
    "unauthorized",
    "limit_exceeded",
    "verification_failed",
    "unsupported",
];

/// Every capability this provider implements.
pub fn contracts() -> Vec<Contract> {
    use modality::*;
    let options = json!({"type": "object", "additionalProperties": false, "properties": {
        "width": {"type": "number", "minimum": 1, "description": "layout width (CSS px for x.post, default 600)"},
        "theme": {"enum": ["light", "dark"]},
        "scale": {"type": "number", "minimum": 1, "maximum": 3, "description": "device pixels per layout unit (default 2)"},
        "surface": {"enum": ["web-timeline", "web-detail", "ios-timeline", "ios-detail"], "description": "x.post only"}}});
    let projection_inputs = json!({
        "artifact": artifact_ref(),
        "consumer": {"enum": ["text", "vision"], "default": "text", "description": "text: no pixels, everything else; vision: also returns the image(s)"},
        "format": {"enum": ["text", "json"], "default": "text"},
        "focus": {"type": "string", "description": "element id; its ancestors are kept as context"},
        "depth": {"type": "integer", "minimum": 0},
        "sections": {"type": "array", "items": {"enum": ["elements", "layout", "relations", "state", "capabilities", "limitations", "consumers"]}},
        "maxText": {"type": "integer", "minimum": 1, "description": "truncate any single string, saying so"},
        "maxChars": {"type": "integer", "minimum": 1, "description": "total budget; filled by priority, omissions listed"},
        "compactCapabilities": {"type": "boolean"}});

    vec![
        Contract {
            id: "representation.capture".into(),
            version: CONTRACT_VERSION.into(),
            title: "Capture an observation as an artifact".into(),
            summary: "Turn source data (an X post, a GitHub pull request) into a verified artifact: a visual view plus semantics, state, provenance, capabilities and limitations.".into(),
            description: "The caller supplies the source data inline (`data`) or as a file path inside an allowed read root (`path`); the provider never fetches. The result is stored content-addressed and verified, including byte-for-byte reproduction of its view, before it is returned.".into(),
            outcomes: s(&["capture an observation", "make a screenshot an agent can understand", "turn a post into structured data", "represent a pull request", "create an artifact", "convert source data into a visual and semantic representation", "render a post with semantics", "snapshot a pull request"]),
            input_modalities: s(&[JSON]),
            modes: vec![Mode { output: s(&[ARTIFACT, JSON]), select: json!({}), note: "returns a reference; use representation.understand to read it".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["source"], "properties": {
                "source": {"enum": ["x.post", "github.pull_request"]},
                "data": {"type": "object", "description": "x.post: a model::Post or `xeet resolve` output; github.pull_request: {pull, reviews?, comments?, files?, commits?, apiVersion?} of REST responses"},
                "path": {"type": "string", "description": "a file with the same content, inside an allowed read root (needs fs:read)"},
                "options": options,
                "capturedAt": {"type": "string", "description": "ISO 8601 observation time; defaults to the host clock"},
                "method": {"type": "string", "description": "how the data was obtained: draft, source-payload, api-response, fixture"},
                "reference": {"type": "string", "description": "x.post: the post URL, if known"},
                "sourceId": {"type": "string", "description": "x.post: the status id, if known"}}}),
            output_schema: created(),
            effects: "creates-artifact".into(),
            consequential: false,
            authority: s(&["store:write"]),
            preconditions: s(&["exactly one of data or path", "path inputs also need fs:read and must lie inside an allowed read root"]),
            guarantees: s(&["the stored artifact passed verification, including reproduction of its view from the preserved input", "same input and capturedAt give the same artifact id", "source bytes are preserved verbatim in the artifact"]),
            limitations: s(&["two source types today: x.post and github.pull_request", "no network: source data must be supplied", "authenticity of the source data is recorded as unverified"]),
            constraints: constraints(true, &["typical latency tens of milliseconds"]),
            errors: s(ERRORS_WRITE),
        },
        Contract {
            id: "representation.ingest".into(),
            version: CONTRACT_VERSION.into(),
            title: "Ingest an existing artifact package".into(),
            summary: "Verify an artifact package from a path and add it to the store.".into(),
            description: "Runs full verification, with reproduction when an adapter for the artifact is available, and refuses the package if any check fails.".into(),
            outcomes: s(&["import an artifact", "add an artifact to the store", "load a package"]),
            input_modalities: s(&[PACKAGE]),
            modes: vec![Mode { output: s(&[ARTIFACT, JSON]), select: json!({}), note: "returns a reference".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["path"], "properties": {"path": {"type": "string", "minLength": 1}}}),
            output_schema: created(),
            effects: "creates-artifact".into(),
            consequential: false,
            authority: s(&["store:write", "fs:read"]),
            preconditions: s(&["path inside an allowed read root"]),
            guarantees: s(&["nothing failing verification enters the store"]),
            limitations: s(&["directory packages only"]),
            constraints: constraints(true, &[]),
            errors: s(ERRORS_WRITE),
        },
        Contract {
            id: "representation.inspect".into(),
            version: CONTRACT_VERSION.into(),
            title: "Inspect an artifact document".into(),
            summary: "Return the complete artifact document (subject, provenance, views, semantic graph, state, capabilities, limitations, coverage, assets) as stored.".into(),
            description: "The authoritative structured form, unprojected. For a compact or budgeted reading, use representation.understand.".into(),
            outcomes: s(&["inspect an artifact", "get the raw artifact document", "full provenance and assets", "read the artifact json"]),
            input_modalities: s(&[ARTIFACT]),
            modes: vec![Mode { output: s(&[JSON]), select: json!({}), note: "the document".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["artifact"], "properties": {"artifact": artifact_ref()}}),
            output_schema: json!({"type": "object", "required": ["artifact", "document"], "properties": {"artifact": {"type": "string"}, "document": {"type": "object"}}}),
            effects: "none".into(),
            consequential: false,
            authority: s(&["store:read"]),
            preconditions: s(&[]),
            guarantees: s(&["the document is returned exactly as stored; its id can be re-derived with representation.verify"]),
            limitations: s(&[]),
            constraints: constraints(true, &[]),
            errors: s(ERRORS_READ),
        },
        Contract {
            id: "representation.understand".into(),
            version: CONTRACT_VERSION.into(),
            title: "Understand an artifact (projection for a consumer)".into(),
            summary: "Get an artifact's full meaning as text or JSON — elements, positions, layout rows, state, capabilities, limitations — without needing vision; optionally with the image.".into(),
            description: "A deterministic projection. Text-only consumers receive everything except pixels, plus an explicit list of content that exists only as pixels. Vision consumers additionally receive the view image; anything read from it is inference. Supports a focus element, depth, sections, and character budgets for small context windows; omissions are always listed.".into(),
            outcomes: s(&["understand a screenshot without vision", "read visual information as text", "describe what is on screen", "text representation of an image", "make visual content readable by a text-only model", "summarize an artifact for a small context window", "get layout and positions as text", "what does this artifact contain", "read the state of a pull request", "get the semantic content"]),
            input_modalities: s(&[ARTIFACT]),
            modes: vec![
                Mode { output: s(&[TEXT]), select: json!({"consumer": "text", "format": "text"}), note: "line-oriented text; works for any model".into() },
                Mode { output: s(&[JSON]), select: json!({"consumer": "text", "format": "json"}), note: "the same projection, typed".into() },
                Mode { output: s(&[PNG, TEXT]), select: json!({"consumer": "vision"}), note: "image(s) plus the same text".into() },
            ],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["artifact"], "properties": projection_inputs}),
            output_schema: json!({"type": "object", "required": ["artifact", "format", "projection"], "properties": {
                "artifact": {"type": "string"},
                "format": {"enum": ["text", "json"]},
                "projection": {"type": ["string", "object"]},
                "images": {"type": "array", "items": {"type": "object", "required": ["view", "mediaType", "data"], "properties": {
                    "view": {"type": "string"}, "mediaType": {"type": "string"}, "data": {"type": "string", "description": "base64"}}}}}}),
            effects: "none".into(),
            consequential: false,
            authority: s(&["store:read"]),
            preconditions: s(&[]),
            guarantees: s(&["deterministic: same artifact and request give the same bytes", "element ids are the artifact's ids", "basis marks distinguish source, derived, observed, inferred and unknown", "provenance header is never dropped; every omission is listed"]),
            limitations: s(&["content marked visual-only is not readable as text by anyone; vision readings are inference"]),
            constraints: constraints(true, &["typical latency a few milliseconds"]),
            errors: s(ERRORS_READ),
        },
        Contract {
            id: "representation.render".into(),
            version: CONTRACT_VERSION.into(),
            title: "Get a visual view".into(),
            summary: "Return one of an artifact's images (PNG) with its view metadata.".into(),
            description: "For consumers that read images or for showing people. Text-only consumers should use representation.understand instead; this capability only returns pixels.".into(),
            outcomes: s(&["show the image", "get the screenshot", "render the view", "visual evidence for a person"]),
            input_modalities: s(&[ARTIFACT]),
            modes: vec![Mode { output: s(&[PNG]), select: json!({}), note: "image only".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["artifact"], "properties": {"artifact": artifact_ref(), "view": {"type": "string", "default": "v0"}}}),
            output_schema: json!({"type": "object", "required": ["artifact", "view", "mediaType", "data"], "properties": {
                "artifact": {"type": "string"}, "view": {"type": "object"}, "mediaType": {"type": "string"}, "data": {"type": "string", "description": "base64"}}}),
            effects: "none".into(),
            consequential: false,
            authority: s(&["store:read"]),
            preconditions: s(&[]),
            guarantees: s(&["the bytes are the stored view, whose hash the artifact names"]),
            limitations: s(&[]),
            constraints: constraints(true, &[]),
            errors: s(ERRORS_READ),
        },
        Contract {
            id: "representation.derive".into(),
            version: CONTRACT_VERSION.into(),
            title: "Run a local projection capability".into(),
            summary: "Produce a new artifact of the same observation with different view options (expand, retheme), from the preserved input.".into(),
            description: "Runs only capabilities the artifact marks local-executable. Anything that needs outside authority (reply, merge, like) is refused: the provider never acts at a source.".into(),
            outcomes: s(&["expand show more", "switch to dark theme", "re-render differently", "derive a new view"]),
            input_modalities: s(&[ARTIFACT]),
            modes: vec![Mode { output: s(&[ARTIFACT, JSON]), select: json!({}), note: "returns a reference".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["artifact", "capability"], "properties": {
                "artifact": artifact_ref(), "capability": {"type": "string", "minLength": 1}, "input": {"type": "object"}}}),
            output_schema: created(),
            effects: "creates-artifact".into(),
            consequential: false,
            authority: s(&["store:read", "store:write"]),
            preconditions: s(&["the capability is local-executable in the artifact"]),
            guarantees: s(&["the derived artifact names its parent in provenance.derivedFrom and passes verification"]),
            limitations: s(&["only x.post artifacts have local capabilities today"]),
            constraints: constraints(true, &[]),
            errors: s(ERRORS_WRITE),
        },
        Contract {
            id: "representation.verify".into(),
            version: CONTRACT_VERSION.into(),
            title: "Verify an artifact".into(),
            summary: "Check integrity, consistency, honesty rules and (optionally) byte-for-byte reproduction; also lists what cannot be established.".into(),
            description: "Named checks: format, schema, identity, assets, views, semantic, basis, state, capabilities, provenance, coverage, reproduction.".into(),
            outcomes: s(&["check integrity", "verify an artifact", "is this artifact authentic or tampered", "prove the image matches the data"]),
            input_modalities: s(&[ARTIFACT]),
            modes: vec![Mode { output: s(&[JSON]), select: json!({}), note: "verification report".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["artifact"], "properties": {"artifact": artifact_ref(), "reproduce": {"type": "boolean", "default": true}}}),
            output_schema: json!({"type": "object", "required": ["ok", "checks", "cannotEstablish"], "properties": {
                "ok": {"type": "boolean"}, "artifact": {"type": ["string", "null"]},
                "checks": {"type": "array", "items": {"type": "object", "required": ["name", "status"]}},
                "requiresExternalAuthority": {"type": "array"}, "cannotEstablish": {"type": "array"}}}),
            effects: "none".into(),
            consequential: false,
            authority: s(&["store:read"]),
            preconditions: s(&[]),
            guarantees: s(&["deterministic; no network, clock or model"]),
            limitations: s(&["cannot establish source authenticity or current truth"]),
            constraints: constraints(true, &[]),
            errors: s(ERRORS_READ),
        },
        Contract {
            id: "representation.compare".into(),
            version: CONTRACT_VERSION.into(),
            title: "Compare two observations".into(),
            summary: "What differs between two artifacts: views, elements, state facts, capabilities.".into(),
            description: "Identical pixels do not imply identical meaning and vice versa; both are reported.".into(),
            outcomes: s(&["what changed", "diff two snapshots", "compare observations over time"]),
            input_modalities: s(&[ARTIFACT]),
            modes: vec![Mode { output: s(&[JSON]), select: json!({}), note: "comparison".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["a", "b"], "properties": {"a": artifact_ref(), "b": artifact_ref()}}),
            output_schema: json!({"type": "object", "required": ["identical", "views", "factsChanged"], "properties": {"identical": {"type": "boolean"}, "views": {"type": "object"}, "factsChanged": {"type": "array"}}}),
            effects: "none".into(),
            consequential: false,
            authority: s(&["store:read"]),
            preconditions: s(&[]),
            guarantees: s(&["deterministic"]),
            limitations: s(&["element matching relies on adapter-stable ids"]),
            constraints: constraints(true, &[]),
            errors: s(ERRORS_READ),
        },
        Contract {
            id: "representation.retrieve".into(),
            version: CONTRACT_VERSION.into(),
            title: "Find artifacts in the store".into(),
            summary: "List stored artifacts, filtered by source type, subject reference or text in the title.".into(),
            description: "Returns references and summaries, newest capture first. Read the content with representation.understand.".into(),
            outcomes: s(&["find artifacts", "list observations", "history of a subject", "search stored artifacts"]),
            input_modalities: s(&[JSON]),
            modes: vec![Mode { output: s(&[JSON]), select: json!({}), note: "list of references".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "properties": {
                "subjectType": {"type": "string"}, "reference": {"type": "string"}, "text": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50}}}),
            output_schema: json!({"type": "object", "required": ["artifacts", "total"], "properties": {
                "artifacts": {"type": "array", "items": {"type": "object", "required": ["id", "uri", "subjectType", "title", "capturedAt"]}},
                "total": {"type": "integer"}}}),
            effects: "none".into(),
            consequential: false,
            authority: s(&["store:read"]),
            preconditions: s(&[]),
            guarantees: s(&["deterministic order: capturedAt descending, then id"]),
            limitations: s(&["substring filters, not semantic search"]),
            constraints: constraints(true, &[]),
            errors: s(ERRORS_READ),
        },
        Contract {
            id: "representation.annotate".into(),
            version: CONTRACT_VERSION.into(),
            title: "Add content read from pixels, as inference".into(),
            summary: "Record what an external vision model or OCR read from an artifact's image, as declared inference in a new derived artifact.".into(),
            description: "Accepts only elements and facts with basis `inferred`, requires who read them, how and when, never edits or overrides observed content, and leaves the original artifact untouched.".into(),
            outcomes: s(&["record what a vision model saw", "add OCR results", "describe an image for text-only models", "store extracted chart values"]),
            input_modalities: s(&[ARTIFACT, JSON]),
            modes: vec![Mode { output: s(&[ARTIFACT, JSON]), select: json!({}), note: "returns a reference".into() }],
            input_schema: json!({"type": "object", "additionalProperties": false, "required": ["artifact", "annotation"], "properties": {
                "artifact": artifact_ref(),
                "annotation": {"type": "object", "required": ["parent", "by", "method", "at"], "properties": {
                    "parent": {"type": "string"}, "elements": {"type": "array"}, "facts": {"type": "array"},
                    "by": {"type": "object", "required": ["name", "version"]}, "method": {"type": "string"}, "at": {"type": "string"}}}}}),
            output_schema: created(),
            effects: "creates-artifact".into(),
            consequential: false,
            authority: s(&["store:read", "store:write"]),
            preconditions: s(&["every added element and fact has basis inferred"]),
            guarantees: s(&["inference is never recorded as source; the original is unchanged; views and inputs are carried over byte for byte"]),
            limitations: s(&["the provider does not perform the reading itself"]),
            constraints: constraints(false, &["the reading it records is non-deterministic by nature; recording it is deterministic"]),
            errors: s(ERRORS_WRITE),
        },
    ]
}

pub fn manifest() -> Manifest {
    let v = env!("CARGO_PKG_VERSION").to_string();
    let cov = |pairs: &[(&str, &str)]| -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(k, x)| (k.to_string(), json!(x)))
                .collect(),
        )
    };
    Manifest {
        format: MANIFEST_FORMAT.into(),
        version: MANIFEST_VERSION.into(),
        provider: Provider {
            id: "xeet".into(),
            name: "xeet representation engine".into(),
            version: v.clone(),
            description: "Captures observations as durable artifacts that people can see and agents can read without vision, verifies them offline, and describes what can be done with them without granting it.".into(),
        },
        interfaces: vec![
            Interface { kind: "mcp-stdio".into(), description: "`xeet engine mcp`: progressive tools (engine_discover, engine_describe, engine_invoke) by default, every capability as a tool with --tools all; artifacts as resources".into() },
            Interface { kind: "cli".into(), description: "`xeet engine manifest | discover | describe | call <capability> --input JSON`".into() },
            Interface { kind: "rust".into(), description: "xeet_engine::Engine::invoke(capability, input, session)".into() },
        ],
        compatibility: Compatibility {
            artifact_format: xeet_artifact::FORMAT.into(),
            artifact_versions_read: "0.x".into(),
            artifact_version_written: xeet_artifact::FORMAT_VERSION.into(),
            projection_format: format!("{} {}", xeet_artifact::project::PROJECTION_FORMAT, xeet_artifact::project::PROJECTION_VERSION),
            contract_version: CONTRACT_VERSION.into(),
            notes: s(&["contract ids name capabilities, not this provider; minor versions only add optional fields", "unknown input fields are rejected, unknown output fields must be ignored"]),
        },
        authority: AuthorityModel {
            summary: "Discovery grants nothing. The host that starts the provider grants scopes for a session; every invocation checks the scopes its contract names and fails with `unauthorized` listing the missing ones. No capability acts at a source system.".into(),
            scopes: vec![
                Scope { id: "store:read".into(), meaning: "read artifacts in the provider's store".into() },
                Scope { id: "store:write".into(), meaning: "add artifacts to the store (never modify or delete)".into() },
                Scope { id: "fs:read".into(), meaning: "read files and package directories inside the read roots the host allows".into() },
            ],
        },
        sources: vec![
            SourceSupport {
                kind: "x.post".into(),
                adapter: xeet_capture::x::ADAPTER.into(),
                adapter_version: v.clone(),
                input: "a model::Post JSON (see crates/xeet-core/src/model.rs) or `xeet resolve` output".into(),
                visual_fidelity: "oracle-verified against Chrome for the ruleset's verified surfaces".into(),
                coverage: cov(&[("visual", "supported"), ("semantic", "partial"), ("bounds", "partial"), ("provenance", "supported"), ("state", "partial"), ("capabilities", "partial"), ("reproduction", "supported")]),
                notes: s(&["entity-level bounds are not computed", "media without bytes render as placeholders and are declared approximated"]),
            },
            SourceSupport {
                kind: "github.pull_request".into(),
                adapter: xeet_capture::github::ADAPTER.into(),
                adapter_version: v.clone(),
                input: "{pull, reviews?, comments?, files?, commits?, apiVersion?}: GitHub REST API responses".into(),
                visual_fidelity: "generic document layout; not a replica of github.com".into(),
                coverage: cov(&[("visual", "partial"), ("semantic", "supported"), ("bounds", "partial"), ("provenance", "supported"), ("state", "partial"), ("capabilities", "supported"), ("reproduction", "supported")]),
                notes: s(&["markdown shown as text", "inline review threads, checks and statuses not read"]),
            },
        ],
        capabilities: contracts(),
        non_capabilities: vec![
            NonCapability {
                outcomes: s(&["ocr an arbitrary image", "read text from a photo", "understand an arbitrary screenshot or image file", "describe a picture", "extract chart values from pixels"]),
                why: "the provider builds representations from source data; it does not interpret pixels it did not render, and would only be guessing".into(),
                alternative: "use an external vision or OCR provider, then record its reading with representation.annotate, where it stays marked as inference".into(),
            },
            NonCapability {
                outcomes: s(&["merge a pull request", "approve a review", "post a reply", "like a post", "execute an action at the source", "comment on a pull request"]),
                why: "the provider never acts at a source system; artifacts describe such actions with the authority they require".into(),
                alternative: "read the action's contract with representation.understand (capabilities section) and execute it with the origin system under your own authority".into(),
            },
            NonCapability {
                outcomes: s(&["fetch a url", "download a post", "call the github api", "scrape a page"]),
                why: "the provider has no network access by design; the caller supplies source data".into(),
                alternative: "fetch with your own client (or `xeet resolve` for X), then representation.capture".into(),
            },
        ],
        limitations: s(&["two source types", "no OCR or image interpretation", "no network", "artifact authenticity is always unverified", "discovery matches declared outcomes by keywords, not by meaning"]),
        constraints: Constraints {
            network: "none".into(),
            deterministic: true,
            max_input_bytes: MAX_INPUT_BYTES,
            notes: s(&["single process, local filesystem store", "no clock except the host-supplied capture time"]),
        },
    }
}
