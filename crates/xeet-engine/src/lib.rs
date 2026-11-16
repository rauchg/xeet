//! `xeet-engine`: the representation engine as a provider.
//!
//! - [`contract`] — capability contracts and the versioned manifest: data,
//!   independent of any transport.
//! - [`discover`] — discovery by outcome and modality negotiation over one or
//!   more manifests (several providers may implement the same capability).
//!   Discovery cannot invoke anything.
//! - [`Engine::invoke`] — the single execution path every transport uses:
//!   input size and schema checked, then the session's authority, then the
//!   capability runs, then its output is checked against its own declared
//!   schema. Returns an [`Outcome`] with digests of input and result.
//! - [`store`] — the content-addressed artifact store.
//!
//! Three separations, enforced in code: discovery is not invocation (it works
//! on manifests only); invocation is not authorization (grants come from the
//! host's [`Session`], never from discovery or from an artifact); and no
//! capability acts at a source system.

#![forbid(unsafe_code)]

pub mod contract;
pub mod discover;
pub mod schema;
pub mod store;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use xeet_artifact::{annotate, canon, compare, project, verify, Artifact, Package};
use xeet_capture::{github, x, Reproducers};
use xeet_core::model::{RenderOptions, Surface, Theme};

use contract::{Contract, Manifest};
use store::Store;

/// What the host grants for a session. Discovery never changes it, and
/// nothing an agent sends can widen it.
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub grants: BTreeSet<String>,
    /// Directories `fs:read` may read inside. Empty: none.
    pub read_roots: Vec<PathBuf>,
    /// The host clock, as ISO 8601, for captures that do not name a time.
    pub now: Option<String>,
}

