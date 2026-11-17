//! The gateway: inputs a model cannot consume become representations it can,
//! injected beside the user's unchanged instruction, with a record of every
//! decision.
//!
//! The gateway owns coordination only. Providers own execution (through
//! their contracts, invoked with the host's session); the [`ModelProfile`]
//! owns what the model accepts; the runtime's [`ModelClient`] owns talking to
//! the model. The gateway knows **contract ids** — `representation.retrieve`,
//! `representation.capture`, `vision.read`, `representation.annotate`,
//! `representation.understand`, `representation.inspect` — never a specific
//! provider: whichever provider declares the contract, matches the input
//! modality, and is authorized for this session is used, in the host's
//! provider order after discovery ranking.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use xeet_artifact::canon;
use xeet_engine::contract::{modality, Contract};
use xeet_engine::discover::{self, Query};
use xeet_engine::{missing_scopes, schema, store, Outcome, Provider, Session};

use crate::inspect::{inspect, Class, Input, Inspection};
use crate::model::{Acceptance, InfoSource, ModelProfile};
use crate::request::{
    Classification, Classifier, Content, ErrorKind, Message, ModelClient, ModelRequest,
    ModelResponse, Role,
};

pub const RECORD_FORMAT: &str = "modality-gateway.record";
pub const RECORD_VERSION: &str = "0.1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnknownPolicy {
    /// Represent inputs whose support is unknown (no rejection risk).
    Transform,
    /// Send them as-is and rely on the rejection fallback.
    Send,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub unknown_policy: UnknownPolicy,
    /// Fallback attempts after a confirmed modality rejection. Bounded.
    pub max_fallbacks: u8,
    /// Projection budget when the profile states no context size.
    pub default_budget_chars: usize,
    pub default_chars_per_token: f64,
    pub reserve_output_tokens: u64,
    /// Per-string truncation inside projections (always declared).
    pub max_text: Option<usize>,
    pub classifier: Classifier,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            unknown_policy: UnknownPolicy::Transform,
            max_fallbacks: 1,
            default_budget_chars: 24_000,
            default_chars_per_token: 4.0,
            reserve_output_tokens: 1024,
            max_text: Some(600),
            classifier: Classifier::default(),
        }
    }
}

/// What the agent assembled: the user's instruction and its inputs.
#[derive(Clone, Debug)]
pub struct Task {
    pub instruction: String,
    pub inputs: Vec<Input>,
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub provider: String,
    pub capability: String,
    pub version: String,
    pub score: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    /// `reuse`, `ingest`, `read`, `annotate`, `validate`, `project`.
    pub purpose: String,
    pub query: Value,
    pub candidates: Vec<Candidate>,
    /// Candidates passed over, with why (`unauthorized: missing …`, …).
    pub skipped: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<Candidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_digest: Option<String>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    /// Declared non-capabilities discovery reported, when nothing matched.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub not_provided: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PartRecord {
    pub inspection: Inspection,
    pub acceptance: Acceptance,
    /// `pass-through`, `represent`, `unavailable`.
    pub decision: String,
    pub reason: String,
    pub steps: Vec<Step>,
    /// The artifact whose projection was delivered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    pub reused: bool,
    /// What the projection delivered and left out (from the provider).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_chars: Option<usize>,
    /// Whether a reader supplied content; if not, the content stays unknown.
    pub content_read: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    pub chars_per_token: f64,
    pub reserved_output_tokens: u64,
    pub fixed_chars: usize,
    pub available_chars: usize,
    pub per_representation_chars: usize,
    pub represented: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    /// The request to send. Never part of a serialized record: records keep
    /// digests and sizes, not content.
    #[serde(skip, default)]
    pub request: ModelRequest,
    pub parts: Vec<PartRecord>,
    pub budget: Budget,
    pub request_digest: String,
    /// Content kinds and sizes sent; never the content itself.
    pub request_summary: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub n: usize,
    /// Profile adjustments applied for this attempt (observed rejections).
    pub observed: Vec<Value>,
    pub prepared: Prepared,
    /// `accepted`, or `rejected`.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification: Option<Classification>,
    /// The provider's error message, at most 300 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_excerpt: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub format: String,
    pub version: String,
    /// `sha256:` of this record's canonical JSON with `id` empty.
    pub id: String,
    /// Host-supplied time of the run.
    pub at: String,
    pub instruction_digest: String,
    pub instruction_chars: usize,
    pub model: Value,
    pub providers: Vec<Value>,
    pub attempts: Vec<Attempt>,
    pub fallback: Value,
    /// `accepted`, `rejected`, or `not-sent` (preparation failed).
    pub result: String,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub response: Option<ModelResponse>,
    pub record: RunRecord,
}

