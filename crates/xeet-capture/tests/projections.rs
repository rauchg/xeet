//! Model-independent consumption: a text-only consumer must be able to get
//! everything an artifact knows without the image, deterministically, and
//! anything read from pixels must arrive labeled as inference.

use std::path::{Path, PathBuf};

use serde_json::json;
use xeet_artifact::annotate::{annotate, Annotation};
use xeet_artifact::project::{self, Consumer, Form, Request, Section};
use xeet_artifact::{canon, verify, Artifact, Basis, Element, Fact, Package, Tool};
use xeet_capture::{x, Reproducers};
use xeet_core::model::RenderOptions;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn example(name: &str) -> (Package, Artifact) {
    let p = Package::read_dir(&root().join("docs/artifacts").join(name)).unwrap();
    let a = p.artifact().unwrap();
    (p, a)
}

fn text(a: &Artifact, req: &Request) -> String {
    project::render(a, req, Form::Text).unwrap()
}

fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn the_text_projection_carries_every_meaning_without_the_image() {
    for name in ["x-post", "github-pr"] {
        let (p, a) = example(name);
        let t = text(&a, &Request::default());
        for e in &a.semantic.elements {
            assert!(
                t.contains(&format!(" {} {} {}", e.id, e.kind, e.role)),
                "{name}: {} missing",
                e.id
            );
            if let Some(s) = &e.text {
                assert!(t.contains(&q(s)), "{name}: text of {} missing", e.id);
            }
            if let Some(v) = &e.value {
                assert!(
                    t.contains(&canon::canonical(v)),
                    "{name}: value of {} missing",
                    e.id
                );
            }
            for b in &e.bounds {
                assert!(
                    t.contains(&format!("@{}(", b.view)),
                    "{name}: bounds of {}",
                    e.id
                );
            }
        }
        for f in &a.state.facts {
            assert!(
                t.contains(&format!(
                    "{}.{} = {}",
                    f.subject,
                    f.key,
                    canon::canonical(&f.value)
                )),
                "{name}: fact {}.{}",
                f.subject,
                f.key
            );
        }
        for c in &a.capabilities {
            assert!(
                t.contains(&format!("  {} ", c.id)),
                "{name}: capability {}",
                c.id
            );
        }
        assert!(t.contains(&a.id) && t.contains(&a.provenance.capture.captured_at));
        // No pixels travel in a text projection, and it is far smaller than the package.
        let png = p.files.values().map(Vec::len).max().unwrap();
        assert!(
            t.len() * 4 < png,
            "{name}: {} chars vs {png} bytes",
            t.len()
        );
        assert!(!t.contains("iVBOR"), "no base64 PNG");
    }
}

#[test]
fn projections_are_deterministic_in_both_forms() {
    let (_, a) = example("github-pr");
    for form in [Form::Text, Form::Json] {
        for req in [
            Request::default(),
            Request {
                focus: Some("reviews".into()),
                max_chars: Some(1500),
                ..Request::default()
            },
        ] {
            assert_eq!(
                project::render(&a, &req, form).unwrap(),
                project::render(&a, &req, form).unwrap()
            );
        }
    }
    // The JSON form is the same projection, typed.
    let json = project::render(&a, &Request::default(), Form::Json).unwrap();
    let back: project::Projection = serde_json::from_str(&json).unwrap();
    assert_eq!(back, project::project(&a, &Request::default()).unwrap());
    assert_eq!(back.elements.len(), a.semantic.elements.len());
}

#[test]
fn layout_rows_preserve_spatial_organization() {
    let (_, a) = example("x-post");
    let p = project::project(&a, &Request::default()).unwrap();
    let row_of = |id: &str| {
        p.layout
            .iter()
            .position(|r| r.items.iter().any(|i| i == id))
            .unwrap()
    };
    assert_eq!(row_of("author.name"), row_of("author.handle"));
    let header = &p.layout[row_of("author.name")].items;
    let pos = |id: &str| header.iter().position(|i| i == id).unwrap();
    assert!(pos("author.avatar") < pos("author.name") && pos("author.name") < pos("author.handle"));
    let actions = &p.layout[row_of("action.like")].items;
    assert_eq!(
        actions,
        &[
            "action.reply",
            "action.repost",
            "action.like",
            "action.views",
            "action.bookmark",
            "action.share"
        ]
    );
    assert!(
        row_of("body") < row_of("card") && row_of("card") < row_of("action.like"),
        "top to bottom"
    );
}

