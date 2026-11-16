//! The model-request boundary: a provider-neutral request, the client trait a
//! runtime implements for its model provider, and error classification.
//!
//! The gateway never talks to a model itself. A runtime (or `openai.rs`, or a
//! test harness) implements [`ModelClient`]; the gateway builds the request,
//! hands it over, and classifies what comes back.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Content {
    Text {
        text: String,
    },
    /// Binary media sent as-is (an image to a vision model).
    Media {
        media_type: String,
        data_base64: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequest {
    pub provider: String,
    pub model: String,
    pub messages: Vec<Message>,
}

impl ModelRequest {
    /// Media types sent as binary in this request.
    pub fn media_types(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|c| match c {
                Content::Media { media_type, .. } => Some(media_type.clone()),
                Content::Text { .. } => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelResponse {
    pub text: String,
}

/// What a model provider said when it refused a request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderError {
    pub status: Option<u16>,
    pub body: String,
    /// The request never got an HTTP answer (DNS, TLS, timeout).
    pub transport: bool,
}

pub trait ModelClient {
    fn send(&mut self, request: &ModelRequest) -> Result<ModelResponse, ProviderError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    UnsupportedModality,
    Authentication,
    Quota,
    RateLimit,
    ContextOverflow,
    InvalidRequest,
    Server,
    Transport,
    Unknown,
}

/// One classification rule: all given conditions must hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub kind: ErrorKind,
    /// Statuses this rule applies to; empty means any.
    #[serde(default)]
    pub statuses: Vec<u16>,
    /// Lowercase phrases, any of which must appear in the body; empty means
    /// none required.
    #[serde(default)]
    pub phrases: Vec<String>,
    /// Where the phrases come from, for audit.
    pub evidence: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub kind: ErrorKind,
    pub rule: Option<usize>,
    /// For modality errors: the family the body names (`image`, `audio`, …).
    pub modality_family: Option<String>,
    /// The phrase that matched, so a reviewer can see the evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
}

/// Ordered rules; the first match wins. A host can append or replace rules
/// for its providers. Modality is never inferred from a status code alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Classifier {
    pub rules: Vec<Rule>,
}

fn rule(kind: ErrorKind, statuses: &[u16], phrases: &[&str], evidence: &str) -> Rule {
    Rule {
        kind,
        statuses: statuses.to_vec(),
        phrases: phrases.iter().map(|p| p.to_string()).collect(),
        evidence: evidence.into(),
    }
}

impl Default for Classifier {
    fn default() -> Self {
        use ErrorKind::*;
        Classifier {
            rules: vec![
                rule(Authentication, &[401, 403], &[], "HTTP semantics"),
                rule(Authentication, &[], &["invalid api key", "invalid_api_key", "incorrect api key", "authentication"], "common provider error text"),
                // Recorded from api.openai.com in this repository's session:
                // HTTP 429, type insufficient_quota, code credit_balance_exhausted.
                rule(Quota, &[], &["insufficient_quota", "credit_balance_exhausted", "exceeded your current quota", "billing"], "tests/fixtures/gateway/openai-429-insufficient-quota.json"),
                rule(RateLimit, &[429], &[], "HTTP semantics"),
                // Context overflow is checked before modality: both are 400s.
                rule(ContextOverflow, &[], &["context_length_exceeded", "maximum context length", "context window", "prompt is too long", "too many tokens"], "common provider error text"),
                // Observed live through Vercel AI Gateway (fixtures named below).
                rule(
                    UnsupportedModality,
                    &[400],
                    &["does not accept image input", "does not accept audio input", "does not accept video input", "doesn't support the image content block", "doesn't support the audio content block", "doesn't support the video content block", "doesn't support the document content block"],
                    "tests/fixtures/gateway/ai-gateway-400-deepseek-v3.1-image.json, ai-gateway-400-nova-micro-image.json",
                ),
                rule(
                    UnsupportedModality,
                    &[400, 415, 422],
                    &[
                        "image_url is only supported by certain models",
                        "does not support image",
                        "image input is not supported",
                        "images are not supported",
                        "does not support audio",
                        "audio input is not supported",
                        "does not support pdf",
                        "does not support document",
                        "unsupported modality",
                        "unsupported content type",
                    ],
                    "public provider error texts; not observed live in this repository (no model credits)",
                ),
                rule(InvalidRequest, &[400, 404, 409, 413, 422], &[], "HTTP semantics"),
                rule(Server, &[500, 502, 503, 504, 529], &[], "HTTP semantics"),
            ],
        }
    }
}

impl Classifier {
    pub fn classify(&self, e: &ProviderError) -> Classification {
        let none = |kind| Classification {
            kind,
            rule: None,
            modality_family: None,
            matched: None,
        };
        if e.transport {
            return none(ErrorKind::Transport);
        }
        let body = e.body.to_lowercase();
        for (i, r) in self.rules.iter().enumerate() {
            let status_ok =
                r.statuses.is_empty() || e.status.is_some_and(|s| r.statuses.contains(&s));
            let hit = r
                .phrases
                .iter()
                .find(|p| body.contains(p.as_str()))
                .cloned();
            let phrase_ok = r.phrases.is_empty() || hit.is_some();
            if status_ok && phrase_ok && !(r.statuses.is_empty() && r.phrases.is_empty()) {
                // The family the matched phrase names; else the first the body names.
                let families = ["image", "audio", "video", "pdf", "document"];
                let family = (r.kind == ErrorKind::UnsupportedModality)
                    .then(|| {
                        let from = |t: &str| families.iter().find(|f| t.contains(*f)).copied();
                        hit.as_deref()
                            .and_then(from)
                            .or_else(|| from(&body))
                            .map(|f| match f {
                                "pdf" | "document" => "application".to_string(),
                                other => other.to_string(),
                            })
                    })
                    .flatten();
                return Classification {
                    kind: r.kind,
                    rule: Some(i),
                    modality_family: family,
                    matched: hit,
                };
            }
        }
        none(ErrorKind::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(status: u16, body: &str) -> ProviderError {
        ProviderError {
            status: Some(status),
            body: body.into(),
            transport: false,
        }
    }

    /// A recorded body, without the `_recorded` note added for humans: only
    /// what the provider sent is evidence.
    fn fixture(name: &str) -> String {
        let text = std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/gateway/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        if let Some(m) = v.as_object_mut() {
            m.remove("_recorded");
        }
        v.to_string()
    }

    #[test]
    fn real_rejections_recorded_live_are_classified_correctly() {
        let c = Classifier::default();
        for (name, status, kind, family) in [
            (
                "ai-gateway-400-deepseek-v3.1-image.json",
                400,
                ErrorKind::UnsupportedModality,
                Some("image"),
            ),
            (
                "ai-gateway-400-nova-micro-image.json",
                400,
                ErrorKind::UnsupportedModality,
                Some("image"),
            ),
            // The top-level message alone ("Bad Request") names no modality: no fallback.
            (
                "ai-gateway-400-llama-3.1-8b-image.json",
                400,
                ErrorKind::InvalidRequest,
                None,
            ),
            // The full body carries the gateway's routed provider attempts; one says
            // images are unsupported. That is real evidence, and it is what the run saw.
            (
                "ai-gateway-400-llama-3.1-8b-image-full.json",
                400,
                ErrorKind::UnsupportedModality,
                Some("image"),
            ),
            (
                "ai-gateway-403-free-tier.json",
                403,
                ErrorKind::Authentication,
                None,
            ),
            (
                "openai-429-insufficient-quota.json",
                429,
                ErrorKind::Quota,
                None,
            ),
        ] {
            let got = c.classify(&e(status, &fixture(name)));
            assert_eq!(
                (got.kind, got.modality_family.as_deref()),
                (kind, family),
                "{name}"
            );
        }
    }

    #[test]
    fn only_modality_errors_are_modality_errors() {
        let c = Classifier::default();
        let real_429 = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/gateway/openai-429-insufficient-quota.json"
        ))
        .unwrap();
        assert_eq!(c.classify(&e(429, &real_429)).kind, ErrorKind::Quota);
        assert_eq!(
            c.classify(&e(429, "{\"error\":{\"message\":\"Rate limit reached\"}}"))
                .kind,
            ErrorKind::RateLimit
        );
        assert_eq!(c.classify(&e(401, "{}")).kind, ErrorKind::Authentication);
        let m = c.classify(&e(
            400,
            "Invalid content type. image_url is only supported by certain models.",
        ));
        assert_eq!(
            (m.kind, m.modality_family.as_deref()),
            (ErrorKind::UnsupportedModality, Some("image"))
        );
        // A 400 that mentions images but is a context overflow is not a modality error.
        assert_eq!(
            c.classify(&e(
                400,
                "This model's maximum context length is 8192 tokens; your image and text..."
            ))
            .kind,
            ErrorKind::ContextOverflow
        );
        // Status alone never means modality.
        assert_eq!(
            c.classify(&e(415, "unsupported media")).kind,
            ErrorKind::Unknown
        );
        assert_eq!(
            c.classify(&e(400, "missing field messages")).kind,
            ErrorKind::InvalidRequest
        );
        assert_eq!(c.classify(&e(503, "overloaded")).kind, ErrorKind::Server);
        assert_eq!(
            c.classify(&ProviderError {
                status: None,
                body: String::new(),
                transport: true
            })
            .kind,
            ErrorKind::Transport
        );
    }
}
