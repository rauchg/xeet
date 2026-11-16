//! The modality gateway end to end, with deterministic stand-ins for the
//! vision provider and the models (see `support`).

mod support;

use serde_json::json;
use support::*;
use xeet_engine::Provider;
use xeet_gateway::gateway::{Gateway, Task};
use xeet_gateway::inspect::Input;
use xeet_gateway::request::{Content, ErrorKind};

const ASK: &str = "Look at this screenshot and tell me which invoices are overdue.";

fn screenshot() -> Input {
    Input::Bytes {
        bytes: fixture("invoices.png"),
        declared_type: Some("image/png".into()),
        name: Some("invoices.png".into()),
    }
}

fn task(inputs: Vec<Input>) -> Task {
    Task {
        instruction: ASK.into(),
        inputs,
    }
}

#[test]
fn a_text_only_model_answers_from_an_injected_representation_of_a_screenshot() {
    let engine = engine("e2e");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    let mut model = TextOnlyModel::default();
    let run = g.run(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &mut model,
        &all_grants(),
        T,
    );

    // The model accepted the request and answered from the representation.
    let answer = run.response.expect("accepted").text;
    assert_eq!(answer, "Overdue: INV-1041, INV-1043. Unclear: INV-1045.");
    assert_eq!(run.record.result, "accepted");
    assert_eq!(run.record.attempts.len(), 1, "preflight was enough");

    // The instruction is the first content item, byte for byte; no image was sent.
    let sent = &model.seen[0];
    assert_eq!(
        sent.messages[0].content[0],
        Content::Text { text: ASK.into() }
    );
    assert!(sent.media_types().is_empty());
    let injected = &texts(sent)[1];
    assert!(injected
        .starts_with("[attachment 1 of 1: image/png 480x356 \"invoices.png\", sha256:15b81b83"));
    assert!(injected
        .contains("the original is not included; it is kept by the runtime as artifact://sha256/"));
    assert!(injected.contains("!inferred") && injected.contains("vision.confidence"));
    assert!(
        injected.contains("\"the status text is cut off"),
        "uncertainty is delivered, not resolved"
    );
    assert!(injected.contains("OBSERVED") && injected.contains("authenticity unverified"));

    // The record explains every step and holds no image content.
    let part = &run.record.attempts[0].prepared.parts[0];
    assert_eq!(part.decision, "represent");
    assert!(part.content_read);
    let purposes: Vec<&str> = part.steps.iter().map(|s| s.purpose.as_str()).collect();
    assert_eq!(
        purposes,
        ["reuse", "ingest", "validate", "read", "annotate", "validate", "project"]
    );
    let read = &part.steps[3];
    assert_eq!(
        read.selected.as_ref().unwrap().provider,
        "test-vision-reader"
    );
    assert!(run.record.attempts[0]
        .prepared
        .request_summary
        .iter()
        .all(|s| s["type"] == "text"));
    let record = serde_json::to_string(&run.record).unwrap();
    assert!(
        !record.contains("iVBOR") && !record.contains(ASK),
        "no image bytes or instruction text in the record"
    );
    assert!(run.record.id.starts_with("sha256:"));

    // The delivered artifact is in the store and verifies, readings marked inferred.
    let uri = part.artifact.clone().unwrap();
    let v = engine.invoke(
        "representation.verify",
        &json!({"artifact": uri}),
        &all_grants(),
    );
    assert_eq!(v.result.unwrap()["ok"], true);
}

