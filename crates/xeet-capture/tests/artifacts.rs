//! The artifact contract, end to end: both adapters, through disk, through the
//! universal verifier, with reproduction — plus the failures it must catch.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use xeet_artifact::{
    compare, verify, AssetStore, Availability, Basis, CapabilityKind, ExecutorKind, Package,
};
use xeet_capture::{github, x, Reproducers};
use xeet_core::model::{RenderOptions, Theme};

const T0: &str = "2026-11-16T12:00:00Z";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn x_capture(json: &[u8], opts: RenderOptions) -> Package {
    x::capture(&x::Capture {
        post_json: json,
        options: opts,
        captured_at: T0.into(),
        method: "fixture".into(),
        source_url: None,
        source_id: None,
        extra_inputs: vec![],
    })
    .expect("capture")
}

fn pr_bundle() -> Vec<u8> {
    std::fs::read(root().join("tests/fixtures/github/pr-open-reviewed.json")).unwrap()
}

fn pr_capture(bundle: &[u8], at: &str) -> Package {
    github::capture(&github::Capture {
        bundle_json: bundle,
        width: 600.0,
        theme: Theme::Light,
        scale: 2.0,
        captured_at: at.into(),
        method: "fixture".into(),
    })
    .expect("capture")
}

/// Write → read from disk → verify with reproduction. Returns the re-read package.
fn through_disk(p: &Package, name: &str) -> Package {
    let dir =
        std::env::temp_dir().join(format!("xeet-artifact-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    p.write_dir(&dir).unwrap();
    let back = Package::read_dir(&dir).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    back
}

fn assert_verifies(p: &Package, what: &str) {
    let v = verify::verify(p, Some(&Reproducers));
    let failed: Vec<_> = v
        .checks
        .iter()
        .filter(|c| c.status != verify::Status::Pass && c.status != verify::Status::Warn)
        .collect();
    assert!(v.ok && failed.is_empty(), "{what}: {failed:#?}");
}

fn check<'a>(v: &'a verify::Verification, name: &str) -> &'a verify::Check {
    v.checks.iter().find(|c| c.name == name).unwrap()
}

#[test]
fn every_x_fixture_round_trips_and_verifies_with_reproduction() {
    let dir = root().join("tests/fixtures/posts");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let json = std::fs::read(&path).unwrap();
        for theme in [Theme::Light, Theme::Dark] {
            let opts = RenderOptions {
                width: 390.0,
                theme,
                ..RenderOptions::default()
            };
            let p = x_capture(&json, opts);
            let back = through_disk(&p, &format!("x-{n}"));
            assert_eq!(
                p.document, back.document,
                "{path:?}: document changed on disk"
            );
            assert_verifies(&back, &format!("{path:?}"));
            n += 1;
        }
    }
    assert!(
        n >= 14,
        "expected the 7 post fixtures in two themes, got {n}"
    );
}

#[test]
fn the_x_view_is_the_existing_renderers_bytes() {
    let json = std::fs::read(root().join("tests/fixtures/posts/iconic-pizzahut.json")).unwrap();
    let opts = RenderOptions {
        width: 390.0,
        dpr: 3.0,
        ..RenderOptions::default()
    };
    let p = x_capture(&json, opts.clone());
    let a = p.artifact().unwrap();
    let post = serde_json::from_slice(&json).unwrap();
    let (png, report) = xeet_core::render::render(&post, &opts).unwrap();
    assert_eq!(p.asset_bytes(&a.views[0].asset).unwrap(), png.as_slice());
    assert_eq!(
        a.views[0].renderer.ruleset.as_deref(),
        Some(report.ruleset.as_str())
    );
    // The body's bounds are the frame's own text box.
    let boxes = xeet_core::render::frame_boxes(&post, &opts);
    let body = a.semantic.elements.iter().find(|e| e.id == "body").unwrap();
    assert_eq!(body.bounds[0].y, (boxes["text"].y * 64.0).round() / 64.0);
}

#[test]
fn capture_is_deterministic() {
    let json = std::fs::read(root().join("tests/fixtures/posts/media-four.json")).unwrap();
    let a = x_capture(&json, RenderOptions::default());
    let b = x_capture(&json, RenderOptions::default());
    assert_eq!(a.document["id"], b.document["id"]);
    assert_eq!(
        pr_capture(&pr_bundle(), T0).document["id"],
        pr_capture(&pr_bundle(), T0).document["id"]
    );
}

