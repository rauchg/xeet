//! Deterministic stand-ins for the parts this repository does not own: a
//! vision provider and model providers. They are test harness, labelled as
//! such everywhere they surface; nothing here claims to be a real model.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use xeet_engine::contract::{self, Constraints, Contract, Manifest, Mode};
use xeet_engine::{schema, store::Store, Engine, Outcome, Provider, Session};
use xeet_gateway::model::ModelProfile;
use xeet_gateway::request::{Content, ModelClient, ModelRequest, ModelResponse, ProviderError};

pub const T: &str = "2026-11-16T12:00:00Z";

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(root().join("tests/fixtures/gateway").join(name)).unwrap()
}

pub fn profile(name: &str) -> ModelProfile {
    let p: ModelProfile =
        serde_json::from_slice(&fixture(&format!("profile-{name}.json"))).unwrap();
    assert!(p.validate().is_empty(), "{:?}", p.validate());
    p
}

pub fn engine(name: &str) -> Engine {
    let dir = std::env::temp_dir().join(format!("xeet-gateway-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Engine::new(Store::open(&dir).unwrap())
}

pub fn session(grants: &[&str]) -> Session {
    Session {
        grants: grants.iter().map(|g| g.to_string()).collect(),
        read_roots: vec![root().join("tests")],
        now: Some(T.into()),
    }
}

pub fn all_grants() -> Session {
    session(&["store:read", "store:write", "fs:read", "vision:read"])
}

// ---------------------------------------------------------------------------
// A stand-in vision provider: implements the `vision.read` contract by
// returning a hand-authored reading for the one image it knows.
// ---------------------------------------------------------------------------

pub struct TestReader {
    manifest: Manifest,
    reading: Value,
}

impl TestReader {
    pub fn new() -> TestReader {
        let mut manifest = contract::manifest();
        manifest.provider.id = "test-vision-reader".into();
        manifest.provider.name =
            "Test vision reader (HARNESS: returns a hand-authored reading)".into();
        manifest.provider.version = "0.0.1".into();
        manifest.sources.clear();
        manifest.non_capabilities.clear();
        manifest.authority.scopes = vec![contract::Scope {
            id: "vision:read".into(),
            meaning: "send image bytes to this reader".into(),
        }];
        manifest.capabilities = vec![Contract {
            id: "vision.read".into(),
            version: "0.1".into(),
            title: "Read an image".into(),
            summary: "Read text and structure from an image; returns an annotation whose content is all inferred.".into(),
            description: "Test harness.".into(),
            outcomes: vec!["read text from an image".into(), "ocr an arbitrary image".into(), "describe a picture".into()],
            input_modalities: vec!["image/png".into(), "image/jpeg".into()],
            modes: vec![Mode { output: vec![contract::modality::JSON.into()], select: json!({}), note: "annotation".into() }],
            input_schema: json!({"type": "object", "required": ["image", "parent"], "properties": {
                "artifact": {"type": "string"}, "parent": {"type": "string"},
                "image": {"type": "object", "required": ["mediaType", "dataBase64"]}}}),
            output_schema: json!({"type": "object", "required": ["annotation"], "properties": {"annotation": {"type": "object", "required": ["parent", "by", "method", "at"]}}}),
            effects: "none".into(),
            consequential: false,
            authority: vec!["vision:read".into()],
            preconditions: vec![],
            guarantees: vec!["every element and fact it returns is marked inferred".into()],
            limitations: vec!["test harness: knows one image".into()],
            constraints: Constraints { network: "none".into(), deterministic: true, max_input_bytes: 16 << 20, notes: vec![] },
            errors: vec!["invalid_input".into(), "unauthorized".into()],
        }];
        TestReader {
            manifest,
            reading: serde_json::from_slice(&fixture("invoices.reading.json")).unwrap(),
        }
    }
}

fn outcome(cap: &str, input: &Value, result: Result<Value, Value>) -> Outcome {
    let digest =
        |v: &Value| xeet_artifact::canon::sha256_id(xeet_artifact::canon::canonical(v).as_bytes());
    let (ok, result, error) = match result {
        Ok(r) => (true, Some(r), None),
        Err(e) => (false, None, Some(serde_json::from_value(e).unwrap())),
    };
    Outcome {
        capability: cap.into(),
        version: "0.1".into(),
        provider: "test-vision-reader".into(),
        provider_version: "0.0.1".into(),
        ok,
        result_digest: result.as_ref().map(digest),
        result,
        error,
        input_digest: digest(input),
        artifacts: vec![],
        authority_checked: vec!["vision:read".into()],
    }
}

impl Provider for TestReader {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn invoke(&self, cap: &str, input: &Value, s: &Session) -> Outcome {
        let c = &self.manifest.capabilities[0];
        if cap != c.id {
            return outcome(
                cap,
                input,
                Err(json!({"code": "not_found", "message": cap})),
            );
        }
        if !s.grants.contains("vision:read") {
            return outcome(
                cap,
                input,
                Err(
                    json!({"code": "unauthorized", "message": "needs vision:read", "details": {"missing": ["vision:read"]}}),
                ),
            );
        }
        if !schema::validate(&c.input_schema, input).is_empty() {
            return outcome(
                cap,
                input,
                Err(json!({"code": "invalid_input", "message": "bad input"})),
            );
        }
        let bytes = xeet_engine::unb64(input["image"]["dataBase64"].as_str().unwrap_or(""))
            .unwrap_or_default();
        let sha = xeet_artifact::canon::sha256_id(&bytes);
        let (elements, facts) = if Some(sha.as_str()) == self.reading["imageSha256"].as_str() {
            (
                self.reading["elements"].clone(),
                self.reading["facts"].clone(),
            )
        } else {
            // An image it does not know: an honest, low-confidence nothing.
            (
                json!([{"id": "reading", "kind": "text", "role": "vision.unreadable", "text": "no legible content recognized", "visible": true, "basis": "inferred", "attrs": {"vision.confidence": 0.1}}]),
                json!([]),
            )
        };
        let annotation = json!({
            "parent": input["parent"], "elements": elements, "facts": facts,
            "by": {"name": "test-vision-reader", "version": "0.0.1"},
            "method": "harness: hand-authored reading keyed by image digest", "at": T,
        });
        outcome(cap, input, Ok(json!({"annotation": annotation})))
    }
}

// ---------------------------------------------------------------------------
// A provider that lies about artifact content (to prove digests are checked).
// ---------------------------------------------------------------------------

pub struct Tampering<'a> {
    pub inner: &'a Engine,
    pub manifest: Manifest,
}

impl<'a> Tampering<'a> {
    pub fn new(inner: &'a Engine) -> Self {
        let mut manifest = inner.manifest().clone();
        manifest.provider.id = "tampering-provider".into();
        Tampering { inner, manifest }
    }
}

impl Provider for Tampering<'_> {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn invoke(&self, cap: &str, input: &Value, s: &Session) -> Outcome {
        let mut o = self.inner.invoke(cap, input, s);
        if cap == "representation.inspect" {
            if let Some(r) = o.result.as_mut() {
                r["document"]["subject"]["title"] = json!("something else");
            }
        }
        o
    }
}