// ---------------------------------------------------------------------------

pub struct Gateway<'a> {
    pub providers: Vec<&'a dyn Provider>,
    pub options: Options,
}

fn short(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

impl<'a> Gateway<'a> {
    pub fn new(providers: Vec<&'a dyn Provider>) -> Self {
        Gateway {
            providers,
            options: Options::default(),
        }
    }

    fn manifests(&self) -> Vec<xeet_engine::contract::Manifest> {
        self.providers
            .iter()
            .map(|p| p.manifest().clone())
            .collect()
    }

    /// Finds providers for a known contract id via manifest discovery, in
    /// discovery order, and invokes the first one this session authorizes.
    /// `accept` lets the caller reject a candidate before invocation.
    fn call(
        &self,
        purpose: &str,
        capability: &str,
        query: Query,
        input: impl Fn(&dyn Provider) -> Value,
        accept: impl Fn(&dyn Provider) -> Option<String>,
        session: &Session,
    ) -> (Step, Option<(Outcome, usize)>) {
        let d = discover::discover(&self.manifests(), &query);
        let mut step = Step {
            purpose: purpose.into(),
            query: serde_json::to_value(&query).unwrap(),
            not_provided: d
                .not_provided
                .iter()
                .map(|n| serde_json::to_value(n).unwrap())
                .collect(),
            ..Step::default()
        };
        for m in d.matches.iter().filter(|m| m.capability == capability) {
            let cand = Candidate {
                provider: m.provider.clone(),
                capability: m.capability.clone(),
                version: m.version.clone(),
                score: m.score,
            };
            step.candidates.push(cand.clone());
            let Some((pi, p)) = self
                .providers
                .iter()
                .enumerate()
                .find(|(_, p)| p.manifest().provider.id == m.provider)
            else {
                continue;
            };
            let contract: &Contract = match p
                .manifest()
                .capabilities
                .iter()
                .find(|c| c.id == capability)
            {
                Some(c) => c,
                None => continue,
            };
            if let Some(why) = accept(*p) {
                step.skipped.push((m.provider.clone(), why));
                continue;
            }
            // Discovery granted nothing: authority is checked here, and again
            // by the provider itself.
            let missing = missing_scopes(contract, session);
            if !missing.is_empty() {
                step.skipped.push((
                    m.provider.clone(),
                    format!("unauthorized: this session lacks {}", missing.join(", ")),
                ));
                continue;
            }
            let inp = input(*p);
            let out = p.invoke(capability, &inp, session);
            step.selected = Some(cand);
            step.input_digest = Some(out.input_digest.clone());
            step.result_digest = out.result_digest.clone();
            step.ok = out.ok;
            step.error = out.error.as_ref().map(|e| serde_json::to_value(e).unwrap());
            // Do not trust a provider's result shape: check it against its own contract.
            if let Some(r) = &out.result {
                let problems = schema::validate(&contract.output_schema, r);
                if !problems.is_empty() {
                    step.ok = false;
                    step.error = Some(
                        json!({"code": "invalid_result", "message": "result does not match the provider's declared output schema", "details": problems}),
                    );
                    return (step, None);
                }
            }
            return (step, out.ok.then_some((out, pi)));
        }
        if step.candidates.is_empty() {
            step.error = Some(
                json!({"code": "no_provider", "message": format!("no provider declares {capability} for this input")}),
            );
        } else if step.selected.is_none() {
            step.error = Some(
                json!({"code": "no_authorized_provider", "message": format!("every provider of {capability} was skipped")}),
            );
        }
        (step, None)
    }

    /// Fetches an artifact by id from the provider that made it and checks
    /// that its content hashes to that id.
    fn validate(&self, provider: usize, id: &str, session: &Session) -> Step {
        let p = self.providers[provider];
        let out = p.invoke("representation.inspect", &json!({"artifact": id}), session);
        let mut step = Step {
            purpose: "validate".into(),
            query: json!({"artifact": id}),
            selected: Some(Candidate {
                provider: p.manifest().provider.id.clone(),
                capability: "representation.inspect".into(),
                version: out.version.clone(),
                score: 0,
            }),
            input_digest: Some(out.input_digest.clone()),
            result_digest: out.result_digest.clone(),
            ..Step::default()
        };
        match out
            .result
            .as_ref()
            .map(|r| canon::document_id(&r["document"]))
        {
            Some(computed) if computed == id => step.ok = true,
            Some(computed) => {
                step.error = Some(
                    json!({"code": "invalid_artifact_digest", "message": format!("provider returned id {id}, content hashes to {computed}")}),
                )
            }
            None => step.error = out.error.map(|e| serde_json::to_value(e).unwrap()),
        }
        step
    }

    fn bytes_of(&self, input: &Input, session: &Session) -> Result<Vec<u8>, String> {
        match input {
            Input::Bytes { bytes, .. } => Ok(bytes.clone()),
            Input::Text(t) => Ok(t.as_bytes().to_vec()),
            Input::Path(p) => {
                let path = xeet_engine::check_path(session, &p.to_string_lossy())
                    .map_err(|e| e.message)?;
                let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                if len > xeet_engine::contract::MAX_INPUT_BYTES {
                    return Err(format!(
                        "{len} bytes exceeds the {} byte limit",
                        xeet_engine::contract::MAX_INPUT_BYTES
                    ));
                }
                std::fs::read(&path).map_err(|e| e.to_string())
            }
            Input::Artifact(_) => Err("an artifact reference has no raw bytes".into()),
        }
    }

    /// Builds a model request for `profile` from `task`. Never sends it.
    pub fn prepare(
        &self,
        task: &Task,
        profile: &ModelProfile,
        session: &Session,
        now: &str,
    ) -> Prepared {
        let mut parts = Vec::new();
        let mut raw: Vec<Option<Vec<u8>>> = Vec::new();
        // Pass 1: inspect, decide, and make or find an artifact where needed.
        for (i, input) in task.inputs.iter().enumerate() {
            let insp = match inspect(i, input, session) {
                Ok(x) => x,
                Err(e) => {
                    parts.push(PartRecord {
                        inspection: Inspection {
                            index: i,
                            origin: "unreadable".into(),
                            class: Class::Unknown,
                            media_type: "application/octet-stream".into(),
                            declared_type: None,
                            declared_name: None,
                            declared_mismatch: false,
                            bytes: 0,
                            digest: String::new(),
                            dimensions: None,
                        },
                        acceptance: Acceptance::Unknown {
                            reason: "input could not be inspected".into(),
                        },
                        decision: "unavailable".into(),
                        reason: format!("{}: {}", e.code, e.message),
                        steps: vec![],
                        artifact: None,
                        reused: false,
                        delivery: None,
                        budget_chars: None,
                        content_read: false,
                    });
                    raw.push(None);
                    continue;
                }
            };
            let acceptance = if insp.class == Class::Artifact {
                Acceptance::Unknown {
                    reason: "artifacts are delivered as projections".into(),
                }
            } else {
                profile.accepts(&insp.media_type, insp.bytes, insp.dimensions)
            };
            let mut rec = PartRecord {
                inspection: insp.clone(),
                acceptance: acceptance.clone(),
                decision: String::new(),
                reason: String::new(),
                steps: vec![],
                artifact: None,
                reused: false,
                delivery: None,
                budget_chars: None,
                content_read: false,
            };
            let send_as_is = match &acceptance {
                Acceptance::Accept { .. } => true,
                Acceptance::Unknown { .. } => {
                    insp.class != Class::Artifact
                        && self.options.unknown_policy == UnknownPolicy::Send
                }
                Acceptance::Reject { .. } => false,
            } || insp.class == Class::Text;
            if send_as_is {
                rec.decision = "pass-through".into();
                rec.reason = match &acceptance {
                    Acceptance::Accept { rule, source } => format!(
                        "model accepts {} (rule {rule}, {source:?})",
                        insp.media_type
                    ),
                    _ => format!(
                        "sent as-is: {}",
                        if insp.class == Class::Text {
                            "text".to_string()
                        } else {
                            "support unknown, policy send".to_string()
                        }
                    ),
                };
                match self.bytes_of(input, session) {
                    Ok(b) => raw.push(Some(b)),
                    Err(e) => {
                        rec.decision = "unavailable".into();
                        rec.reason = e;
                        raw.push(None);
                    }
                }
                parts.push(rec);
                continue;
            }
            rec.reason = match &acceptance {
                Acceptance::Reject { reason, source, .. } => {
                    format!("model does not accept it: {reason} ({source:?})")
                }
                Acceptance::Unknown { reason } if insp.class == Class::Artifact => reason.clone(),
                Acceptance::Unknown { reason } => format!("{reason}; policy transform"),
                Acceptance::Accept { .. } => unreachable!(),
            };
            raw.push(None);
            if insp.class == Class::Artifact {
                rec.decision = "represent".into();
                rec.artifact = Some(store::uri(&insp.digest));
                rec.reused = true;
                parts.push(rec);
                continue;
            }
            self.represent(input, &insp, &mut rec, session, now, None);
            parts.push(rec);
        }

        // Pass 2: budget, then projections.
        let cpt = profile
            .chars_per_token
            .unwrap_or(self.options.default_chars_per_token);
        let represented = parts.iter().filter(|p| p.decision == "represent").count();
        let text_chars: usize = task
            .inputs
            .iter()
            .zip(&parts)
            .filter(|(_, p)| p.decision == "pass-through" && p.inspection.class == Class::Text)
            .map(|(_, p)| p.inspection.bytes as usize)
            .sum();
        let fixed = task.instruction.chars().count() + text_chars + 700 * parts.len();
        let (source, available, reserve) = match profile.context_tokens {
            Some(ctx) => {
                let reserve = profile
                    .max_output_tokens
                    .unwrap_or(self.options.reserve_output_tokens);
                let usable = (ctx.saturating_sub(reserve) as f64 * cpt) as usize;
                ("profile".to_string(), usable.saturating_sub(fixed), reserve)
            }
            None => (
                "gateway-default (profile states no context size)".to_string(),
                self.options.default_budget_chars,
                self.options.reserve_output_tokens,
            ),
        };
        let per = available.checked_div(represented).map_or(0, |p| p.max(1));
        let budget = Budget {
            source,
            context_tokens: profile.context_tokens,
            chars_per_token: cpt,
            reserved_output_tokens: reserve,
            fixed_chars: fixed,
            available_chars: available,
            per_representation_chars: per,
            represented,
        };

        let mut content = vec![Content::Text {
            text: task.instruction.clone(),
        }];
        let total = parts.len();
        for (rec, bytes) in parts.iter_mut().zip(raw) {
            let i = &rec.inspection;
            let head = format!(
                "[attachment {} of {total}: {}{}{}, {}]",
                i.index + 1,
                i.media_type,
                i.dimensions
                    .map(|(w, h)| format!(" {w}x{h}"))
                    .unwrap_or_default(),
                i.declared_name
                    .as_deref()
                    .map(|n| format!(" \"{n}\""))
                    .unwrap_or_default(),
                if i.digest.is_empty() {
                    "no digest".to_string()
                } else {
                    i.digest.clone()
                }
            );
            match rec.decision.as_str() {
                "pass-through" => {
                    let b = bytes.unwrap_or_default();
                    if i.class == Class::Text {
                        content.push(Content::Text {
                            text: format!("{head}\n{}", String::from_utf8_lossy(&b)),
                        });
                    } else {
                        content.push(Content::Media {
                            media_type: i.media_type.clone(),
                            data_base64: xeet_engine::b64(&b),
                            name: i.declared_name.clone(),
                        });
                    }
                }
                "represent" => {
                    let artifact = rec.artifact.clone().unwrap_or_default();
                    rec.budget_chars = Some(per);
                    let (step, out) = self.call(
                        "project",
                        "representation.understand",
                        Query { has: vec![modality::ARTIFACT.into()], accepts: vec![modality::TEXT.into()], ..Query::default() },
                        |_| {
                            let mut v = json!({"artifact": artifact, "consumer": "text", "format": "text", "maxChars": per});
                            if let Some(m) = self.options.max_text {
                                v["maxText"] = json!(m);
                            }
                            v
                        },
                        |_| None,
                        session,
                    );
                    rec.steps.push(step);
                    match out {
                        Some((o, _)) => {
                            let r = o.result.unwrap_or_default();
                            rec.delivery = r.get("delivery").cloned();
                            let reader = if rec.content_read {
                                "a reader's output, marked !inferred: read from pixels, may be wrong"
                            } else if i.class == Class::Artifact {
                                "an existing artifact"
                            } else {
                                "no reader was available: the content itself is unknown and marked [visual-only]"
                            };
                            content.push(Content::Text {
                                text: format!(
                                    "{head}\nThis model cannot take {} directly, so the original is not included; it is kept by the runtime as {artifact}. Below is a text projection of it ({reader}). Marks: unmarked = from the source; ~derived; ^observed (measured); !inferred (may be wrong); ?unknown. For more detail, request {artifact} with an element id (focus) or a section.\n{}\n[end of attachment {}]",
                                    i.media_type,
                                    r["projection"].as_str().unwrap_or(""),
                                    i.index + 1
                                ),
                            });
                        }
                        None => {
                            rec.decision = "unavailable".into();
                            rec.reason = format!("{}; projection failed", rec.reason);
                            content.push(Content::Text { text: format!("{head}\nNot delivered: {}. The original is kept by the runtime.", rec.reason) });
                        }
                    }
                }
                _ => {
                    content.push(Content::Text {
                        text: format!("{head}\nNot delivered: {}. No authorized provider could represent it for this model. The original is kept by the runtime.", rec.reason),
                    });
                }
            }
        }
        let request = ModelRequest {
            provider: profile.provider.clone(),
            model: profile.model.clone(),
            messages: vec![Message {
                role: Role::User,
                content,
            }],
        };
        let request_digest =
            canon::sha256_id(canon::canonical(&serde_json::to_value(&request).unwrap()).as_bytes());
        let request_summary = request.messages[0]
            .content
            .iter()
            .map(|c| match c {
                Content::Text { text } => json!({"type": "text", "chars": text.chars().count(), "sha256": canon::sha256_id(text.as_bytes())}),
                Content::Media { media_type, data_base64, .. } => json!({"type": "media", "mediaType": media_type, "base64Chars": data_base64.len()}),
            })
            .collect();
        Prepared {
            request,
            parts,
            budget,
            request_digest,
            request_summary,
        }
    }

    /// Readers (`vision.read` providers) for this media type that the session
    /// authorizes, as `provider@version` — the identity a reading carries.
    fn readers(&self, mt: &str, session: &Session) -> Vec<String> {
        self.providers
            .iter()
            .filter_map(|p| {
                let m = p.manifest();
                let c = m.capabilities.iter().find(|c| c.id == "vision.read")?;
                (c.input_modalities.iter().any(|x| x == mt)
                    && missing_scopes(c, session).is_empty())
                .then(|| format!("{}@{}", m.provider.id, m.provider.version))
            })
            .collect()
    }

    /// Reuse, or ingest + read + annotate, for one input the model cannot take.
    ///
    /// Reuse is by content and by reader identity: a reading made by an
    /// available reader with the same id and version (which for local OCR
    /// includes its engine and configuration) is reused as is; otherwise the
    /// bare ingested artifact is reused and only the reading runs. With a
    /// capture context, only artifacts from the same pixels *and* the same
    /// context are the same observation.
    fn represent(
        &self,
        input: &Input,
        insp: &Inspection,
        rec: &mut PartRecord,
        session: &Session,
        now: &str,
        context: Option<&Value>,
    ) {
        rec.decision = "represent".into();
        let mt = insp.media_type.clone();
        let readers = self.readers(&mt, session);
        let context_digest = context.map(|c| canon::sha256_id(canon::canonical(c).as_bytes()));

        // 1. Reuse.
        let mut base: Option<String> = None;
        let (step, found) = self.call(
            "reuse",
            "representation.retrieve",
            Query {
                has: vec![modality::JSON.into()],
                ..Query::default()
            },
            |_| json!({"inputDigest": insp.digest, "limit": 200}),
            |_| None,
            session,
        );
        rec.steps.push(step);
        if let Some((o, pi)) = found {
            let list: Vec<Value> = o
                .result
                .as_ref()
                .and_then(|r| r["artifacts"].as_array().cloned())
                .unwrap_or_default();
            let same_observation = |a: &&Value| {
                let inputs: Vec<&str> = a["inputs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                match &context_digest {
                    Some(d) => inputs.contains(&d.as_str()),
                    None => inputs.len() == 1,
                }
            };
            let newest = |xs: Vec<&Value>| -> Option<Value> {
                xs.into_iter()
                    .max_by(|a, b| {
                        a["capturedAt"]
                            .as_str()
                            .cmp(&b["capturedAt"].as_str())
                            .then(b["id"].as_str().cmp(&a["id"].as_str()))
                    })
                    .cloned()
            };
            let annotated_by = |a: &Value, ids: &[String]| {
                a["annotatedBy"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .any(|x| ids.iter().any(|r| r == x))
            };
            let pool: Vec<&Value> = list.iter().filter(same_observation).collect();
            let by_available = newest(
                pool.iter()
                    .copied()
                    .filter(|a| annotated_by(a, &readers))
                    .collect(),
            );
            let any_reading = newest(
                pool.iter()
                    .copied()
                    .filter(|a| a["inferred"].as_u64().unwrap_or(0) > 0)
                    .collect(),
            );
            let bare = newest(
                pool.iter()
                    .copied()
                    .filter(|a| {
                        a["inferred"].as_u64().unwrap_or(0) == 0 && a["derivedFrom"].is_null()
                    })
                    .collect(),
            );
            let (choice, finished) = match (&by_available, readers.is_empty()) {
                (Some(a), _) => (Some(a.clone()), true),
                (None, false) => (bare.clone(), false),
                (None, true) => (any_reading.clone().or(bare.clone()), true),
            };
            if let Some(a) = choice {
                let id = a["id"].as_str().unwrap_or("").to_string();
                let v = self.validate(pi, &id, session);
                let ok = v.ok;
                rec.steps.push(v);
                if ok {
                    rec.reused = true;
                    if finished {
                        rec.artifact = Some(store::uri(&id));
                        rec.content_read = a["inferred"].as_u64().unwrap_or(0) > 0;
                        return;
                    }
                    base = Some(id);
                }
            }
        }

        // 2. Ingest, unless a bare artifact for this observation was reused.
        let raw = match input {
            Input::Path(_) => None,
            _ => self.bytes_of(input, session).ok(),
        };
        let path = match input {
            Input::Path(p) => Some(p.to_string_lossy().to_string()),
            _ => None,
        };
        let mut id = match base {
            Some(id) => id,
            None => {
                let source = if context.is_some() {
                    "screenshot"
                } else {
                    "image"
                };
                let (step, made) = self.call(
                    "ingest",
                    "representation.capture",
                    Query { outcome: Some(format!("represent an {} file", insp.class_name())), has: vec![mt.clone()], accepts: vec![modality::ARTIFACT.into(), modality::JSON.into()], ..Query::default() },
                    |_| {
                        let mut v = json!({"source": source, "capturedAt": now, "method": "modality-gateway"});
                        match (&path, &raw) {
                            (Some(p), _) => v["path"] = json!(p),
                            (None, Some(b)) => v["dataBase64"] = json!(xeet_engine::b64(b)),
                            _ => {}
                        }
                        if let Some(n) = &insp.declared_name {
                            v["name"] = json!(n);
                        }
                        if let Some(t) = &insp.declared_type {
                            v["declaredType"] = json!(t);
                        }
                        if let Some(c) = context {
                            v["context"] = c.clone();
                        }
                        v
                    },
                    |p| {
                        (!p.manifest().sources.iter().any(|s| s.kind == "image") || insp.class != Class::Image)
                            .then(|| format!("{} has no source adapter for {mt}", p.manifest().provider.id))
                    },
                    session,
                );
                let failed = step.error.clone();
                rec.steps.push(step);
                let Some((o, pi)) = made else {
                    rec.decision = "unavailable".into();
                    rec.reason = format!(
                        "{}; {}",
                        rec.reason,
                        failed
                            .and_then(|e| e["message"].as_str().map(str::to_string))
                            .unwrap_or_else(|| "ingest failed".into())
                    );
                    return;
                };
                let id = o
                    .result
                    .as_ref()
                    .and_then(|r| r["id"].as_str())
                    .unwrap_or("")
                    .to_string();
                let v = self.validate(pi, &id, session);
                let ok = v.ok;
                rec.steps.push(v);
                if !ok {
                    rec.decision = "unavailable".into();
                    rec.reason = format!(
                        "{}; the ingested artifact failed digest validation",
                        rec.reason
                    );
                    return;
                }
                id
            }
        };

        // 3. Read: any provider of vision.read for this modality. Optional.
        let bytes = raw
            .clone()
            .or_else(|| self.bytes_of(input, session).ok())
            .unwrap_or_default();
        let artifact_uri = store::uri(&id);
        let (step, read) = self.call(
            "read",
            "vision.read",
            Query { outcome: Some("read text from an image".into()), has: vec![mt.clone()], ..Query::default() },
            |_| json!({"artifact": artifact_uri, "parent": "image", "image": {"mediaType": mt, "dataBase64": xeet_engine::b64(&bytes)}}),
            |_| None,
            session,
        );
        rec.steps.push(step);
        if let Some((o, _)) = read {
            if let Some(annotation) = o.result.as_ref().and_then(|r| r.get("annotation")).cloned() {
                // 4. Annotate: the reading enters only as declared inference,
                // in a new artifact derived from the original.
                let (step, ann) = self.call(
                    "annotate",
                    "representation.annotate",
                    Query {
                        has: vec![modality::ARTIFACT.into()],
                        ..Query::default()
                    },
                    |_| json!({"artifact": id, "annotation": annotation}),
                    |_| None,
                    session,
                );
                rec.steps.push(step);
                if let Some((o, pi)) = ann {
                    let new_id = o
                        .result
                        .as_ref()
                        .and_then(|r| r["id"].as_str())
                        .unwrap_or("")
                        .to_string();
                    let v = self.validate(pi, &new_id, session);
                    if v.ok {
                        id = new_id;
                        rec.content_read = true;
                    }
                    rec.steps.push(v);
                }
            }
        }
        rec.artifact = Some(store::uri(&id));
    }

    /// The capture path: makes (or reuses) the artifact for one input and
    /// reads it with any authorized reader, without any model involved.
    /// `context` is a screenshot's capture context.
    pub fn derive(
        &self,
        input: &Input,
        session: &Session,
        now: &str,
        context: Option<&Value>,
    ) -> PartRecord {
        let mut rec = PartRecord {
            inspection: Inspection {
                index: 0,
                origin: "unreadable".into(),
                class: Class::Unknown,
                media_type: "application/octet-stream".into(),
                declared_type: None,
                declared_name: None,
                declared_mismatch: false,
                bytes: 0,
                digest: String::new(),
                dimensions: None,
            },
            acceptance: Acceptance::Unknown {
                reason: "capture: no model profile".into(),
            },
            decision: String::new(),
            reason: "capture".into(),
            steps: vec![],
            artifact: None,
            reused: false,
            delivery: None,
            budget_chars: None,
            content_read: false,
        };
        match inspect(0, input, session) {
            Ok(i) if i.class == Class::Image => {
                rec.inspection = i.clone();
                self.represent(input, &i, &mut rec, session, now, context);
            }
            Ok(i) => {
                rec.reason = format!("capture takes images; this is {}", i.media_type);
                rec.inspection = i;
                rec.decision = "unavailable".into();
            }
            Err(e) => {
                rec.decision = "unavailable".into();
                rec.reason = format!("{}: {}", e.code, e.message);
            }
        }
        rec
    }

    /// Prepares, sends, and on a confirmed modality rejection re-prepares
    /// with that modality marked unsupported and sends once more (bounded by
    /// `max_fallbacks`). Nothing else triggers a fallback.
    pub fn run(
        &self,
        task: &Task,
        profile: &ModelProfile,
        client: &mut dyn ModelClient,
        session: &Session,
        now: &str,
    ) -> Run {
        let mut current = profile.clone();
        let mut attempts: Vec<Attempt> = Vec::new();
        let mut observed: Vec<Value> = Vec::new();
        let mut fallback = json!({"triggered": false});
        let mut response = None;
        let mut result = "rejected".to_string();
        for n in 0..=self.options.max_fallbacks as usize {
            let prepared = self.prepare(task, &current, session, now);
            let sent_media = prepared.request.media_types();
            match client.send(&prepared.request) {
                Ok(r) => {
                    attempts.push(Attempt {
                        n,
                        observed: observed.clone(),
                        prepared,
                        outcome: "accepted".into(),
                        status: None,
                        classification: None,
                        error_excerpt: None,
                    });
                    response = Some(r);
                    result = "accepted".into();
                    if n > 0 {
                        fallback["result"] = json!("accepted after fallback");
                    }
                    break;
                }
                Err(e) => {
                    let c = self.options.classifier.classify(&e);
                    let excerpt = Some(short(&e.body, 300));
                    attempts.push(Attempt {
                        n,
                        observed: observed.clone(),
                        prepared,
                        outcome: "rejected".into(),
                        status: e.status,
                        classification: Some(c.clone()),
                        error_excerpt: excerpt,
                    });
                    if c.kind != ErrorKind::UnsupportedModality {
                        fallback = json!({"triggered": false, "reason": format!("{:?} is not a modality error; no transformation can fix it", c.kind)});
                        break;
                    }
                    let targets: Vec<String> = sent_media
                        .iter()
                        .filter(|m| {
                            c.modality_family
                                .as_deref()
                                .is_none_or(|f| m.starts_with(f))
                        })
                        .cloned()
                        .collect();
                    if targets.is_empty() {
                        fallback = json!({"triggered": false, "reason": "the rejection names a modality, but the request carried no such media: nothing to transform"});
                        break;
                    }
                    if n >= self.options.max_fallbacks as usize {
                        fallback["result"] = json!("rejected again; fallback limit reached");
                        break;
                    }
                    for t in &targets {
                        current.observe_rejection(
                            t,
                            &format!("rejected at attempt {n}: HTTP {:?}", e.status),
                        );
                        observed.push(json!({"mediaType": t, "support": "unsupported", "source": InfoSource::ObservedRejection}));
                    }
                    fallback = json!({"triggered": true, "reason": "confirmed modality rejection", "mediaTypes": targets});
                }
            }
        }
        if attempts.is_empty() {
            result = "not-sent".into();
        }
        let mut record = RunRecord {
            format: RECORD_FORMAT.into(),
            version: RECORD_VERSION.into(),
            id: String::new(),
            at: now.into(),
            instruction_digest: canon::sha256_id(task.instruction.as_bytes()),
            instruction_chars: task.instruction.chars().count(),
            model: json!({
                "provider": profile.provider, "model": profile.model, "revision": profile.revision,
                "profileSource": profile.source,
                "profileDigest": canon::sha256_id(canon::canonical(&serde_json::to_value(profile).unwrap()).as_bytes()),
                "declared": profile.inputs,
                "observed": observed,
            }),
            providers: self.providers.iter().map(|p| json!({"id": p.manifest().provider.id, "version": p.manifest().provider.version})).collect(),
            attempts,
            fallback,
            result,
        };
        let v = serde_json::to_value(&record).unwrap();
        record.id = canon::document_id(&v);
        Run { response, record }
    }

    /// Progressive retrieval: more of an artifact, for a runtime to expose to
    /// the model as a follow-up (a tool call, a second turn).
    pub fn expand(
        &self,
        artifact: &str,
        focus: Option<&str>,
        sections: &[&str],
        max_chars: Option<usize>,
        session: &Session,
    ) -> Result<String, Value> {
        let (step, out) = self.call(
            "expand",
            "representation.understand",
            Query {
                has: vec![modality::ARTIFACT.into()],
                accepts: vec![modality::TEXT.into()],
                ..Query::default()
            },
            |_| {
                let mut v = json!({"artifact": artifact, "consumer": "text", "format": "text"});
                if let Some(f) = focus {
                    v["focus"] = json!(f);
                }
                if !sections.is_empty() {
                    v["sections"] = json!(sections);
                }
                if let Some(m) = max_chars {
                    v["maxChars"] = json!(m);
                }
                v
            },
            |_| None,
            session,
        );
        match out {
            Some((o, _)) => Ok(o.result.unwrap_or_default()["projection"]
                .as_str()
                .unwrap_or("")
                .to_string()),
            None => Err(step.error.unwrap_or(Value::Null)),
        }
    }
}

impl Inspection {
    fn class_name(&self) -> &'static str {
        match self.class {
            Class::Image => "image",
            Class::Audio => "audio",
            Class::Video => "video",
            Class::Document => "document",
            Class::Structured => "structured data",
            Class::Text => "text",
            Class::Artifact => "artifact",
            Class::Unknown => "unknown",
        }
    }
}