#[test]
fn a_vision_model_receives_the_original_image() {
    let engine = engine("vision");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    let mut model = VisionModel::default();
    let run = g.run(
        &task(vec![screenshot()]),
        &profile("vision"),
        &mut model,
        &all_grants(),
        T,
    );
    assert_eq!(
        run.response.unwrap().text,
        "received media: [\"image/png\"]"
    );
    let part = &run.record.attempts[0].prepared.parts[0];
    assert_eq!(part.decision, "pass-through");
    assert!(
        part.steps.is_empty(),
        "no transformation for a model that can see"
    );
    match &model.seen[0].messages[0].content[1] {
        Content::Media { data_base64, .. } => assert_eq!(
            xeet_engine::unb64(data_base64).unwrap(),
            fixture("invoices.png")
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_existing_representation_is_reused_not_recreated() {
    let engine = engine("reuse");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    let first = g.prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &all_grants(),
        T,
    );
    let second = g.prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &all_grants(),
        T,
    );
    let (a, b) = (&first.parts[0], &second.parts[0]);
    assert!(!a.reused && b.reused);
    assert_eq!(
        a.artifact, b.artifact,
        "the annotated artifact is found by input digest"
    );
    let purposes: Vec<&str> = b.steps.iter().map(|s| s.purpose.as_str()).collect();
    assert_eq!(purposes, ["reuse", "validate", "project"]);
    assert!(b.content_read);
    // Same inputs, same request.
    assert_eq!(first.request_digest, second.request_digest);
}

#[test]
fn a_modality_with_no_provider_is_reported_not_dropped() {
    let engine = engine("audio");
    let g = Gateway::new(vec![&engine as &dyn Provider]);
    let audio = Input::Bytes {
        bytes: b"ID3\x04\x00\x00\x00\x00\x00\x00fake mp3 frames".to_vec(),
        declared_type: None,
        name: Some("note.mp3".into()),
    };
    let mut model = TextOnlyModel::default();
    let run = g.run(
        &task(vec![audio]),
        &profile("text-only"),
        &mut model,
        &all_grants(),
        T,
    );
    let part = &run.record.attempts[0].prepared.parts[0];
    assert_eq!(part.inspection.media_type, "audio/mpeg");
    assert_eq!(part.decision, "unavailable");
    assert_eq!(
        part.steps
            .iter()
            .find(|s| s.purpose == "ingest")
            .unwrap()
            .error
            .as_ref()
            .unwrap()["code"],
        "no_provider"
    );
    let notice = &texts(&model.seen[0])[1];
    assert!(
        notice.contains("audio/mpeg")
            && notice.contains("Not delivered")
            && notice.contains("kept by the runtime"),
        "{notice}"
    );
    assert_eq!(
        run.record.result, "accepted",
        "the task still goes ahead, honestly"
    );
}

#[test]
fn without_a_reader_the_content_is_declared_unknown_not_guessed() {
    let engine = engine("no-reader");
    let g = Gateway::new(vec![&engine as &dyn Provider]);
    let p = g.prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &all_grants(),
        T,
    );
    let part = &p.parts[0];
    assert_eq!(part.decision, "represent");
    assert!(!part.content_read);
    assert_eq!(
        part.steps
            .iter()
            .find(|s| s.purpose == "read")
            .unwrap()
            .error
            .as_ref()
            .unwrap()["code"],
        "no_provider"
    );
    let injected = &texts(&p.request)[1];
    assert!(
        injected.contains("no reader was available: the content itself is unknown")
            && injected.contains("[visual-only]")
    );
    assert!(
        injected.contains("480x356"),
        "what is known is still delivered"
    );
    assert!(!injected.contains("INV-"), "nothing is invented");
}