// ---------------------------------------------------------------------------
// Model stand-ins
// ---------------------------------------------------------------------------

/// A text-only model stand-in. Rejects binary media with an error body of the
/// kind providers return for unsupported images. "Answers" deterministically
/// from the text it was given: the invoice rows marked overdue, and rows it
/// cannot decide.
#[derive(Default)]
pub struct TextOnlyModel {
    pub seen: Vec<ModelRequest>,
}

pub fn texts(r: &ModelRequest) -> Vec<String> {
    r.messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.clone()),
            Content::Media { .. } => None,
        })
        .collect()
}

impl ModelClient for TextOnlyModel {
    fn send(&mut self, r: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        self.seen.push(r.clone());
        if !r.media_types().is_empty() {
            return Err(ProviderError { status: Some(400), body: "{\"error\":{\"message\":\"Invalid content type. image_url is only supported by certain models.\",\"type\":\"invalid_request_error\"}}".into(), transport: false });
        }
        let all = texts(r).join("\n");
        let mut overdue = Vec::new();
        let mut unclear = Vec::new();
        for line in all.lines() {
            let Some(id) = line
                .split_whitespace()
                .find(|w| w.starts_with("reading.INV-"))
            else {
                continue;
            };
            let inv = id.trim_start_matches("reading.");
            if line.contains("status: Overdue") {
                overdue.push(inv.to_string());
            } else if line.contains("status: unreadable") {
                unclear.push(inv.to_string());
            }
        }
        Ok(ModelResponse {
            text: format!(
                "Overdue: {}. Unclear: {}.",
                overdue.join(", "),
                unclear.join(", ")
            ),
        })
    }
}

/// A vision-capable model stand-in: accepts media and reports what it got.
#[derive(Default)]
pub struct VisionModel {
    pub seen: Vec<ModelRequest>,
}

impl ModelClient for VisionModel {
    fn send(&mut self, r: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        self.seen.push(r.clone());
        Ok(ModelResponse {
            text: format!("received media: {:?}", r.media_types()),
        })
    }
}

/// Always fails with the given status and body.
pub struct FailingModel {
    pub status: u16,
    pub body: String,
    pub sends: usize,
}

impl ModelClient for FailingModel {
    fn send(&mut self, _: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        self.sends += 1;
        Err(ProviderError {
            status: Some(self.status),
            body: self.body.clone(),
            transport: false,
        })
    }
}
