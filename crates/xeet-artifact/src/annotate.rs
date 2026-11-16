//! Adding what a model read from the pixels, without letting it pass as fact.
//!
//! When content exists only as pixels (a chart with no data, an image with no
//! alt text), a vision model or OCR can read it. This is the one way that
//! reading enters the format, and it enforces the distinction structurally:
//!
//! - every added element and fact must have basis `inferred`;
//! - each is declared in `limitations.inferred` with who read it and how;
//! - the result is a **new** artifact, `derivedFrom` the original, which is
//!   left untouched; the step is recorded as non-deterministic, with its time;
//! - views and inputs are carried over byte for byte, so reproduction still
//!   proves the pixels and the source are unchanged.
//!
//! An annotation can add; it cannot edit or remove anything observed.

use crate::model::*;
use crate::{AssetStore, Package};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Annotation {
    /// The element the new elements hang under (e.g. the chart).
    pub parent: String,
    /// New elements. Ids must be new; `children` may refer to each other.
    pub elements: Vec<Element>,
    /// New facts, about new or existing elements.
    pub facts: Vec<Fact>,
    /// The model, OCR engine or person that produced the reading.
    pub by: Tool,
    /// How: e.g. "read from view v0 by a vision model, prompt sha256:…".
    pub method: String,
    /// When the reading was made (ISO 8601). Not the observation time.
    pub at: String,
}

pub fn annotate(package: &Package, ann: &Annotation) -> Result<Package, String> {
    let mut a = package.artifact()?;
    if ann.by.name.is_empty() || ann.method.is_empty() || ann.at.is_empty() {
        return Err("an annotation must say who made it (by), how (method) and when (at)".into());
    }
    let existing: BTreeSet<String> = a.semantic.elements.iter().map(|e| e.id.clone()).collect();
    if !existing.contains(&ann.parent) {
        return Err(format!("parent {} is not an element", ann.parent));
    }
    let new_ids: BTreeSet<String> = ann.elements.iter().map(|e| e.id.clone()).collect();
    if new_ids.len() != ann.elements.len() {
        return Err("annotation element ids repeat".into());
    }
    let mut referenced = BTreeSet::new();
    for e in &ann.elements {
        if existing.contains(&e.id) {
            return Err(format!(
                "{}: already an element; annotations add, they do not edit",
                e.id
            ));
        }
        if e.basis != Basis::Inferred {
            return Err(format!(
                "{}: basis is {:?}; annotations can only add inferred content",
                e.id, e.basis
            ));
        }
        for c in &e.children {
            if !new_ids.contains(c) {
                return Err(format!(
                    "{}: child {c} is not part of this annotation",
                    e.id
                ));
            }
            referenced.insert(c.clone());
        }
    }
    for f in &ann.facts {
        if f.basis != Basis::Inferred || f.value.is_null() {
            return Err(format!(
                "fact {}.{}: annotations can only add inferred, non-null facts",
                f.subject, f.key
            ));
        }
        if !existing.contains(&f.subject) && !new_ids.contains(&f.subject) {
            return Err(format!(
                "fact {}.{}: subject does not exist",
                f.subject, f.key
            ));
        }
        if a.state
            .facts
            .iter()
            .any(|x| x.subject == f.subject && x.key == f.key)
        {
            return Err(format!(
                "fact {}.{}: already observed; annotations do not override observations",
                f.subject, f.key
            ));
        }
    }

    let why = format!(
        "read by {} {} ({}) at {}",
        ann.by.name, ann.by.version, ann.method, ann.at
    );
    let parent = a
        .semantic
        .elements
        .iter_mut()
        .find(|e| e.id == ann.parent)
        .expect("checked");
    for e in &ann.elements {
        if !referenced.contains(&e.id) {
            parent.children.push(e.id.clone());
        }
    }
    for e in &ann.elements {
        a.limitations.inferred.push(Note::new(
            Some(&e.id),
            "content read from pixels, not supplied by the source",
            why.clone(),
        ));
        // Bounds are quantized to 1/64 layout unit, as every producer's are,
        // so they survive any JSON round trip and identity stays stable.
        let mut e = e.clone();
        for b in &mut e.bounds {
            for v in [&mut b.x, &mut b.y, &mut b.w, &mut b.h] {
                *v = (*v * 64.0).round() / 64.0;
            }
        }
        a.semantic.elements.push(e);
    }
    for f in &ann.facts {
        a.limitations.inferred.push(Note::new(
            Some(&f.subject),
            format!("fact {} read from pixels", f.key),
            why.clone(),
        ));
        a.state.facts.push(f.clone());
    }
    a.provenance.transformations.push(Transformation {
        step: "annotate (inferred content)".into(),
        tool: ann.by.clone(),
        inputs: a.views.iter().map(|v| v.asset.clone()).collect(),
        outputs: ann.elements.iter().map(|e| e.id.clone()).collect(),
        deterministic: false,
        at: Some(ann.at.clone()),
    });
    a.provenance.derived_from = Some(a.id.clone());
    if a.coverage.get("semantic") == Some(&Support::Supported) {
        a.coverage.insert("semantic".into(), Support::Partial);
    }

    let mut store = AssetStore::default();
    for asset in &a.assets {
        let bytes = package
            .files
            .get(&asset.path)
            .ok_or_else(|| format!("{} missing", asset.path))?;
        store.add(bytes.clone(), &asset.media_type);
    }
    Ok(store.seal(a))
}
