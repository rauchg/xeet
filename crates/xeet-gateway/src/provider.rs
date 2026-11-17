//! The gateway as a provider of one contract, `modality.adapt`, so agents
//! and hosts discover and invoke it through the same contract system as any
//! other capability (MCP, CLI, Rust).
//!
//! `modality.adapt` prepares content for a stated model profile and returns
//! it; it does not send anything to any model. Whether a runtime injects the
//! result into its own requests is that runtime's decision — discovering this
//! capability connects it to nothing.

use serde_json::{json, Value};
use xeet_engine::contract::{self, modality, Constraints, Contract, Manifest, Mode};
use xeet_engine::{schema, Outcome, Provider, Session};

use crate::gateway::{Gateway, Task};
use crate::inspect::Input;
use crate::model::ModelProfile;

pub const PROVIDER_ID: &str = "xeet-gateway";

pub fn manifest() -> Manifest {
    let mut m = contract::manifest();
    m.provider = contract::Provider {
        id: PROVIDER_ID.into(),
        name: "xeet modality gateway".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "Prepares inputs for a model whose input modalities are stated in a profile: passes through what it accepts, represents the rest as text through other providers' contracts, and records why.".into(),
    };
    m.sources.clear();
    m.non_capabilities = vec![contract::NonCapability {
        outcomes: vec!["send a request to a model".into(), "call the model".into(), "connect to my model runtime".into()],
        why: "the gateway prepares content; sending it is the runtime's model client's job".into(),
        alternative: "inject modality.adapt's content into your own model request (or use xeet-gateway's Gateway::run in Rust with your ModelClient)".into(),
    }];
    m.limitations = vec![
        "reads content only through providers that declare vision.read; without one, an image's content is reported as unknown".into(),
        "ingests images only (PNG, JPEG, GIF, WebP); other modalities are reported as not deliverable".into(),
    ];
    m.capabilities = vec![Contract {
        id: "modality.adapt".into(),
        version: "0.1".into(),
        title: "Adapt inputs to a model's input modalities".into(),
        summary: "Given an instruction, inputs and a model capability profile, return content that model can consume: accepted inputs as-is, others as provenance-preserving text representations, with a decision record.".into(),
        description: "Inputs the profile does not accept are inspected by content, reused if an artifact for the same bytes exists, otherwise ingested, read by a vision.read provider when one is authorized, annotated as inference, and projected within the profile's context budget. The instruction is returned unchanged as the first content item.".into(),
        outcomes: vec![
            "make an image readable by a text-only model".into(),
            "convert an attachment for a model that cannot read it".into(),
            "adapt inputs to model capabilities".into(),
            "the model does not support images".into(),
            "prepare a model request from a screenshot".into(),
            "understand a screenshot without vision".into(),
        ],
        input_modalities: vec![modality::JSON.into(), modality::PNG.into(), "image/jpeg".into(), modality::ARTIFACT.into()],
        modes: vec![
            Mode { output: vec![modality::TEXT.into()], select: json!({"model": "a profile that accepts only text"}), note: "every content part is text; the record comes as structured data".into() },
            Mode { output: vec![modality::TEXT.into(), modality::PNG.into(), "image/jpeg".into()], select: json!({"model": "a profile that accepts images"}), note: "accepted media pass through unchanged".into() },
        ],
        input_schema: json!({"type": "object", "additionalProperties": false, "required": ["instruction", "inputs", "model"], "properties": {
            "instruction": {"type": "string", "description": "the user's instruction; returned unchanged"},
            "inputs": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {
                "text": {"type": "string"}, "dataBase64": {"type": "string"}, "path": {"type": "string"},
                "artifact": {"type": "string"}, "declaredType": {"type": "string"}, "name": {"type": "string"}}}},
            "model": {"type": "object", "required": ["format", "version", "provider", "model", "inputs", "structuredInput", "source"], "description": "a model-input-capabilities profile"},
            "now": {"type": "string", "description": "ISO 8601 time for captures; defaults to the host clock"}}}),
        output_schema: json!({"type": "object", "required": ["content", "parts", "budget", "requestDigest"], "properties": {
            "content": {"type": "array", "items": {"type": "object", "required": ["type"]}},
            "parts": {"type": "array"}, "budget": {"type": "object"}, "requestDigest": {"type": "string"}}}),
        effects: "creates-artifact".into(),
        consequential: false,
        authority: vec!["store:read".into()],
        preconditions: vec!["each transformation step needs its own provider's scopes (store:write to ingest, vision:read to read); missing ones are reported per step, not escalated".into()],
        guarantees: vec![
            "the instruction is the first content item, unchanged".into(),
            "inferred content is marked !inferred; content no provider could read is declared unknown".into(),
            "nothing is sent to any model".into(),
        ],
        limitations: vec!["see the provider's limitations".into()],
        constraints: Constraints { network: "none".into(), deterministic: true, max_input_bytes: contract::MAX_INPUT_BYTES, notes: vec!["deterministic given the same providers, store and readers".into()] },
        errors: vec!["invalid_input".into(), "unauthorized".into()],
    }];
    m
}

