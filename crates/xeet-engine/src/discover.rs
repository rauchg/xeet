//! Discovery and negotiation over manifests.
//!
//! Discovery is a pure function of manifests: it cannot invoke anything and
//! knows nothing about sessions or grants. It answers "who can do this, in a
//! form I can consume?" by outcome, not by tool name, and returns summaries
//! first (progressive disclosure); full contracts come from [`describe`].
//!
//! Matching is deterministic keyword overlap with each contract's declared
//! outcomes, title and summary. It is not semantic search, and says so in the
//! manifest; an agent with a model can of course rank the summaries itself.

use crate::contract::{Contract, Manifest, Mode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Query {
    /// What the agent wants to achieve, in its own words.
    pub outcome: Option<String>,
    /// Modalities the consumer can take in (e.g. ["text/plain"] for a
    /// text-only model). Empty: anything.
    pub accepts: Vec<String>,
    /// Modalities the agent can supply as input. Empty: anything.
    pub has: Vec<String>,
    /// Only providers that support this source type.
    pub source: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Match {
    pub provider: String,
    pub provider_version: String,
    pub capability: String,
    pub version: String,
    pub title: String,
    pub summary: String,
    pub score: u32,
    pub authority: Vec<String>,
    pub effects: String,
    /// The first mode whose output the consumer accepts.
    pub mode: Mode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NotProvided {
    pub provider: String,
    pub matched: String,
    pub why: String,
    pub alternative: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub matches: Vec<Match>,
    /// Declared non-capabilities the outcome matched: an honest "no".
    pub not_provided: Vec<NotProvided>,
    /// Capabilities that matched the outcome but produce nothing the consumer
    /// accepts, with the modalities they would need.
    pub excluded_by_modality: Vec<(String, String, Vec<String>)>,
    pub note: String,
}

const STOP: &[&str] = &[
    "a", "an", "the", "to", "of", "for", "and", "or", "in", "on", "with", "without", "is", "it",
    "this", "that", "my", "i", "me", "be", "as", "by", "from", "into", "can", "what", "how", "do",
    "get", "want", "need", "some", "any", "are",
];

fn tokens(s: &str) -> BTreeSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1 && !STOP.contains(w))
        .map(|w| w.trim_end_matches('s').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

fn overlap(q: &BTreeSet<String>, text: &str) -> u32 {
    tokens(text).intersection(q).count() as u32
}

fn score(c: &Contract, q: &BTreeSet<String>) -> u32 {
    let outcomes = c.outcomes.iter().map(|o| overlap(q, o)).max().unwrap_or(0);
    outcomes * 3
        + overlap(q, &c.title) * 2
        + overlap(q, &c.summary)
        + overlap(q, &c.id.replace('.', " ")) * 2
}

fn mode_for<'a>(c: &'a Contract, accepts: &[String]) -> Option<&'a Mode> {
    if accepts.is_empty() {
        return c.modes.first();
    }
    c.modes
        .iter()
        .find(|m| m.output.iter().all(|o| accepts.contains(o)))
}

/// Searches one or more manifests. Deterministic: ties break by provider id,
/// then capability id.
pub fn discover(manifests: &[Manifest], q: &Query) -> Discovery {
    let qt = q.outcome.as_deref().map(tokens).unwrap_or_default();
    let mut matches = Vec::new();
    let mut excluded = Vec::new();
    let mut not_provided = Vec::new();
    for m in manifests {
        if let Some(src) = &q.source {
            if !m.sources.iter().any(|s| &s.kind == src) {
                continue;
            }
        }
        for c in &m.capabilities {
            let sc = if qt.is_empty() { 1 } else { score(c, &qt) };
            if sc == 0 {
                continue;
            }
            if !q.has.is_empty() && !c.input_modalities.iter().any(|i| q.has.contains(i)) {
                continue;
            }
            match mode_for(c, &q.accepts) {
                Some(mode) => matches.push(Match {
                    provider: m.provider.id.clone(),
                    provider_version: m.provider.version.clone(),
                    capability: c.id.clone(),
                    version: c.version.clone(),
                    title: c.title.clone(),
                    summary: c.summary.clone(),
                    score: sc,
                    authority: c.authority.clone(),
                    effects: c.effects.clone(),
                    mode: mode.clone(),
                }),
                None => excluded.push((
                    m.provider.id.clone(),
                    c.id.clone(),
                    c.modes
                        .iter()
                        .flat_map(|x| x.output.clone())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                )),
            }
        }
        if !qt.is_empty() {
            for n in &m.non_capabilities {
                if let Some(best) = n.outcomes.iter().max_by_key(|o| overlap(&qt, o)) {
                    let hit = overlap(&qt, best);
                    // A declared "no" counts when it covers most of the query.
                    if hit >= 2 && hit as usize * 2 >= qt.len() {
                        not_provided.push(NotProvided {
                            provider: m.provider.id.clone(),
                            matched: best.clone(),
                            why: n.why.clone(),
                            alternative: n.alternative.clone(),
                        });
                    }
                }
            }
        }
    }
    matches.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.provider.cmp(&b.provider))
            .then(a.capability.cmp(&b.capability))
    });
    if let Some(limit) = q.limit {
        matches.truncate(limit);
    }
    let note = if not_provided.is_empty() {
        "summaries only; call describe for a contract's schemas. Discovery grants no authority."
    } else {
        "the outcome matches something a provider declares it does NOT do (see notProvided); matches share words with the request but may not perform it. Summaries only; discovery grants no authority."
    };
    Discovery {
        matches,
        not_provided,
        excluded_by_modality: excluded,
        note: note.into(),
    }
}

/// Full contracts by id, from every manifest that has them.
pub fn describe(manifests: &[Manifest], ids: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in manifests {
        for c in &m.capabilities {
            if ids.contains(&c.id) {
                let mut v = serde_json::to_value(c).expect("contract serializes");
                v["provider"] = Value::String(m.provider.id.clone());
                v["providerVersion"] = Value::String(m.provider.version.clone());
                out.push(v);
            }
        }
    }
    out
}