#[test]
fn a_focused_projection_keeps_context_and_says_what_it_left_out() {
    let (_, a) = example("github-pr");
    let req = Request {
        focus: Some("reviews".into()),
        ..Request::default()
    };
    let p = project::project(&a, &req).unwrap();
    let ids: Vec<&str> = p.elements.iter().map(|e| e.id.as_str()).collect();
    assert!(
        ids.contains(&"pr")
            && ids.contains(&"reviews")
            && ids.contains(&"review:5001")
            && ids.contains(&"review:5002")
    );
    assert!(
        p.elements
            .iter()
            .find(|e| e.id == "pr")
            .unwrap()
            .context_only
    );
    assert!(!ids.iter().any(|i| i.starts_with("file:")));
    assert!(p.omitted.elements.iter().any(|i| i == "file:docs/CI.md"));
    assert!(p
        .relations
        .iter()
        .any(|r| r.from == "review:5002" && r.kind == "reviews"));
    assert!(
        p.state.iter().any(|f| f.key == "reviewDecision"),
        "facts on context ancestors stay"
    );
    assert_eq!(
        p.context.captured_at, a.provenance.capture.captured_at,
        "provenance survives slicing"
    );
    assert!(project::render(
        &a,
        &Request {
            focus: Some("nope".into()),
            ..Request::default()
        },
        Form::Text
    )
    .is_err());

    let only_state = Request {
        sections: vec![Section::State],
        ..Request::default()
    };
    let t = text(&a, &only_state);
    assert!(t.contains("STATE") && !t.contains("\nELEMENTS") && t.contains("sections: elements"));
}

#[test]
fn a_character_budget_is_met_by_priority_and_never_silently() {
    let (_, a) = example("github-pr");
    let full = text(&a, &Request::default()).chars().count();
    let mut last_sections: Vec<Section> = Vec::new();
    for budget in [1800, 2500, 3500, 4500, 6000, 9000, full] {
        let req = Request {
            max_chars: Some(budget),
            max_text: Some(80),
            ..Request::default()
        };
        let t = text(&a, &req);
        let json = project::render(&a, &req, Form::Json).unwrap();
        let p: project::Projection = serde_json::from_str(&json).unwrap();
        assert!(
            t.chars().count() <= budget,
            "{budget}: {}",
            t.chars().count()
        );
        assert!(
            t.starts_with("ARTIFACT ") && t.contains("OBSERVED "),
            "header is never dropped"
        );
        // Every element is either shown or listed as omitted.
        let shown = t
            .lines()
            .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
            .count();
        let _ = shown;
        for e in &a.semantic.elements {
            assert!(
                t.contains(&format!(" {} {} ", e.id, e.kind)) || t.contains(&e.id),
                "{budget}: {} vanished",
                e.id
            );
        }
        // A larger budget never keeps fewer sections.
        let kept: Vec<Section> = project::ALL_SECTIONS
            .iter()
            .copied()
            .filter(|s| !p.omitted.sections.contains(s))
            .collect();
        assert!(
            last_sections.iter().all(|s| kept.contains(s)),
            "{budget}: {kept:?} lost some of {last_sections:?}"
        );
        last_sections = kept;
        if budget < full {
            assert!(t.contains("OMITTED FROM THIS PROJECTION"), "{budget}");
        }
    }
    // Top-level parts survive a tight budget; state and (compact) capabilities
    // come before list items as the budget grows.
    let t = text(
        &a,
        &Request {
            max_chars: Some(2500),
            max_text: Some(80),
            ..Request::default()
        },
    );
    assert!(t.contains(" pr.title heading"), "{t}");
    let t = text(
        &a,
        &Request {
            max_chars: Some(6000),
            max_text: Some(80),
            ..Request::default()
        },
    );
    assert!(
        t.contains("\nSTATE")
            && t.contains("compact: inputs")
            && t.contains("  merge mutation on pr"),
        "{t}"
    );
    let tiny = text(
        &a,
        &Request {
            max_chars: Some(50),
            ..Request::default()
        },
    );
    assert!(p_exceeded(&tiny));
}

