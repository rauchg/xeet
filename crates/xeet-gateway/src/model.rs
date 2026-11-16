//! Model input capabilities: what a target model accepts, and on what basis
//! anyone believes it.
//!
//! The host owns the authoritative profile. Provider metadata, documentation
//! and observed rejections feed it, but nothing here is inferred from a model
//! name, a provider, or the fact that an API accepts a generic request shape.
//! There is no built-in table of vendor models on purpose.

use serde::{Deserialize, Serialize};

pub const PROFILE_FORMAT: &str = "model-input-capabilities";
pub const PROFILE_VERSION: &str = "0.1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Support {
    Supported,
    Unsupported,
    /// Supported within stated limits (`maxBytes`, `maxWidth`, …) or under a
    /// stated condition.
    Conditional,
    Unknown,
}

/// Where a capability statement comes from. Ordered from weakest to strongest
/// evidence for display; the host's configuration wins regardless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InfoSource {
    /// Documentation or a model card says so.
    Declared,
    /// The provider's API reported it.
    ProviderReported,
    /// The host configured it (authoritative for this deployment).
    Configured,
    /// Tested against the live model.
    Verified,
    /// The model rejected this input at runtime (recorded by the gateway).
    ObservedRejection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSupport {
    /// An exact media type (`image/png`), a family (`image/*`) or `*/*`.
    pub media_type: String,
    pub support: Support,
    pub source: InfoSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    pub format: String,
    pub version: String,
    /// Model provider identity (`openai`, `local-llama`, …).
    pub provider: String,
    pub model: String,
    /// Revision of this profile, for audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub inputs: Vec<InputSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Characters per token for size estimates. An estimate, configured by
    /// the host; the gateway never tokenizes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars_per_token: Option<f64>,
    pub structured_input: Support,
    pub source: InfoSource,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// The gateway's decision for one input against one profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "decision")]
pub enum Acceptance {
    Accept {
        rule: String,
        source: InfoSource,
    },
    Reject {
        rule: String,
        source: InfoSource,
        reason: String,
    },
    /// Nothing states whether the model accepts this.
    Unknown {
        reason: String,
    },
}

fn matches(pattern: &str, media_type: &str) -> u8 {
    if pattern == media_type {
        3
    } else if let Some(family) = pattern.strip_suffix("/*") {
        if family == "*" {
            1
        } else if media_type.split('/').next() == Some(family) {
            2
        } else {
            0
        }
    } else {
        0
    }
}

impl ModelProfile {
    /// Problems that make a profile unusable. An empty list means valid.
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.format != PROFILE_FORMAT {
            out.push(format!(
                "format is {:?}, not {PROFILE_FORMAT:?}",
                self.format
            ));
        }
        if self.version.split('.').next() != PROFILE_VERSION.split('.').next() {
            out.push(format!(
                "version {:?} is not readable as {PROFILE_VERSION}",
                self.version
            ));
        }
        if self.provider.is_empty() || self.model.is_empty() {
            out.push("provider and model are required".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for i in &self.inputs {
            if !seen.insert(&i.media_type) {
                out.push(format!("{} is listed twice", i.media_type));
            }
            if !i.media_type.contains('/') {
                out.push(format!("{} is not a media type", i.media_type));
            }
            if i.support == Support::Conditional
                && i.condition.is_none()
                && i.max_bytes.is_none()
                && i.max_width.is_none()
                && i.max_height.is_none()
            {
                out.push(format!(
                    "{}: conditional support must state its condition or limits",
                    i.media_type
                ));
            }
        }
        if let Some(c) = self.chars_per_token {
            if !(c.is_finite() && c > 0.0) {
                out.push("charsPerToken must be positive".into());
            }
        }
        out
    }

    /// The most specific rule for a media type: exact, then family, then `*/*`.
    pub fn rule_for(&self, media_type: &str) -> Option<&InputSupport> {
        self.inputs
            .iter()
            .filter(|i| matches(&i.media_type, media_type) > 0)
            .max_by_key(|i| matches(&i.media_type, media_type))
    }

    /// Whether this model accepts an input of this type, size and dimensions.
    pub fn accepts(&self, media_type: &str, bytes: u64, dims: Option<(u32, u32)>) -> Acceptance {
        let Some(r) = self.rule_for(media_type) else {
            return Acceptance::Unknown {
                reason: format!("no rule for {media_type} in the profile"),
            };
        };
        let rule = r.media_type.clone();
        let limits = || -> Option<String> {
            if let Some(m) = r.max_bytes {
                if bytes > m {
                    return Some(format!("{bytes} bytes exceeds maxBytes {m}"));
                }
            }
            if let (Some((w, h)), mw, mh) = (dims, r.max_width, r.max_height) {
                if mw.is_some_and(|m| w > m) || mh.is_some_and(|m| h > m) {
                    return Some(format!(
                        "{w}x{h} exceeds {}x{}",
                        mw.map_or("-".into(), |v| v.to_string()),
                        mh.map_or("-".into(), |v| v.to_string())
                    ));
                }
            }
            None
        };
        match r.support {
            Support::Unsupported => Acceptance::Reject {
                rule,
                source: r.source,
                reason: format!("{} is unsupported", r.media_type),
            },
            Support::Unknown => Acceptance::Unknown {
                reason: format!("support for {} is unknown ({:?})", r.media_type, r.source),
            },
            Support::Supported | Support::Conditional => match limits() {
                Some(reason) => Acceptance::Reject {
                    rule,
                    source: r.source,
                    reason,
                },
                None if r.support == Support::Conditional && r.condition.is_some() => {
                    Acceptance::Unknown {
                        reason: format!(
                            "{} is conditional: {}",
                            r.media_type,
                            r.condition.as_deref().unwrap_or("")
                        ),
                    }
                }
                None => Acceptance::Accept {
                    rule,
                    source: r.source,
                },
            },
        }
    }

    /// Applies the host's configuration on top: its input rules replace rules
    /// for the same media type, and its scalar fields replace when present.
    pub fn with_host_config(mut self, host: &ModelProfile) -> ModelProfile {
        for h in &host.inputs {
            self.inputs.retain(|i| i.media_type != h.media_type);
            self.inputs.push(h.clone());
        }
        self.context_tokens = host.context_tokens.or(self.context_tokens);
        self.max_output_tokens = host.max_output_tokens.or(self.max_output_tokens);
        self.chars_per_token = host.chars_per_token.or(self.chars_per_token);
        if host.structured_input != Support::Unknown {
            self.structured_input = host.structured_input;
        }
        self.source = InfoSource::Configured;
        self.revision = host.revision.clone().or(self.revision);
        self.inputs.sort_by(|a, b| a.media_type.cmp(&b.media_type));
        self
    }

    /// Records that the model rejected a media type at runtime. The gateway
    /// applies this only within one run; persisting it is the host's decision.
    pub fn observe_rejection(&mut self, media_type: &str, evidence: &str) {
        self.inputs.retain(|i| i.media_type != media_type);
        self.inputs.push(InputSupport {
            media_type: media_type.into(),
            support: Support::Unsupported,
            source: InfoSource::ObservedRejection,
            max_bytes: None,
            max_width: None,
            max_height: None,
            condition: None,
            evidence: Some(evidence.into()),
        });
    }
}