#[test]
fn a_wrong_profile_is_corrected_by_a_bounded_fallback_on_confirmed_rejection() {
    let engine = engine("fallback");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    // Provider metadata claims images work; the model says otherwise.
    let mut wrong = profile("vision");
    wrong.model = "text-only-1".into();
    let mut model = TextOnlyModel::default();
    let run = g.run(
        &task(vec![screenshot()]),
        &wrong,
        &mut model,
        &all_grants(),
        T,
    );
    assert_eq!(run.record.attempts.len(), 2);
    let first = &run.record.attempts[0];
    assert_eq!(first.outcome, "rejected");
    assert_eq!(
        first.classification.as_ref().unwrap().kind,
        ErrorKind::UnsupportedModality
    );
    assert_eq!(run.record.fallback["triggered"], true);
    assert_eq!(run.record.fallback["mediaTypes"], json!(["image/png"]));
    assert_eq!(
        run.record.attempts[1].observed[0]["source"],
        "observed-rejection"
    );
    assert_eq!(run.record.attempts[1].outcome, "accepted");
    assert_eq!(run.record.result, "accepted");
    assert_eq!(
        run.response.unwrap().text,
        "Overdue: INV-1041, INV-1043. Unclear: INV-1045."
    );
}

#[test]
fn unrelated_failures_never_trigger_a_transformation() {
    let engine = engine("unrelated");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    let real_429 = String::from_utf8(fixture("openai-429-insufficient-quota.json")).unwrap();
    for (status, body, kind) in [
        (429, real_429.as_str(), ErrorKind::Quota),
        (429, "{\"error\":{\"message\":\"Rate limit reached for requests\"}}", ErrorKind::RateLimit),
        (401, "{\"error\":{\"message\":\"Incorrect API key provided\"}}", ErrorKind::Authentication),
        (400, "{\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"maximum context length exceeded; the image and text...\"}}", ErrorKind::ContextOverflow),
        (500, "internal error", ErrorKind::Server),
    ] {
        let mut model = FailingModel { status, body: body.into(), sends: 0 };
        let run = g.run(&task(vec![screenshot()]), &profile("vision"), &mut model, &all_grants(), T);
        assert_eq!(model.sends, 1, "{kind:?}: exactly one send");
        assert_eq!(run.record.attempts[0].classification.as_ref().unwrap().kind, kind);
        assert_eq!(run.record.fallback["triggered"], false);
        assert_eq!(run.record.result, "rejected");
        assert!(run.response.is_none());
    }
}

#[test]
fn fallback_is_bounded_even_when_the_model_keeps_refusing() {
    let engine = engine("bounded");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    let mut model = FailingModel {
        status: 400,
        body: "image input is not supported for this model".into(),
        sends: 0,
    };
    let run = g.run(
        &task(vec![screenshot()]),
        &profile("vision"),
        &mut model,
        &all_grants(),
        T,
    );
    assert_eq!(model.sends, 2, "one fallback, then stop");
    assert_eq!(run.record.result, "rejected");
    assert!(
        run.record.fallback["reason"]
            .as_str()
            .unwrap()
            .contains("no such media"),
        "{}",
        run.record.fallback
    );
}

#[test]
fn a_small_context_gets_a_budgeted_projection_and_more_on_request() {
    let engine = engine("budget");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    let mut small = profile("text-only");
    small.context_tokens = Some(1024 + 900); // after the reserve: ~3.6k chars, minus fixed text
    let p = g.prepare(&task(vec![screenshot()]), &small, &all_grants(), T);
    let part = &p.parts[0];
    let budget = part.budget_chars.unwrap();
    assert!(budget < 3000, "{budget}");
    let delivery = part.delivery.as_ref().unwrap();
    assert!(
        delivery["chars"].as_u64().unwrap() as usize <= budget
            || delivery["omitted"]["budgetExceeded"] == true
    );
    let omitted: Vec<String> =
        serde_json::from_value(delivery["omitted"]["elements"].clone()).unwrap();
    assert!(!omitted.is_empty(), "something had to give: {delivery}");
    let injected = &texts(&p.request)[1];
    assert!(
        injected.contains("OMITTED FROM THIS PROJECTION"),
        "omissions are stated in what the model sees"
    );
    assert_eq!(p.budget.source, "profile");

    // Progressive retrieval of what was left out, by element id.
    let missing = omitted
        .iter()
        .find(|id| id.starts_with("reading.INV-"))
        .cloned()
        .unwrap_or_else(|| "reading".into());
    let more = g
        .expand(
            part.artifact.as_ref().unwrap(),
            Some(&missing),
            &["elements"],
            Some(2000),
            &all_grants(),
        )
        .unwrap();
    assert!(more.contains(&format!(" {missing} ")), "{more}");
}

