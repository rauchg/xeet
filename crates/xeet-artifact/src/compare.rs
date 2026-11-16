//! Comparing two observations.
//!
//! Element ids are chosen by adapters to be stable for the same thing across
//! captures (`count.likes`, `file:src/lib.rs`), so a comparison can say what
//! changed between two observations of one subject without guessing.
//!
//! Visual equality is reported separately from semantic equality: identical
//! pixels do not prove identical meaning, and vice versa.

use crate::model::*;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    /// Same id: the same observation, byte for byte.
    pub identical: bool,
    pub same_subject: bool,
    pub captured_at: [String; 2],
    /// View id → whether both artifacts have that view with identical bytes.
    pub views: BTreeMap<String, ViewDiff>,
    pub elements_added: Vec<String>,
    pub elements_removed: Vec<String>,
    pub elements_changed: Vec<Change>,
    pub facts_changed: Vec<Change>,
    pub capabilities_added: Vec<String>,
    pub capabilities_removed: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ViewDiff {
    Identical,
    Different,
    OnlyInFirst,
    OnlyInSecond,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub id: String,
    pub field: String,
    pub before: Value,
    pub after: Value,
}

pub fn compare(a: &Artifact, b: &Artifact) -> Comparison {
    let mut views = BTreeMap::new();
    for v in &a.views {
        let diff = match b.views.iter().find(|w| w.id == v.id) {
            None => ViewDiff::OnlyInFirst,
            Some(w) if w.asset == v.asset => ViewDiff::Identical,
            Some(_) => ViewDiff::Different,
        };
        views.insert(v.id.clone(), diff);
    }
    for w in &b.views {
        views.entry(w.id.clone()).or_insert(ViewDiff::OnlyInSecond);
    }

    let ea: BTreeMap<&str, &Element> = a
        .semantic
        .elements
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();
    let eb: BTreeMap<&str, &Element> = b
        .semantic
        .elements
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();
    let mut changed = Vec::new();
    for (id, x) in &ea {
        if let Some(y) = eb.get(id) {
            let fields: [(&str, Value, Value); 5] = [
                ("kind", x.kind.clone().into(), y.kind.clone().into()),
                ("text", opt(&x.text), opt(&y.text)),
                (
                    "value",
                    x.value.clone().unwrap_or(Value::Null),
                    y.value.clone().unwrap_or(Value::Null),
                ),
                ("visible", x.visible.into(), y.visible.into()),
                (
                    "attrs",
                    serde_json::to_value(&x.attrs).unwrap(),
                    serde_json::to_value(&y.attrs).unwrap(),
                ),
            ];
            for (field, before, after) in fields {
                if before != after {
                    changed.push(Change {
                        id: id.to_string(),
                        field: field.to_string(),
                        before,
                        after,
                    });
                }
            }
        }
    }

    let fa: BTreeMap<(&str, &str), &Fact> = a
        .state
        .facts
        .iter()
        .map(|f| ((f.subject.as_str(), f.key.as_str()), f))
        .collect();
    let fb: BTreeMap<(&str, &str), &Fact> = b
        .state
        .facts
        .iter()
        .map(|f| ((f.subject.as_str(), f.key.as_str()), f))
        .collect();
    let keys: BTreeSet<_> = fa.keys().chain(fb.keys()).collect();
    let mut facts = Vec::new();
    for k in keys {
        let before = fa.get(k).map(|f| f.value.clone()).unwrap_or(Value::Null);
        let after = fb.get(k).map(|f| f.value.clone()).unwrap_or(Value::Null);
        if before != after {
            facts.push(Change {
                id: k.0.to_string(),
                field: k.1.to_string(),
                before,
                after,
            });
        }
    }

    let ca: BTreeSet<&str> = a.capabilities.iter().map(|c| c.id.as_str()).collect();
    let cb: BTreeSet<&str> = b.capabilities.iter().map(|c| c.id.as_str()).collect();

    Comparison {
        identical: a.id == b.id,
        same_subject: a.subject.kind == b.subject.kind
            && a.subject.reference.is_some()
            && a.subject.reference == b.subject.reference,
        captured_at: [
            a.provenance.capture.captured_at.clone(),
            b.provenance.capture.captured_at.clone(),
        ],
        views,
        elements_added: eb
            .keys()
            .filter(|k| !ea.contains_key(*k))
            .map(|k| k.to_string())
            .collect(),
        elements_removed: ea
            .keys()
            .filter(|k| !eb.contains_key(*k))
            .map(|k| k.to_string())
            .collect(),
        elements_changed: changed,
        facts_changed: facts,
        capabilities_added: cb.difference(&ca).map(|s| s.to_string()).collect(),
        capabilities_removed: ca.difference(&cb).map(|s| s.to_string()).collect(),
    }
}

fn opt(s: &Option<String>) -> Value {
    s.clone().map(Value::String).unwrap_or(Value::Null)
}