#[test]
fn text_and_vision_consumers_get_the_same_artifact_differently() {
    let (_, a) = example("x-post");
    let t = text(&a, &Request::default());
    let v = text(
        &a,
        &Request {
            consumer: Consumer::Vision,
            ..Request::default()
        },
    );
    assert!(t.contains("image not included"));
    assert!(v.contains("image attached; anything read from its pixels is an inference"));
    // Same elements, same ids, same facts: only the view line differs.
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.starts_with("ARTIFACT") && !l.starts_with("VIEW"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&t), strip(&v));

    let (_, pr) = example("github-pr");
    let cov = project::consumer_coverage(&pr);
    assert!(
        cov.visual_only.is_empty(),
        "nothing in the PR requires vision: {cov:?}"
    );
    assert!(cov
        .text_consumer
        .contains("nothing semantic requires vision"));
}

fn media_post() -> (Package, Artifact) {
    let json = std::fs::read(root().join("tests/fixtures/posts/media-four.json")).unwrap();
    let p = x::capture(&x::Capture {
        post_json: &json,
        options: RenderOptions {
            width: 390.0,
            ..RenderOptions::default()
        },
        captured_at: "2026-11-16T12:00:00Z".into(),
        method: "fixture".into(),
        source_url: None,
        source_id: None,
        extra_inputs: vec![],
    })
    .unwrap();
    let a = p.artifact().unwrap();
    (p, a)
}

fn reading(basis: Basis) -> Annotation {
    let mut e = Element {
        id: "media.0.reading".into(),
        kind: "text".into(),
        role: "description".into(),
        text: Some("a bar chart; the tallest bar reads about 120".into()),
        visible: true,
        basis,
        ..Element::default()
    };
    e.attrs.insert("from".into(), json!("v0"));
    Annotation {
        parent: "media.0".into(),
        elements: vec![e],
        facts: vec![Fact {
            subject: "media.0".into(),
            key: "approxMax".into(),
            value: json!(120),
            basis: Basis::Inferred,
            source_time: None,
            volatile: false,
        }],
        by: Tool {
            name: "example-vision-model".into(),
            version: "1".into(),
            ruleset: None,
        },
        method: "read from view v0".into(),
        at: "2026-11-16T13:00:00Z".into(),
    }
}

#[test]
fn pixel_only_content_is_named_and_can_only_be_added_as_inference() {
    let (p, a) = media_post();
    let cov = project::consumer_coverage(&a);
    let media: Vec<&str> = cov
        .visual_only
        .iter()
        .map(String::as_str)
        .filter(|i| i.starts_with("media."))
        .collect();
    assert!(
        !media.is_empty(),
        "images without alt text are visual-only: {cov:?}"
    );
    assert!(text(&a, &Request::default()).contains("[visual-only]"));

    // Anything other than inferred is refused, as are edits to observations.
    assert!(annotate(&p, &reading(Basis::Source))
        .unwrap_err()
        .contains("only add inferred"));
    let mut overwrite = reading(Basis::Inferred);
    overwrite.elements[0].id = "body".into();
    assert!(annotate(&p, &overwrite).is_err());
    let mut refact = reading(Basis::Inferred);
    refact.facts[0] = Fact {
        subject: "post".into(),
        key: "weightedLength".into(),
        value: json!(1),
        basis: Basis::Inferred,
        source_time: None,
        volatile: true,
    };
    assert!(annotate(&p, &refact)
        .unwrap_err()
        .contains("do not override"));
    let mut anonymous = reading(Basis::Inferred);
    anonymous.by.name.clear();
    assert!(annotate(&p, &anonymous).is_err());

    // A proper annotation: a new artifact, derived, verifiable, reproducible.
    let child = annotate(&p, &reading(Basis::Inferred)).unwrap();
    let c = child.artifact().unwrap();
    let v = verify::verify(&child, Some(&Reproducers));
    assert!(v.ok, "{v:#?}");
    assert_eq!(c.provenance.derived_from.as_deref(), Some(a.id.as_str()));
    assert_ne!(c.id, a.id);
    assert_eq!(c.views, a.views, "pixels unchanged");
    let step = c.provenance.transformations.last().unwrap();
    assert!(!step.deterministic && step.at.is_some());

    let t = text(&c, &Request::default());
    assert!(t.contains("media.0.reading text description \"a bar chart; the tallest bar reads about 120\" !inferred"), "{t}");
    assert!(t.contains("media.0.approxMax = 120 !inferred"));
    assert!(t.contains("inferred: media.0.reading"));
    assert!(t.contains("DERIVED FROM"));
}

fn p_exceeded(t: &str) -> bool {
    t.contains("even the minimal projection") && t.contains("exceeds the requested budget")
}