#[test]
fn authority_stays_with_the_host() {
    let engine = engine("authority");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    // No vision:read: the reader is discovered but skipped, so content stays unknown.
    let p = g.prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &session(&["store:read", "store:write"]),
        T,
    );
    let read = p.parts[0]
        .steps
        .iter()
        .find(|s| s.purpose == "read")
        .unwrap();
    assert_eq!(read.candidates[0].provider, "test-vision-reader");
    assert!(read.skipped[0]
        .1
        .contains("unauthorized: this session lacks vision:read"));
    assert!(read.selected.is_none() && !p.parts[0].content_read);
    // No store:write (and a fresh store, so nothing can be reused): nothing is ingested.
    let fresh = support::engine("authority-fresh");
    let g = Gateway::new(vec![&fresh as &dyn Provider, &reader]);
    let p = g.prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &session(&["store:read", "vision:read"]),
        T,
    );
    assert_eq!(p.parts[0].decision, "unavailable");
    assert!(p.parts[0]
        .steps
        .iter()
        .find(|s| s.purpose == "ingest")
        .unwrap()
        .skipped[0]
        .1
        .contains("store:write"));
    // A path outside the read roots is refused before it is read.
    let p = g.prepare(
        &task(vec![Input::Path("/etc/hostname".into())]),
        &profile("text-only"),
        &all_grants(),
        T,
    );
    assert_eq!(p.parts[0].decision, "unavailable");
    assert!(
        p.parts[0].reason.contains("outside the read roots"),
        "{}",
        p.parts[0].reason
    );
}

#[test]
fn a_provider_returning_mismatched_content_is_rejected_by_digest() {
    let engine = engine("tamper");
    let liar = Tampering::new(&engine);
    let g = Gateway::new(vec![&liar as &dyn Provider]);
    let p = g.prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &all_grants(),
        T,
    );
    let part = &p.parts[0];
    assert_eq!(part.decision, "unavailable");
    let v = part.steps.iter().find(|s| s.purpose == "validate").unwrap();
    assert_eq!(v.error.as_ref().unwrap()["code"], "invalid_artifact_digest");
}

#[test]
fn declared_types_are_checked_against_content() {
    let engine = engine("mismatch");
    let g = Gateway::new(vec![&engine as &dyn Provider]);
    let lying = Input::Bytes {
        bytes: fixture("invoices.png"),
        declared_type: Some("image/jpeg".into()),
        name: Some("photo.jpg".into()),
    };
    let p = g.prepare(&task(vec![lying]), &profile("text-only"), &all_grants(), T);
    let i = &p.parts[0].inspection;
    assert_eq!(i.media_type, "image/png");
    assert!(i.declared_mismatch);
    assert!(
        texts(&p.request)[1].contains("declared type image/jpeg"),
        "the mismatch reaches the model as unverified"
    );
}

#[test]
fn a_path_input_inside_the_read_roots_works_without_loading_it_for_inspection() {
    let engine = engine("path");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    let input = Input::Path(root().join("tests/fixtures/gateway/invoices.png"));
    let mut model = TextOnlyModel::default();
    let run = g.run(
        &task(vec![input]),
        &profile("text-only"),
        &mut model,
        &all_grants(),
        T,
    );
    assert_eq!(
        run.response.unwrap().text,
        "Overdue: INV-1041, INV-1043. Unclear: INV-1045."
    );
    assert_eq!(
        run.record.attempts[0].prepared.parts[0].inspection.digest,
        "sha256:15b81b83902b4c53bd08f121145633089af221c168fecfeaf267dcbe8cf86b9f"
    );
}