/// The gateway over a set of providers, as a provider itself.
pub struct GatewayProvider<'a> {
    pub providers: Vec<&'a dyn Provider>,
    manifest: Manifest,
}

impl<'a> GatewayProvider<'a> {
    pub fn new(providers: Vec<&'a dyn Provider>) -> Self {
        GatewayProvider {
            providers,
            manifest: manifest(),
        }
    }
}

fn digest(v: &Value) -> String {
    xeet_artifact::canon::sha256_id(xeet_artifact::canon::canonical(v).as_bytes())
}

impl Provider for GatewayProvider<'_> {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn invoke(&self, cap: &str, input: &Value, session: &Session) -> Outcome {
        let c = &self.manifest.capabilities[0];
        let mut out = Outcome {
            capability: cap.into(),
            version: c.version.clone(),
            provider: PROVIDER_ID.into(),
            provider_version: self.manifest.provider.version.clone(),
            ok: false,
            result: None,
            error: None,
            input_digest: digest(input),
            result_digest: None,
            artifacts: vec![],
            authority_checked: c.authority.clone(),
        };
        let fail = |code: &str, msg: String, details: Value| {
            Some(xeet_engine::Error {
                code: code.into(),
                message: msg,
                details,
            })
        };
        if cap != c.id {
            out.error = fail("not_found", format!("no capability {cap}"), Value::Null);
            return out;
        }
        let problems = schema::validate(&c.input_schema, input);
        if !problems.is_empty() {
            out.error = fail(
                "invalid_input",
                "input does not match the contract".into(),
                json!(problems),
            );
            return out;
        }
        let missing = xeet_engine::missing_scopes(c, session);
        if !missing.is_empty() {
            out.error = fail(
                "unauthorized",
                format!("this session lacks {missing:?}"),
                json!({"missing": missing}),
            );
            return out;
        }
        let profile: ModelProfile = match serde_json::from_value(input["model"].clone()) {
            Ok(p) => p,
            Err(e) => {
                out.error = fail("invalid_input", format!("model profile: {e}"), Value::Null);
                return out;
            }
        };
        let v = profile.validate();
        if !v.is_empty() {
            out.error = fail("invalid_input", "model profile is invalid".into(), json!(v));
            return out;
        }
        let mut inputs = Vec::new();
        for (i, x) in input["inputs"].as_array().into_iter().flatten().enumerate() {
            let s = |k: &str| x.get(k).and_then(Value::as_str).map(str::to_string);
            inputs.push(
                match (s("text"), s("dataBase64"), s("path"), s("artifact")) {
                    (Some(t), None, None, None) => Input::Text(t),
                    (None, Some(b), None, None) => match xeet_engine::unb64(&b) {
                        Some(bytes) => Input::Bytes {
                            bytes,
                            declared_type: s("declaredType"),
                            name: s("name"),
                        },
                        None => {
                            out.error = fail(
                                "invalid_input",
                                format!("inputs/{i}/dataBase64 is not base64"),
                                Value::Null,
                            );
                            return out;
                        }
                    },
                    (None, None, Some(p), None) => Input::Path(p.into()),
                    (None, None, None, Some(a)) => Input::Artifact(a),
                    _ => {
                        out.error = fail(
                            "invalid_input",
                            format!(
                                "inputs/{i}: give exactly one of text, dataBase64, path, artifact"
                            ),
                            Value::Null,
                        );
                        return out;
                    }
                },
            );
        }
        let now = input
            .get("now")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| session.now.clone())
            .unwrap_or_default();
        let g = Gateway::new(self.providers.clone());
        let p = g.prepare(
            &Task {
                instruction: input["instruction"].as_str().unwrap_or("").into(),
                inputs,
            },
            &profile,
            session,
            &now,
        );
        out.artifacts = p.parts.iter().filter_map(|x| x.artifact.clone()).collect();
        let result = json!({
            "content": p.request.messages[0].content,
            "parts": p.parts,
            "budget": p.budget,
            "requestDigest": p.request_digest,
        });
        out.result_digest = Some(digest(&result));
        out.result = Some(result);
        out.ok = true;
        out
    }
}