impl Session {
    pub fn with(grants: &[&str]) -> Session {
        Session {
            grants: grants.iter().map(|g| g.to_string()).collect(),
            ..Session::default()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Error {
    /// `invalid_input`, `not_found`, `unauthorized`, `limit_exceeded`,
    /// `verification_failed`, `unsupported`, or `internal`.
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Value::is_null", default)]
    pub details: Value,
}

fn err(code: &str, message: impl Into<String>, details: Value) -> Error {
    Error {
        code: code.into(),
        message: message.into(),
        details,
    }
}

/// The verifiable result of one invocation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub capability: String,
    pub version: String,
    pub provider: String,
    pub provider_version: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
    /// `sha256:` of the canonical JSON input, so a caller can prove what it asked.
    pub input_digest: String,
    /// `sha256:` of the canonical JSON result (absent on error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_digest: Option<String>,
    /// Artifacts read or created, by id.
    pub artifacts: Vec<String>,
    /// Scopes this invocation was checked against.
    pub authority_checked: Vec<String>,
}

/// Anything that implements capability contracts: this engine, or another
/// provider (a vision reader, a remote service). The gateway and the
/// transports invoke providers only through this, so a contract id means the
/// same thing whichever provider serves it.
pub trait Provider {
    fn manifest(&self) -> &Manifest;
    /// Runs one capability under the host's session. Implementations enforce
    /// their contract's authority themselves; callers may check it first too.
    fn invoke(&self, capability: &str, input: &Value, session: &Session) -> Outcome;
}

impl Provider for Engine {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn invoke(&self, capability: &str, input: &Value, session: &Session) -> Outcome {
        Engine::invoke(self, capability, input, session)
    }
}

/// Whether a session holds every scope a contract requires; the missing ones.
pub fn missing_scopes(contract: &Contract, session: &Session) -> Vec<String> {
    contract
        .authority
        .iter()
        .filter(|s| !session.grants.contains(*s))
        .cloned()
        .collect()
}

/// The `fs:read` rule every component uses: the grant, and a path inside a
/// host-given read root (after resolving symlinks and `..`).
pub fn check_path(session: &Session, path: &str) -> Result<PathBuf, Error> {
    if !session.grants.contains("fs:read") {
        return Err(err(
            "unauthorized",
            "reading a path needs fs:read, which this session lacks",
            json!({"missing": ["fs:read"]}),
        ));
    }
    let p = Path::new(path)
        .canonicalize()
        .map_err(|e| err("not_found", format!("{path}: {e}"), Value::Null))?;
    let inside = session
        .read_roots
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|r| p.starts_with(r));
    if !inside {
        return Err(err(
            "unauthorized",
            format!("{path} is outside the read roots this host allows"),
            json!({"readRoots": session.read_roots}),
        ));
    }
    Ok(p)
}

pub struct Engine {
    pub store: Store,
    manifest: Manifest,
}

impl Engine {
    pub fn new(store: Store) -> Engine {
        Engine {
            store,
            manifest: contract::manifest(),
        }
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn contract(&self, id: &str) -> Option<&Contract> {
        self.manifest.capabilities.iter().find(|c| c.id == id)
    }

    /// Runs one capability. The only execution path; every transport calls it.
    pub fn invoke(&self, capability: &str, input: &Value, session: &Session) -> Outcome {
        let input_digest = canon::sha256_id(canon::canonical(input).as_bytes());
        let mut out = Outcome {
            capability: capability.into(),
            version: String::new(),
            provider: self.manifest.provider.id.clone(),
            provider_version: self.manifest.provider.version.clone(),
            ok: false,
            result: None,
            error: None,
            input_digest,
            result_digest: None,
            artifacts: Vec::new(),
            authority_checked: Vec::new(),
        };
        let Some(c) = self.contract(capability) else {
            let known: Vec<&str> = self
                .manifest
                .capabilities
                .iter()
                .map(|c| c.id.as_str())
                .collect();
            out.error = Some(err(
                "not_found",
                format!("no capability {capability}"),
                json!({"known": known}),
            ));
            return out;
        };
        out.version = c.version.clone();
        let size = canon::canonical(input).len() as u64;
        if size > c.constraints.max_input_bytes {
            out.error = Some(err(
                "limit_exceeded",
                format!(
                    "input is {size} bytes; the limit is {}",
                    c.constraints.max_input_bytes
                ),
                Value::Null,
            ));
            return out;
        }
        let problems = schema::validate(&c.input_schema, input);
        if !problems.is_empty() {
            let details: Vec<Value> = problems
                .iter()
                .map(|(p, m)| json!({"pointer": p, "message": m}))
                .collect();
            out.error = Some(err(
                "invalid_input",
                format!("{} problem(s) in the input", details.len()),
                Value::Array(details),
            ));
            return out;
        }
        out.authority_checked = c.authority.clone();
        let missing: Vec<&String> = c
            .authority
            .iter()
            .filter(|s| !session.grants.contains(*s))
            .collect();
        if !missing.is_empty() {
            out.error = Some(err(
                "unauthorized",
                format!(
                    "this session lacks {:?}; discovery and availability grant nothing",
                    missing
                ),
                json!({"required": c.authority, "missing": missing}),
            ));
            return out;
        }
        let mut cx = Cx {
            engine: self,
            session,
            artifacts: Vec::new(),
            checked: c.authority.clone(),
        };
        let result = cx.run(capability, input);
        out.artifacts = cx.artifacts;
        out.authority_checked = cx.checked;
        match result {
            Ok(v) => {
                let problems = schema::validate(&c.output_schema, &v);
                if !problems.is_empty() {
                    out.error = Some(err(
                        "internal",
                        "the result does not match the declared output schema",
                        json!(problems),
                    ));
                    return out;
                }
                out.result_digest = Some(canon::sha256_id(canon::canonical(&v).as_bytes()));
                out.result = Some(v);
                out.ok = true;
            }
            Err(e) => out.error = Some(e),
        }
        out
    }
}

struct Cx<'a> {
    engine: &'a Engine,
    session: &'a Session,
    artifacts: Vec<String>,
    checked: Vec<String>,
}

fn str_of<'v>(v: &'v Value, k: &str) -> Option<&'v str> {
    v.get(k).and_then(Value::as_str)
}