#[test]
fn a_github_pull_request_satisfies_the_same_contract() {
    let p = through_disk(&pr_capture(&pr_bundle(), T0), "pr");
    assert_verifies(&p, "pr");
    let a = p.artifact().unwrap();
    assert_eq!(a.subject.kind, "github.pull_request");

    let el = |id: &str| {
        a.semantic
            .elements
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("{id}"))
    };
    // Painted elements point at pixels; tokens inside a sentence do not.
    for id in [
        "pr.title",
        "pr.state",
        "label:ci",
        "stat.additions",
        "pr.body",
        "reviews",
        "file:docs/CI.md",
    ] {
        assert_eq!(el(id).bounds.len(), 1, "{id} has bounds");
    }
    assert!(el("pr.author").bounds.is_empty());
    assert_eq!(el("pr.state").text.as_deref(), Some("Open"));

    // State is grounded and classified.
    let fact = |k: &str| {
        a.state
            .facts
            .iter()
            .find(|f| f.subject == "pr" && f.key == k)
            .unwrap()
    };
    assert_eq!(fact("reviewDecision").value, json!("approved"));
    assert_eq!(fact("reviewDecision").basis, Basis::Derived);
    assert_eq!(
        fact("mergeable").basis,
        Basis::Unknown,
        "null mergeable stays unknown"
    );
    assert!(fact("state").volatile);
    assert_eq!(a.state.as_of, T0);

    // Merging is described, never granted.
    let merge = a.capabilities.iter().find(|c| c.id == "merge").unwrap();
    assert_eq!(merge.kind, CapabilityKind::Mutation);
    assert_eq!(merge.executor.kind, ExecutorKind::Origin);
    assert_eq!(merge.availability, Availability::Delegated);
    assert!(!merge.authority.required.is_empty());
    assert_eq!(
        merge.input_schema["properties"]["sha"]["const"],
        json!("71079ff11ab2847c1c2e5973b3901b2c7203c89d")
    );
}

#[test]
fn both_sources_share_one_contract() {
    let post = std::fs::read(root().join("tests/fixtures/posts/links-card.json")).unwrap();
    for p in [
        x_capture(&post, RenderOptions::default()),
        pr_capture(&pr_bundle(), T0),
    ] {
        let a = p.artifact().unwrap();
        for aspect in xeet_artifact::COVERAGE_ASPECTS {
            assert!(a.coverage.contains_key(*aspect), "{aspect}");
        }
        for e in &a.semantic.elements {
            assert!(
                xeet_artifact::ELEMENT_KINDS.contains(&e.kind.as_str()),
                "{}: {}",
                e.id,
                e.kind
            );
        }
        assert!(!a.provenance.inputs.is_empty());
    }
}

#[test]
fn tampering_is_detected() {
    let p = pr_capture(&pr_bundle(), T0);
    let a = p.artifact().unwrap();

    // A changed pixel.
    let mut bad = p.clone();
    let path = a
        .assets
        .iter()
        .find(|x| x.media_type == "image/png")
        .unwrap()
        .path
        .clone();
    let bytes = bad.files.get_mut(&path).unwrap();
    let last = bytes.len() - 20;
    bytes[last] ^= 1;
    let v = verify::verify(&bad, None);
    assert!(!v.ok);
    assert_eq!(check(&v, "assets").status, verify::Status::Fail);

    // A changed word, id left alone.
    let mut bad = p.clone();
    bad.document["semantic"]["elements"][2]["text"] = json!("Something else entirely");
    let v = verify::verify(&bad, None);
    assert_eq!(check(&v, "identity").status, verify::Status::Fail);

    // A changed source input: integrity holds if re-sealed, reproduction does not.
    let mut a2 = a.clone();
    let mut store = AssetStore::default();
    let mut bundle: Value = serde_json::from_slice(&pr_bundle()).unwrap();
    bundle["pull"]["title"] = json!("A different title");
    let input = store.add(serde_json::to_vec(&bundle).unwrap(), "application/json");
    a2.provenance.inputs[0].asset = input;
    for asset in &a.assets {
        if asset.media_type == "image/png" {
            store.add(p.files[&asset.path].clone(), &asset.media_type);
        }
    }
    let resealed = store.seal(a2);
    let v = verify::verify(&resealed, Some(&Reproducers));
    assert_eq!(check(&v, "identity").status, verify::Status::Pass);
    assert_eq!(
        check(&v, "reproduction").status,
        verify::Status::Fail,
        "{v:#?}"
    );
}