#[test]
fn the_gateway_contract_is_committed_and_uses_only_validated_schema_keywords() {
    let m = xeet_gateway::provider::manifest();
    for c in &m.capabilities {
        for s in [&c.input_schema, &c.output_schema] {
            assert!(
                xeet_engine::schema::unsupported_keywords(s).is_empty(),
                "{}",
                c.id
            );
        }
    }
    let committed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root().join("docs/gateway.manifest.json")).unwrap())
            .unwrap();
    assert_eq!(committed, serde_json::to_value(&m).unwrap(), "regenerate: cargo run -q -p xeet-gateway --example print_manifest > docs/gateway.manifest.json");
}

#[test]
fn modality_adapt_through_the_provider_interface() {
    let engine = engine("adapt");
    let reader = TestReader::new();
    let gw = xeet_gateway::provider::GatewayProvider::new(vec![&engine as &dyn Provider, &reader]);
    let input = json!({"instruction": ASK, "model": serde_json::from_slice::<serde_json::Value>(&fixture("profile-text-only.json")).unwrap(),
        "inputs": [{"dataBase64": xeet_engine::b64(&fixture("invoices.png")), "name": "invoices.png"}]});
    let out = gw.invoke("modality.adapt", &input, &all_grants());
    assert!(out.ok, "{:?}", out.error);
    let r = out.result.unwrap();
    assert_eq!(r["content"][0]["text"], ASK);
    assert!(r["content"][1]["text"]
        .as_str()
        .unwrap()
        .contains("reading.INV-1043"));
    // The gateway's own contract is enforced like any other.
    let denied = gw.invoke("modality.adapt", &input, &session(&[]));
    assert_eq!(denied.error.unwrap().code, "unauthorized");
}

/// Prints sizes and timings (`cargo test -p xeet-gateway measure -- --nocapture`).
/// Asserts only what must hold; the numbers are for the report.
#[test]
fn measure() {
    use std::time::Instant;
    let engine = engine("measure");
    let reader = TestReader::new();
    let g = Gateway::new(vec![&engine as &dyn Provider, &reader]);
    let png = fixture("invoices.png");
    let t0 = Instant::now();
    let fresh = g.prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &all_grants(),
        T,
    );
    let fresh_ms = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = Instant::now();
    let reused = g.prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &all_grants(),
        T,
    );
    let reuse_ms = t1.elapsed().as_secs_f64() * 1e3;
    let without_reader = Gateway::new(vec![&engine as &dyn Provider]);
    let fresh_engine = support::engine("measure-noreader");
    let nr = Gateway::new(vec![&fresh_engine as &dyn Provider]).prepare(
        &task(vec![screenshot()]),
        &profile("text-only"),
        &all_grants(),
        T,
    );
    let _ = without_reader;
    let vision = g.prepare(
        &task(vec![screenshot()]),
        &profile("vision"),
        &all_grants(),
        T,
    );
    let chars = |p: &xeet_gateway::gateway::Prepared| {
        texts(&p.request)
            .iter()
            .map(|t| t.chars().count())
            .sum::<usize>()
    };
    let b64 = match &vision.request.messages[0].content[1] {
        Content::Media { data_base64, .. } => data_base64.len(),
        _ => 0,
    };
    assert!(reused.parts[0].reused);
    eprintln!(
        "image: {} bytes PNG; as base64 for a vision model: {b64} chars",
        png.len()
    );
    eprintln!(
        "text-only, with reader: {} chars injected (~{} tokens at 4 chars/token)",
        chars(&fresh),
        chars(&fresh) / 4
    );
    eprintln!("text-only, without reader: {} chars injected", chars(&nr));
    eprintln!("prepare, fresh (reuse miss, ingest, read, annotate, 3 validations, project): {fresh_ms:.1} ms");
    eprintln!("prepare, reused (lookup, validate, project): {reuse_ms:.1} ms");
}