/// Standard base64 (with or without padding); `None` on invalid input.
pub fn unb64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl Cx<'_> {
    fn check_fs(&mut self, path: &str) -> Result<PathBuf, Error> {
        if !self.checked.iter().any(|s| s == "fs:read") {
            self.checked.push("fs:read".into());
        }
        check_path(self.session, path)
    }

    fn load(&mut self, r: &str) -> Result<(Package, Artifact), Error> {
        let p = match store::parse_id(r) {
            Some(id) => self
                .engine
                .store
                .get(&id)
                .map_err(|e| err("not_found", e, Value::Null))?,
            None => {
                let path = self.check_fs(r)?;
                Package::read_dir(&path).map_err(|e| err("invalid_input", e, Value::Null))?
            }
        };
        let a = p.artifact().map_err(|e| {
            err(
                "invalid_input",
                format!("not a readable artifact: {e}"),
                Value::Null,
            )
        })?;
        if !self.artifacts.contains(&a.id) {
            self.artifacts.push(a.id.clone());
        }
        Ok((p, a))
    }

    /// Verifies (with reproduction) and stores a new package.
    fn admit(&mut self, p: &Package) -> Result<Value, Error> {
        let v = verify::verify(p, Some(&Reproducers));
        let failed: Vec<String> = v
            .checks
            .iter()
            .filter(|c| c.status == verify::Status::Fail)
            .map(|c| c.name.clone())
            .collect();
        let skipped: Vec<String> = v
            .checks
            .iter()
            .filter(|c| c.status == verify::Status::Skip)
            .map(|c| c.name.clone())
            .collect();
        if !v.ok {
            return Err(err(
                "verification_failed",
                format!("failed: {}", failed.join(", ")),
                serde_json::to_value(&v).unwrap(),
            ));
        }
        let id = self
            .engine
            .store
            .put(p)
            .map_err(|e| err("verification_failed", e, Value::Null))?;
        let a = p.artifact().map_err(|e| err("internal", e, Value::Null))?;
        self.artifacts.push(id.clone());
        Ok(json!({
            "artifact": store::uri(&id),
            "id": id,
            "subject": {"type": a.subject.kind, "title": a.subject.title, "reference": a.subject.reference},
            "derivedFrom": a.provenance.derived_from,
            "verification": {"ok": true, "failed": failed, "skipped": skipped},
        }))
    }

    fn run(&mut self, capability: &str, i: &Value) -> Result<Value, Error> {
        match capability {
            "representation.capture" => self.capture(i),
            "representation.ingest" => {
                let path = self.check_fs(str_of(i, "path").unwrap_or(""))?;
                let p =
                    Package::read_dir(&path).map_err(|e| err("invalid_input", e, Value::Null))?;
                self.admit(&p)
            }
            "representation.inspect" => {
                let (p, a) = self.load(str_of(i, "artifact").unwrap_or(""))?;
                Ok(json!({"artifact": store::uri(&a.id), "document": p.document}))
            }
            "representation.understand" => {
                let (p, a) = self.load(str_of(i, "artifact").unwrap_or(""))?;
                let mut fields = i.clone();
                let form = match str_of(i, "format") {
                    Some("json") => project::Form::Json,
                    _ => project::Form::Text,
                };
                if let Value::Object(m) = &mut fields {
                    m.remove("artifact");
                    m.remove("format");
                }
                let req: project::Request = serde_json::from_value(fields)
                    .map_err(|e| err("invalid_input", e.to_string(), Value::Null))?;
                let (body, delivery) = project::render_with_report(&a, &req, form)
                    .map_err(|e| err("invalid_input", e, Value::Null))?;
                let projection = match form {
                    project::Form::Json => serde_json::from_str(&body)
                        .map_err(|e| err("internal", e.to_string(), Value::Null))?,
                    project::Form::Text => Value::String(body),
                };
                let mut out = json!({"artifact": store::uri(&a.id), "format": if form == project::Form::Json { "json" } else { "text" }, "projection": projection, "delivery": delivery});
                if req.consumer == project::Consumer::Vision {
                    let images: Vec<Value> = a
                        .views
                        .iter()
                        .filter_map(|v| p.asset_bytes(&v.asset).map(|b| json!({"view": v.id, "mediaType": v.media_type, "data": b64(b)})))
                        .collect();
                    out["images"] = Value::Array(images);
                }
                Ok(out)
            }
            "representation.render" => {
                let (p, a) = self.load(str_of(i, "artifact").unwrap_or(""))?;
                let id = str_of(i, "view").unwrap_or("v0");
                let v = a.views.iter().find(|v| v.id == id).ok_or_else(|| {
                    err(
                        "not_found",
                        format!("no view {id}"),
                        json!({"views": a.views.iter().map(|v| &v.id).collect::<Vec<_>>()}),
                    )
                })?;
                let bytes = p
                    .asset_bytes(&v.asset)
                    .ok_or_else(|| err("internal", "view asset missing", Value::Null))?;
                Ok(
                    json!({"artifact": store::uri(&a.id), "view": v, "mediaType": v.media_type, "data": b64(bytes)}),
                )
            }
            "representation.derive" => {
                let (p, a) = self.load(str_of(i, "artifact").unwrap_or(""))?;
                let cap = str_of(i, "capability").unwrap_or("");
                if a.provenance.adapter.name != x::ADAPTER {
                    return Err(err(
                        "unsupported",
                        format!(
                            "no local executor for adapter {}",
                            a.provenance.adapter.name
                        ),
                        Value::Null,
                    ));
                }
                let input = i.get("input").cloned().unwrap_or(json!({}));
                let child = x::invoke(&a, &p, cap, &input).map_err(|e| {
                    let code = if e.contains("authority") {
                        "unauthorized"
                    } else {
                        "invalid_input"
                    };
                    err(
                        code,
                        e,
                        json!({"note": "the provider never executes actions at a source"}),
                    )
                })?;
                self.admit(&child)
            }
            "representation.verify" => {
                let (p, _) = self.load(str_of(i, "artifact").unwrap_or(""))?;
                let reproduce = i.get("reproduce").and_then(Value::as_bool).unwrap_or(true);
                let v = verify::verify(&p, if reproduce { Some(&Reproducers) } else { None });
                Ok(serde_json::to_value(v).unwrap())
            }
            "representation.compare" => {
                let (_, a) = self.load(str_of(i, "a").unwrap_or(""))?;
                let (_, b) = self.load(str_of(i, "b").unwrap_or(""))?;
                Ok(serde_json::to_value(compare::compare(&a, &b)).unwrap())
            }
            "representation.retrieve" => {
                let mut list = self.engine.store.list();
                let text = str_of(i, "text").map(str::to_lowercase);
                list.retain(|e| {
                    str_of(i, "subjectType").is_none_or(|t| e.subject_type == t)
                        && str_of(i, "reference").is_none_or(|r| e.reference.as_deref() == Some(r))
                        && text
                            .as_deref()
                            .is_none_or(|t| e.title.to_lowercase().contains(t))
                        && str_of(i, "inputDigest").is_none_or(|d| e.inputs.iter().any(|x| x == d))
                });
                list.sort_by(|a, b| b.captured_at.cmp(&a.captured_at).then(a.id.cmp(&b.id)));
                let total = list.len();
                list.truncate(i.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize);
                Ok(json!({"artifacts": list, "total": total}))
            }
            "representation.annotate" => {
                let (p, _) = self.load(str_of(i, "artifact").unwrap_or(""))?;
                let ann: annotate::Annotation = serde_json::from_value(i["annotation"].clone())
                    .map_err(|e| err("invalid_input", e.to_string(), Value::Null))?;
                let child = annotate::annotate(&p, &ann)
                    .map_err(|e| err("invalid_input", e, Value::Null))?;
                self.admit(&child)
            }
            other => Err(err(
                "not_found",
                format!("no implementation for {other}"),
                Value::Null,
            )),
        }
    }

    fn capture(&mut self, i: &Value) -> Result<Value, Error> {
        let bytes = match (i.get("data"), str_of(i, "path"), str_of(i, "dataBase64")) {
            (Some(d), None, None) => canon::canonical(d).into_bytes(),
            (None, None, Some(b)) => unb64(b).ok_or_else(|| {
                err(
                    "invalid_input",
                    "dataBase64 is not valid base64",
                    json!([{"pointer": "/dataBase64"}]),
                )
            })?,
            (None, Some(path), None) => {
                let p = self.check_fs(path)?;
                std::fs::read(&p).map_err(|e| err("not_found", e.to_string(), Value::Null))?
            }
            _ => {
                return Err(err(
                    "invalid_input",
                    "give exactly one of data, dataBase64 or path",
                    json!([{"pointer": "/data"}, {"pointer": "/path"}]),
                ))
            }
        };
        let captured_at = str_of(i, "capturedAt")
            .map(str::to_string)
            .or_else(|| self.session.now.clone())
            .ok_or_else(|| {
                err(
                    "invalid_input",
                    "capturedAt is required: this host supplies no clock",
                    json!([{"pointer": "/capturedAt"}]),
                )
            })?;
        let o = i.get("options").cloned().unwrap_or(json!({}));
        let context_bytes = match (str_of(i, "source"), i.get("context")) {
            (Some("screenshot"), Some(c)) => canon::canonical(c).into_bytes(),
            (Some("screenshot"), None) => {
                return Err(err(
                    "invalid_input",
                    "a screenshot needs its capture context",
                    json!([{"pointer": "/context"}]),
                ))
            }
            _ => Vec::new(),
        };
        let theme = match str_of(&o, "theme") {
            Some("dark") => Theme::Dark,
            _ => Theme::Light,
        };
        let width = o.get("width").and_then(Value::as_f64);
        let scale = o.get("scale").and_then(Value::as_f64).unwrap_or(2.0) as f32;
        let method = str_of(i, "method").unwrap_or("caller-supplied").to_string();
        let package = match str_of(i, "source") {
            Some("x.post") => {
                let env = x::envelope(&bytes);
                let (post, url, id, extra) = match env {
                    Some(e) => (
                        e.post_json,
                        e.url,
                        e.id,
                        vec![(bytes.clone(), x::ENVELOPE_ROLE.to_string())],
                    ),
                    None => (bytes.clone(), None, None, vec![]),
                };
                let surface: Surface = match str_of(&o, "surface") {
                    Some(s) => serde_json::from_value(json!(s))
                        .map_err(|e| err("invalid_input", e.to_string(), Value::Null))?,
                    None => Surface::WebTimeline,
                };
                let options = RenderOptions {
                    surface,
                    width: width.unwrap_or(600.0) as f32,
                    theme,
                    dpr: scale,
                    ..RenderOptions::default()
                };
                x::capture(&x::Capture {
                    post_json: &post,
                    options,
                    captured_at,
                    method,
                    source_url: str_of(i, "reference").map(str::to_string).or(url),
                    source_id: str_of(i, "sourceId").map(str::to_string).or(id),
                    extra_inputs: extra,
                })
            }
            Some("image") => xeet_capture::image::capture(&xeet_capture::image::Capture {
                bytes: &bytes,
                captured_at,
                method,
                declared_name: str_of(i, "name").map(str::to_string),
                declared_type: str_of(i, "declaredType").map(str::to_string),
                context: None,
            }),
            Some("screenshot") => xeet_capture::image::capture(&xeet_capture::image::Capture {
                bytes: &bytes,
                captured_at,
                method,
                declared_name: str_of(i, "name").map(str::to_string),
                declared_type: str_of(i, "declaredType").map(str::to_string),
                context: Some(&context_bytes),
            }),
            Some("github.pull_request") => github::capture(&github::Capture {
                bundle_json: &bytes,
                width: width.unwrap_or(600.0) as f32,
                theme,
                scale,
                captured_at,
                method,
            }),
            other => {
                return Err(err(
                    "unsupported",
                    format!("no adapter for {other:?}"),
                    Value::Null,
                ))
            }
        }
        .map_err(|e| err("invalid_input", e, Value::Null))?;
        self.admit(&package)
    }
}