#[test]
fn an_artifact_cannot_grant_authority() {
    let p = pr_capture(&pr_bundle(), T0);
    let mut a = p.artifact().unwrap();
    let merge = a.capabilities.iter_mut().find(|c| c.id == "merge").unwrap();
    merge.availability = Availability::LocalExecutable;
    merge.executor.kind = ExecutorKind::Local;
    let mut store = AssetStore::default();
    for asset in &a.assets {
        store.add(p.files[&asset.path].clone(), &asset.media_type);
    }
    let v = verify::verify(&store.seal(a), None);
    assert_eq!(check(&v, "capabilities").status, verify::Status::Fail);
}

#[test]
fn undeclared_inference_is_rejected() {
    let p = pr_capture(&pr_bundle(), T0);
    let mut a = p.artifact().unwrap();
    a.semantic.elements[1].basis = Basis::Inferred;
    let mut store = AssetStore::default();
    for asset in &a.assets {
        store.add(p.files[&asset.path].clone(), &asset.media_type);
    }
    let v = verify::verify(&store.seal(a), None);
    assert_eq!(check(&v, "basis").status, verify::Status::Fail);
}

#[test]
fn local_projections_run_and_remote_actions_are_refused() {
    let long = json!({
        "text": "word ".repeat(120),
        "author": {"name": "A", "handle": "a"},
        "createdAt": "2026-11-16T10:00:00Z", "now": T0
    });
    let json = serde_json::to_vec(&long).unwrap();
    let p = x::capture(&x::Capture {
        post_json: &json,
        options: RenderOptions {
            width: 390.0,
            ..RenderOptions::default()
        },
        captured_at: T0.into(),
        method: "fixture".into(),
        source_url: Some("https://x.com/a/status/1".into()),
        source_id: Some("1".into()),
        extra_inputs: vec![],
    })
    .unwrap();
    let a = p.artifact().unwrap();
    assert!(
        a.capabilities.iter().any(|c| c.id == "expand"),
        "a long post offers expand"
    );

    let child = x::project(&a, &p, "expand", &json!({})).unwrap();
    assert_verifies(&child, "expanded");
    let c = child.artifact().unwrap();
    assert_eq!(c.provenance.derived_from.as_deref(), Some(a.id.as_str()));
    assert!(c.views[0].layout_height > a.views[0].layout_height);
    assert!(!c.capabilities.iter().any(|c| c.id == "expand"));

    let err = x::project(&a, &p, "like", &json!({})).unwrap_err();
    assert!(err.contains("authority"), "{err}");
}

#[test]
fn comparison_reports_changed_state_not_just_pixels() {
    let before = pr_capture(&pr_bundle(), T0).artifact().unwrap();
    let mut bundle: Value = serde_json::from_slice(&pr_bundle()).unwrap();
    bundle["pull"]["state"] = json!("closed");
    bundle["pull"]["merged"] = json!(true);
    bundle["pull"]["merged_at"] = json!("2026-11-16T13:00:00Z");
    let after = pr_capture(
        &serde_json::to_vec(&bundle).unwrap(),
        "2026-11-16T14:00:00Z",
    )
    .artifact()
    .unwrap();
    let c = compare::compare(&before, &after);
    assert!(c.same_subject && !c.identical);
    let changed: Vec<_> = c.facts_changed.iter().map(|f| f.field.as_str()).collect();
    assert!(
        changed.contains(&"merged") && changed.contains(&"state"),
        "{changed:?}"
    );
    assert!(
        c.capabilities_removed.contains(&"merge".to_string()),
        "a merged PR no longer offers merge"
    );
    assert_eq!(c.views["v0"], compare::ViewDiff::Different);
}

/// The committed examples in `docs/artifacts/` are the format's compatibility
/// fixtures. Integrity must hold forever: a reader change that cannot verify
/// them breaks every artifact already written. Reproduction is a golden-style
/// check: it fails when the renderer's output changes, which is the moment to
/// re-capture the examples deliberately (and say why in the commit).
#[test]
fn committed_examples_stay_readable_and_reproducible() {
    for name in ["x-post", "github-pr"] {
        let p = Package::read_dir(&root().join("docs/artifacts").join(name)).unwrap();
        let v = verify::verify(&p, None);
        assert!(v.ok, "{name}: integrity: {v:#?}");
        let v = verify::verify(&p, Some(&Reproducers));
        assert_eq!(
            check(&v, "reproduction").status,
            verify::Status::Pass,
            "{name}: the renderer's output changed; re-capture docs/artifacts deliberately: {v:#?}"
        );
    }
}
