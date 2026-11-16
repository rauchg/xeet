//! The provider contract: manifest, discovery, authority, execution, outcomes.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use xeet_engine::contract::{self, modality, Manifest};
use xeet_engine::discover::{self, Query};
use xeet_engine::{schema, store::Store, Engine, Session};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn engine(name: &str) -> Engine {
    let dir = std::env::temp_dir().join(format!("xeet-engine-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Engine::new(Store::open(&dir).unwrap())
}

fn all() -> Session {
    Session {
        grants: ["store:read", "store:write", "fs:read"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        read_roots: vec![root().join("tests"), root().join("docs")],
        now: None,
    }
}

fn pr_input() -> Value {
    json!({"source": "github.pull_request", "path": root().join("tests/fixtures/github/pr-open-reviewed.json"),
           "capturedAt": "2026-11-16T12:00:00Z", "method": "fixture"})
}

fn peer() -> Manifest {
    serde_json::from_slice(
        &std::fs::read(root().join("tests/fixtures/engine/peer-vision.manifest.json")).unwrap(),
    )
    .unwrap()
}

#[test]
fn the_manifest_is_complete_consistent_and_committed() {
    let m = contract::manifest();
    let scopes: Vec<&str> = m.authority.scopes.iter().map(|s| s.id.as_str()).collect();
    let mut ids = std::collections::BTreeSet::new();
    for c in &m.capabilities {
        assert!(ids.insert(&c.id), "{} twice", c.id);
        assert!(
            !c.outcomes.is_empty() && !c.modes.is_empty() && !c.errors.is_empty(),
            "{}",
            c.id
        );
        assert!(
            c.authority.iter().all(|a| scopes.contains(&a.as_str())),
            "{}: undeclared scope",
            c.id
        );
        for s in [&c.input_schema, &c.output_schema] {
            assert_eq!(
                schema::unsupported_keywords(s),
                Vec::<String>::new(),
                "{}: schema uses keywords the validator ignores",
                c.id
            );
        }
        assert!(
            ["none", "creates-artifact"].contains(&c.effects.as_str()),
            "{}",
            c.id
        );
        assert_eq!(
            c.effects == "creates-artifact",
            c.authority.iter().any(|a| a == "store:write"),
            "{}: writes need store:write",
            c.id
        );
    }
    for required in [
        "capture",
        "ingest",
        "inspect",
        "understand",
        "render",
        "verify",
        "compare",
        "retrieve",
    ] {
        assert!(
            ids.contains(&format!("representation.{required}")),
            "{required}"
        );
    }
    assert_eq!(m.sources.len(), 4);
    // The committed manifest is what this build serves: a contract change is a reviewable diff.
    let committed: Value =
        serde_json::from_slice(&std::fs::read(root().join("docs/engine.manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        committed,
        serde_json::to_value(&m).unwrap(),
        "regenerate with `xeet engine manifest > docs/engine.manifest.json`"
    );
}

#[test]
fn a_text_only_model_finds_how_to_read_visual_information() {
    let d = discover::discover(
        &[contract::manifest()],
        &Query {
            outcome: Some("understand a screenshot without vision".into()),
            accepts: vec![modality::TEXT.into()],
            ..Query::default()
        },
    );
    let top = &d.matches[0];
    assert_eq!(top.capability, "representation.understand");
    assert_eq!(top.mode.output, [modality::TEXT]);
    assert_eq!(
        top.mode.select,
        json!({"consumer": "text", "format": "text"})
    );
    assert!(
        d.excluded_by_modality
            .iter()
            .any(|(_, c, out)| c == "representation.render" && out == &[modality::PNG]),
        "render only yields pixels"
    );
    // Progressive disclosure: summaries carry no schemas.
    let summary = serde_json::to_string(&d).unwrap();
    assert!(
        !summary.contains("inputSchema") && summary.len() < 6000,
        "{} bytes",
        summary.len()
    );
    let full = discover::describe(
        &[contract::manifest()],
        std::slice::from_ref(&top.capability),
    );
    assert!(full[0]["inputSchema"]["properties"]["consumer"].is_object());
    assert_eq!(full[0]["provider"], "xeet");
}

#[test]
fn discovery_says_no_honestly_and_finds_other_providers() {
    let ms = [contract::manifest(), peer()];
    let d = discover::discover(
        &ms[..1],
        &Query {
            outcome: Some("OCR text from a photo".into()),
            ..Query::default()
        },
    );
    assert_eq!(d.not_provided.len(), 1);
    assert!(d.not_provided[0]
        .alternative
        .contains("representation.annotate"));
    assert!(d.note.contains("does NOT do"));
    let d = discover::discover(
        &ms[..1],
        &Query {
            outcome: Some("merge a pull request".into()),
            ..Query::default()
        },
    );
    assert!(d
        .not_provided
        .iter()
        .any(|n| n.matched == "merge a pull request"));

    // With a peer: the peer's capability ranks first; xeet still says it does not do it.
    let d = discover::discover(
        &ms,
        &Query {
            outcome: Some("read text from a photo".into()),
            ..Query::default()
        },
    );
    assert_eq!(
        (
            d.matches[0].provider.as_str(),
            d.matches[0].capability.as_str()
        ),
        ("example-vision", "vision.read")
    );
    // The same contract id from two providers is listed twice, by provider.
    let d = discover::discover(
        &ms,
        &Query {
            outcome: Some("understand a screenshot without vision".into()),
            accepts: vec![modality::TEXT.into()],
            ..Query::default()
        },
    );
    let both: Vec<&str> = d
        .matches
        .iter()
        .filter(|m| m.capability == "representation.understand")
        .map(|m| m.provider.as_str())
        .collect();
    assert_eq!(both, ["xeet", "example-vision"]);
    // Source filter.
    let d = discover::discover(
        &ms,
        &Query {
            source: Some("github.pull_request".into()),
            ..Query::default()
        },
    );
    assert!(d.matches.iter().all(|m| m.provider == "xeet"));
}

#[test]
fn discovery_and_availability_grant_nothing() {
    let e = engine("authority");
    let none = Session::default();
    for c in &e.manifest().capabilities {
        let out = e.invoke(&c.id, &json!({}), &none);
        assert!(!out.ok, "{}", c.id);
        let code = &out.error.as_ref().unwrap().code;
        // Either the input is rejected first, or the missing scopes are named.
        assert!(
            code == "invalid_input" || code == "unauthorized",
            "{}: {code}",
            c.id
        );
    }
    let out = e.invoke(
        "representation.capture",
        &pr_input(),
        &Session::with(&["store:read"]),
    );
    let error = out.error.unwrap();
    assert_eq!(error.code, "unauthorized");
    assert_eq!(error.details["missing"], json!(["store:write"]));
    // fs:read is checked when a path is used, and only inside read roots.
    let out = e.invoke(
        "representation.capture",
        &pr_input(),
        &Session::with(&["store:write"]),
    );
    assert_eq!(out.error.unwrap().details["missing"], json!(["fs:read"]));
    let mut s = all();
    s.read_roots = vec![root().join("docs")];
    let out = e.invoke("representation.capture", &pr_input(), &s);
    assert!(out
        .error
        .unwrap()
        .message
        .contains("outside the read roots"));
}

#[test]
fn every_capability_runs_and_returns_a_verifiable_outcome() {
    let e = engine("flow");
    let s = all();
    let ok = |cap: &str, input: Value| {
        let out = e.invoke(cap, &input, &s);
        assert!(out.ok, "{cap}: {:?}", out.error);
        assert!(out
            .result_digest
            .as_deref()
            .is_some_and(|d| d.starts_with("sha256:")));
        out
    };
    // capture: from a path, and inline data
    let pr = ok("representation.capture", pr_input());
    let pr_id = pr.result.as_ref().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let again = ok("representation.capture", pr_input());
    assert_eq!(
        again.result_digest, pr.result_digest,
        "same input, same outcome"
    );
    assert_eq!(again.input_digest, pr.input_digest);
    let post: Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/fixtures/posts/links-card.json")).unwrap(),
    )
    .unwrap();
    let x = ok(
        "representation.capture",
        json!({"source": "x.post", "data": post, "capturedAt": "2026-11-16T12:00:00Z", "options": {"width": 390},
        "reference": "https://x.com/rauchg/status/1", "sourceId": "1"}),
    );
    let x_id = x.result.as_ref().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // understand: text, json, vision
    let t = ok(
        "representation.understand",
        json!({"artifact": pr_id, "maxChars": 3000}),
    );
    let text = t.result.as_ref().unwrap()["projection"].as_str().unwrap();
    assert!(text.chars().count() <= 3000 && text.contains("github.pull_request"));
    assert!(
        t.result.as_ref().unwrap().get("images").is_none(),
        "text consumers get no pixels"
    );
    let j = ok(
        "representation.understand",
        json!({"artifact": pr_id, "format": "json"}),
    );
    assert!(j.result.unwrap()["projection"]["elements"].is_array());
    let v = ok(
        "representation.understand",
        json!({"artifact": x_id, "consumer": "vision", "maxChars": 4000}),
    );
    assert_eq!(v.result.unwrap()["images"][0]["mediaType"], "image/png");

    ok("representation.inspect", json!({"artifact": pr_id}));
    let r = ok("representation.render", json!({"artifact": x_id}));
    assert!(r.result.unwrap()["data"]
        .as_str()
        .unwrap()
        .starts_with("iVBOR"));
    let vr = ok("representation.verify", json!({"artifact": pr_id}));
    assert_eq!(vr.result.unwrap()["ok"], true);

    // derive: a local projection works; a source action is refused
    let d = ok(
        "representation.derive",
        json!({"artifact": x_id, "capability": "retheme", "input": {"theme": "dark"}}),
    );
    assert_eq!(d.result.as_ref().unwrap()["derivedFrom"], json!(x_id));
    let like = e.invoke(
        "representation.derive",
        &json!({"artifact": x_id, "capability": "like"}),
        &s,
    );
    let like = like.error.unwrap();
    assert_eq!(like.code, "unauthorized", "{like:?}");

    let c = ok(
        "representation.compare",
        json!({"a": x_id, "b": d.result.unwrap()["id"]}),
    );
    assert_eq!(c.result.unwrap()["views"]["v0"], "different");

    // annotate: inference only
    let reading = json!({"parent": "author.avatar", "by": {"name": "example-vision", "version": "1"}, "method": "read from v0", "at": "2026-11-16T13:00:00Z",
        "elements": [{"id": "author.avatar.reading", "kind": "text", "role": "description", "text": "a placeholder silhouette", "visible": true, "basis": "inferred"}]});
    ok(
        "representation.annotate",
        json!({"artifact": x_id, "annotation": reading}),
    );
    let mut claim = reading.clone();
    claim["elements"][0]["basis"] = json!("source");
    let bad = e.invoke(
        "representation.annotate",
        &json!({"artifact": x_id, "annotation": claim}),
        &s,
    );
    assert!(bad.error.unwrap().message.contains("only add inferred"));

    // ingest: a committed example passes; a tampered copy is refused
    ok(
        "representation.ingest",
        json!({"path": root().join("docs/artifacts/x-post")}),
    );
    let tampered = root()
        .join("tests")
        .join(format!("tmp-tampered-{}", std::process::id()));
    let p = xeet_artifact::Package::read_dir(&root().join("docs/artifacts/github-pr")).unwrap();
    p.write_dir(&tampered).unwrap();
    let doc = tampered.join("artifact.json");
    std::fs::write(
        &doc,
        std::fs::read_to_string(&doc)
            .unwrap()
            .replacen("Fall back", "Fell back", 1),
    )
    .unwrap();
    let refused = e.invoke("representation.ingest", &json!({"path": tampered}), &s);
    std::fs::remove_dir_all(&tampered).unwrap();
    assert_eq!(refused.error.unwrap().code, "verification_failed");

    // retrieve
    let all = ok("representation.retrieve", json!({}));
    assert!(all.result.as_ref().unwrap()["total"].as_u64().unwrap() >= 5);
    let prs = ok(
        "representation.retrieve",
        json!({"subjectType": "github.pull_request"}),
    );
    assert_eq!(prs.result.unwrap()["total"], 1);
}

#[test]
fn errors_are_specific() {
    let e = engine("errors");
    let s = all();
    let out = e.invoke("representation.ocr", &json!({}), &s);
    let er = out.error.unwrap();
    assert_eq!(er.code, "not_found");
    assert!(er.details["known"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k == "representation.understand"));
    let out = e.invoke(
        "representation.understand",
        &json!({"artifact": "sha256:0", "consumer": "smell", "extra": 1}),
        &s,
    );
    let er = out.error.unwrap();
    assert_eq!(er.code, "invalid_input");
    let at: Vec<&str> = er
        .details
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["pointer"].as_str().unwrap())
        .collect();
    assert_eq!(at, ["/consumer", "/extra"]);
    let out = e.invoke(
        "representation.understand",
        &json!({"artifact": format!("sha256:{}", "a".repeat(64))}),
        &s,
    );
    assert_eq!(out.error.unwrap().code, "not_found");
    let out = e.invoke(
        "representation.capture",
        &json!({"source": "x.post", "data": {"text": "x".repeat(17 * 1024 * 1024)}}),
        &s,
    );
    assert_eq!(out.error.unwrap().code, "limit_exceeded");
    let out = e.invoke(
        "representation.capture",
        &json!({"source": "x.post", "data": {"text": "hi"}}),
        &s,
    );
    assert!(
        out.error.unwrap().message.contains("no clock"),
        "capture time is never invented"
    );
}
