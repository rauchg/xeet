//! `xeet-capture`: source adapters that produce [`xeet_artifact`] packages.
//!
//! - [`x`] — X posts, rendered by xeet-core's X renderer (unchanged), with
//!   semantic bounds taken from the frame's Chrome-gated geometry.
//! - [`github`] — GitHub pull requests, from REST API responses supplied by the
//!   caller, rendered by the source-independent [`view`] renderer.
//!
//! Adapters are pure: the caller supplies the source bytes and the capture
//! time. Nothing here fetches, reads a clock, or calls a model, so a capture is
//! reproducible from its inputs — which is what [`Reproducers`] checks.

#![forbid(unsafe_code)]

pub mod github;
pub mod view;
pub mod x;

use std::collections::BTreeMap;

use serde_json::Value;
use xeet_artifact::{Artifact, Basis, Bounds, Element, Fact, Package, Reproducer, Tool};
use xeet_core::display::Rect;

/// This crate as a provenance tool.
pub fn tool(component: &str) -> Tool {
    Tool {
        name: format!("xeet-capture/{component}"),
        version: env!("CARGO_PKG_VERSION").to_string(),
        ruleset: None,
    }
}

pub(crate) fn el(id: &str, kind: &str, role: &str) -> Element {
    Element {
        id: id.to_string(),
        kind: kind.to_string(),
        role: role.to_string(),
        visible: true,
        basis: Basis::Source,
        ..Element::default()
    }
}

/// Geometry is quantized to 1/64 px — Blink's LayoutUnit, and the tolerance
/// the repository's Chrome parity gate uses — so every coordinate is an exact,
/// short decimal that survives any JSON round trip.
pub(crate) fn q(v: f32) -> f32 {
    (v * 64.0).round() / 64.0
}

pub(crate) fn at(view: &str, r: Rect) -> Bounds {
    Bounds {
        view: view.to_string(),
        x: q(r.x),
        y: q(r.y),
        w: q(r.w),
        h: q(r.h),
    }
}

pub(crate) fn fact(subject: &str, key: &str, value: Value, basis: Basis, volatile: bool) -> Fact {
    let basis = if value.is_null() {
        Basis::Unknown
    } else {
        basis
    };
    Fact {
        subject: subject.to_string(),
        key: key.to_string(),
        value,
        basis,
        source_time: None,
        volatile,
    }
}

/// Every reproducer this crate provides, for `verify`.
pub struct Reproducers;

impl Reproducer for Reproducers {
    fn handles(&self, adapter: &str) -> bool {
        adapter == x::ADAPTER || adapter == github::ADAPTER
    }

    fn reproduce(
        &self,
        artifact: &Artifact,
        package: &Package,
    ) -> Result<BTreeMap<String, Vec<u8>>, String> {
        let input = |role: &str| -> Result<&[u8], String> {
            let use_ = artifact
                .provenance
                .inputs
                .iter()
                .find(|i| i.role == role)
                .ok_or_else(|| format!("no {role} input preserved"))?;
            package
                .asset_bytes(&use_.asset)
                .ok_or_else(|| format!("input {} missing", use_.asset))
        };
        let mut out = BTreeMap::new();
        match artifact.provenance.adapter.name.as_str() {
            x::ADAPTER => {
                let post = input(x::INPUT_ROLE)?;
                for v in &artifact.views {
                    out.insert(v.id.clone(), x::render_view(post, &v.config)?);
                }
            }
            github::ADAPTER => {
                let bundle = input(github::INPUT_ROLE)?;
                for v in &artifact.views {
                    out.insert(v.id.clone(), github::render_view(bundle, &v.config)?);
                }
            }
            other => return Err(format!("unknown adapter {other}")),
        }
        Ok(out)
    }
}
