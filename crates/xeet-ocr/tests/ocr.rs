//! Real OCR on real images (Tesseract), the capture path, and screenshot
//! context. Tests that need Tesseract skip with a message when it is absent,
//! unless `XEET_REQUIRE_TESSERACT=1` (then they fail).

use std::cell::Cell;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use xeet_engine::{store::Store, Engine, Outcome, Provider, Session};
use xeet_gateway::gateway::{Gateway, Task};
use xeet_gateway::inspect::Input;
use xeet_gateway::model::ModelProfile;
use xeet_gateway::request::Content;
use xeet_ocr::{HelperBackend, OcrProvider, TesseractBackend, SCOPE};

const T: &str = "2026-11-16T12:00:00Z";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn invoices() -> Vec<u8> {
    std::fs::read(root().join("tests/fixtures/gateway/invoices.png")).unwrap()
}

fn tesseract() -> Option<TesseractBackend> {
    match TesseractBackend::new(None, &["eng"]) {
        Ok(t) => Some(t),
        Err(e) if std::env::var("XEET_REQUIRE_TESSERACT").as_deref() == Ok("1") => panic!("{e}"),
        Err(e) => {
            eprintln!("SKIPPED (no tesseract): {e}");
            None
        }
    }
}

fn session(grants: &[&str]) -> Session {
    Session {
        grants: grants.iter().map(|g| g.to_string()).collect(),
        read_roots: vec![root().join("tests")],
        now: Some(T.into()),
    }
}

fn all() -> Session {
    session(&["store:read", "store:write", "fs:read", SCOPE])
}

fn engine(name: &str) -> Engine {
    let dir = std::env::temp_dir().join(format!("xeet-ocr-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Engine::new(Store::open(&dir).unwrap())
}

fn read(p: &dyn Provider, bytes: &[u8]) -> Outcome {
    p.invoke("vision.read", &json!({"parent": "image", "image": {"mediaType": "image/png", "dataBase64": xeet_engine::b64(bytes)}}), &all())
}

fn lines(o: &Outcome) -> Vec<Value> {
    o.result.as_ref().unwrap()["annotation"]["elements"]
        .as_array()
        .unwrap()[1..]
        .to_vec()
}

fn find<'a>(ls: &'a [Value], needle: &str) -> &'a Value {
    ls.iter()
        .find(|l| l["text"].as_str().unwrap_or("").contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}: {ls:#?}"))
}

/// Crops a PNG to its left `w` pixels (RGBA8 or RGB8).
fn crop_png(bytes: &[u8], w: u32) -> Vec<u8> {
    let dec = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    let ch = info.color_type.samples();
    let mut out = Vec::new();
    for y in 0..info.height as usize {
        let row = &buf[y * info.line_size..y * info.line_size + w as usize * ch];
        out.extend_from_slice(row);
    }
    encode(w, info.height, info.color_type, &out)
}

fn encode(w: u32, h: u32, ct: png::ColorType, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    {
        let mut e = png::Encoder::new(&mut v, w, h);
        e.set_color(ct);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(data).unwrap();
    }
    v
}

#[test]
fn real_ocr_reads_real_text_with_positions_and_confidence() {
    let Some(t) = tesseract() else { return };
    let p = OcrProvider::new(t);
    let o = read(&p, &invoices());
    assert!(o.ok, "{:?}", o.error);
    let ls = lines(&o);
    assert!(ls.len() >= 10, "{}", ls.len());
    // Text is recognized, with the engine's own imperfections preserved.
    let row = find(&ls, "INV-1041");
    assert!(row["text"].as_str().unwrap().contains("Northwind Traders"));
    // Geometry: the row is drawn at y=146 in the 480x356 image.
    let b = &row["bounds"][0];
    assert!((b["y"].as_f64().unwrap() - 146.0).abs() <= 8.0, "{b}");
    let nb = row["attrs"]["ocr.boxNormalized"].as_array().unwrap();
    assert!((nb[1].as_f64().unwrap() - b["y"].as_f64().unwrap() / 356.0).abs() < 0.001);
    // Confidence is the engine's, in 0..1, not certainty.
    for l in &ls {
        let c = l["attrs"]["ocr.confidence"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&c));
        assert_eq!(l["basis"], "inferred");
    }
    // The cut-off "Overd…" is low-confidence, so its line is uncertain.
    let overd = find(&ls, "2026-10-20");
    assert!(
        overd["attrs"]["ocr.uncertain"]
            .as_str()
            .unwrap()
            .contains("low-confidence"),
        "{overd}"
    );
    // Attribution: engine, version and configuration, and the time.
    let ann = &o.result.as_ref().unwrap()["annotation"];
    assert_eq!(ann["by"]["name"], "xeet-ocr:tesseract");
    assert_eq!(ann["by"]["version"], p.manifest().provider.version);
    assert!(ann["method"].as_str().unwrap().starts_with("tesseract 5."));
    assert_eq!(ann["at"], T);
    // Deterministic for the same engine, configuration and input.
    assert_eq!(read(&p, &invoices()).result_digest, o.result_digest);
}

#[test]
fn clipped_and_blank_images_stay_uncertain_or_empty() {
    let Some(t) = tesseract() else { return };
    let p = OcrProvider::new(t);
    // The right part of every row is cut off.
    let o = read(&p, &crop_png(&invoices(), 120));
    assert!(o.ok, "{:?}", o.error);
    let clipped: Vec<&Value> = lines(&o)
        .iter()
        .filter(|l| {
            l["attrs"]["ocr.uncertain"]
                .as_str()
                .unwrap_or("")
                .contains("right edge")
        })
        .cloned()
        .collect::<Vec<_>>()
        .leak()
        .iter()
        .collect();
    assert!(
        clipped.len() >= 3,
        "rows cut by the crop are flagged: {:#?}",
        lines(&o)
    );
    // A blank image yields nothing, and says so; nothing is invented.
    let blank = encode(200, 80, png::ColorType::Rgb, &vec![255u8; 200 * 80 * 3]);
    let o = read(&p, &blank);
    assert!(o.ok);
    assert!(lines(&o).is_empty());
    assert!(o.result.unwrap()["annotation"]["elements"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("No text recognized"));
}

#[test]
fn authority_formats_and_scope_are_enforced() {
    let Some(t) = tesseract() else { return };
    let p = OcrProvider::new(t);
    let c = &p.manifest().capabilities[0];
    assert!(
        c.input_modalities.contains(&"image/png".to_string()),
        "formats come from the installed engine"
    );
    assert_eq!(p.manifest().constraints.network, "none");
    let o = p.invoke("vision.read", &json!({"parent": "image", "image": {"mediaType": "image/png", "dataBase64": xeet_engine::b64(&invoices())}}), &session(&["store:read"]));
    assert_eq!(o.error.unwrap().code, "unauthorized");
    let o = read(&p, b"not an image");
    assert_eq!(o.error.unwrap().code, "unsupported");
}

/// Counts invocations, to prove what did and did not run.
struct Counting<'a> {
    inner: &'a dyn Provider,
    reads: Cell<usize>,
}

impl Provider for Counting<'_> {
    fn manifest(&self) -> &xeet_engine::contract::Manifest {
        self.inner.manifest()
    }
    fn invoke(&self, cap: &str, input: &Value, s: &Session) -> Outcome {
        if cap == "vision.read" {
            self.reads.set(self.reads.get() + 1);
        }
        self.inner.invoke(cap, input, s)
    }
}

fn context() -> Value {
    // A region on a second display to the left of the main one (negative
    // global x), Retina scale 2: 240x178 points → the 480x356 image.
    json!({
        "capturedBy": {"name": "Xeet Capture", "version": "0.1.0"},
        "method": "screencapturekit", "mode": "region", "platform": "macOS 15.1 (24B83)",
        "display": {"id": 2, "name": "Studio Display", "frame": [-1920.0, 0.0, 1920.0, 1080.0], "scale": 2.0},
        "frame": [-1800.0, 100.0, 240.0, 178.0],
        "window": {"id": 77, "title": "Invoices — Acme Billing", "app": {"name": "Safari", "bundleId": "com.apple.Safari"}, "frame": [-1820.0, 60.0, 900.0, 700.0]},
        "permissions": {"screenRecording": "granted", "accessibility": "granted"},
        "accessibility": {"role": "AXWindow", "title": "Invoices — Acme Billing", "frame": [-1820.0, 60.0, 900.0, 700.0], "children": [
            {"role": "AXHeading", "title": "Invoices", "frame": [-1792.0, 122.0, 40.0, 12.0]},
            {"role": "AXButton", "title": "Export", "frame": [-1000.0, 100.0, 60.0, 20.0]}
        ]}
    })
}

#[test]
fn a_screenshot_becomes_an_artifact_read_once_and_reused() {
    let Some(t) = tesseract() else { return };
    let engine = engine("capture");
    let ocr = OcrProvider::new(t);
    let counted = Counting {
        inner: &ocr,
        reads: Cell::new(0),
    };
    let g = Gateway::new(vec![&engine as &dyn Provider, &counted]);
    let shot = Input::Bytes {
        bytes: invoices(),
        declared_type: Some("image/png".into()),
        name: Some("Screenshot.png".into()),
    };
    let ctx = context();

    let first = g.derive(&shot, &all(), T, Some(&ctx));
    let purposes: Vec<&str> = first.steps.iter().map(|s| s.purpose.as_str()).collect();
    assert_eq!(
        purposes,
        ["reuse", "ingest", "validate", "read", "annotate", "validate"]
    );
    assert!(first.content_read);
    assert_eq!(counted.reads.get(), 1);
    let uri = first.artifact.clone().unwrap();

    // The artifact verifies (including reproduction) and keeps the original pixels.
    let v = engine.invoke("representation.verify", &json!({"artifact": uri}), &all());
    assert_eq!(v.result.as_ref().unwrap()["ok"], true, "{v:?}");
    let doc = engine
        .invoke("representation.inspect", &json!({"artifact": uri}), &all())
        .result
        .unwrap()["document"]
        .clone();
    let a: xeet_artifact::Artifact = serde_json::from_value(doc).unwrap();
    assert_eq!(a.subject.kind, "screenshot");
    assert_eq!(
        a.views[0].asset,
        xeet_artifact::canon::sha256_id(&invoices()),
        "the view is the captured bytes"
    );
    assert!(
        a.provenance.derived_from.is_some(),
        "OCR made a new artifact; the original is untouched"
    );
    let roles: Vec<&str> = a
        .provenance
        .inputs
        .iter()
        .map(|i| i.role.as_str())
        .collect();
    assert_eq!(roles, ["source-image", "capture-context"]);

    // Three kinds of evidence, three bases.
    let el = |id: &str| {
        a.semantic
            .elements
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("{id}"))
    };
    assert_eq!(el("capture.window").basis, xeet_artifact::Basis::Observed);
    assert_eq!(el("ax").basis, xeet_artifact::Basis::Source);
    assert_eq!(el("ocr.line.0").basis, xeet_artifact::Basis::Inferred);
    // Accessibility frames map into image pixels (region origin, scale 2).
    let heading = el("ax.0");
    assert_eq!((heading.bounds[0].x, heading.bounds[0].y), (16.0, 44.0));
    let export = el("ax.1");
    assert!(
        !export.visible && export.bounds.is_empty(),
        "outside the captured region"
    );

    // Same pixels, same context: reused, OCR does not run again.
    let again = g.derive(&shot, &all(), T, Some(&ctx));
    assert_eq!(again.artifact.as_deref(), Some(uri.as_str()));
    assert!(again.reused);
    assert_eq!(counted.reads.get(), 1, "no second OCR");

    // Same pixels, different context: a different observation.
    let mut other = ctx.clone();
    other["frame"] = json!([-1790.0, 100.0, 240.0, 178.0]);
    let third = g.derive(&shot, &all(), T, Some(&other));
    assert_ne!(third.artifact, first.artifact);
}

#[test]
fn a_different_reader_reads_the_reused_original_without_re_ingesting() {
    let Some(t) = tesseract() else { return };
    let engine = engine("reader-change");
    let ocr = OcrProvider::new(t);
    let shot = Input::Bytes {
        bytes: invoices(),
        declared_type: None,
        name: None,
    };
    let g = Gateway::new(vec![&engine as &dyn Provider, &ocr]);
    g.derive(&shot, &all(), T, None);

    // A different engine identity: the scripted protocol helper below.
    let helper = OcrProvider::new(HelperBackend::new(helper_script()).unwrap());
    let g2 = Gateway::new(vec![&engine as &dyn Provider, &helper]);
    let r = g2.derive(&shot, &all(), T, None);
    let purposes: Vec<&str> = r.steps.iter().map(|s| s.purpose.as_str()).collect();
    assert_eq!(
        purposes,
        ["reuse", "validate", "read", "annotate", "validate"],
        "the original is reused, only the reading runs"
    );
}

/// A scripted helper speaking xeet-ocr/1. It tests the protocol adapter, not
/// Apple Vision: it returns a fixed reading for any image.
fn helper_script() -> PathBuf {
    let path = std::env::temp_dir().join(format!("xeet-ocr-helper-{}.sh", std::process::id()));
    let engine = r#"{"name":"scripted-helper","version":"1","languages":["en"],"config":{},"formats":["image/png"],"platform":"test","unsupported":["everything real: this is a protocol test"]}"#;
    let result = format!(
        r#"{{"protocol":"xeet-ocr/1","engine":{engine},"image":{{"width":480,"height":356,"orientation":"up"}},"readingOrder":"geometric","lines":[{{"text":"Invoices","box":[0.0333,0.1236,0.1625,0.0674],"confidence":0.99,"order":0,"words":[]}}],"warnings":[]}}"#
    );
    std::fs::write(&path, format!("#!/bin/sh\nif [ \"$1\" = --identify ]; then echo '{engine}'; else echo '{result}'; fi\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[test]
fn the_helper_protocol_maps_normalized_boxes_to_pixels() {
    let p = OcrProvider::new(HelperBackend::new(helper_script()).unwrap());
    assert_eq!(p.manifest().provider.id, "xeet-ocr:scripted-helper");
    let o = read(&p, &invoices());
    assert!(o.ok, "{:?}", o.error);
    let b = &lines(&o)[0]["bounds"][0];
    // 0.0333 x 480 = 15.98 px, quantized to 1/64 when the reading is annotated;
    // the provider's own output keeps the value it computed.
    assert_eq!(
        (b["x"].as_f64().unwrap(), b["y"].as_f64().unwrap()),
        (15.98, 44.0)
    );
}

#[test]
fn without_ocr_or_its_permission_the_content_stays_unknown() {
    let Some(t) = tesseract() else { return };
    let engine = engine("no-scope");
    let ocr = OcrProvider::new(t);
    let g = Gateway::new(vec![&engine as &dyn Provider, &ocr]);
    let shot = Input::Bytes {
        bytes: invoices(),
        declared_type: None,
        name: None,
    };
    let r = g.derive(&shot, &session(&["store:read", "store:write"]), T, None);
    assert!(!r.content_read);
    let read = r.steps.iter().find(|s| s.purpose == "read").unwrap();
    assert!(
        read.skipped[0].1.contains("lacks ocr:local"),
        "{:?}",
        read.skipped
    );

    // No OCR provider at all (fresh store, so nothing is reused).
    let fresh = self::engine("no-ocr");
    let alone = Gateway::new(vec![&fresh as &dyn Provider]);
    let r = alone.derive(&shot, &all(), T, None);
    assert!(!r.content_read);
    assert_eq!(
        r.steps
            .iter()
            .find(|s| s.purpose == "read")
            .unwrap()
            .error
            .as_ref()
            .unwrap()["code"],
        "no_provider"
    );
}

#[test]
fn accessibility_permission_denied_is_recorded_not_hidden() {
    let engine = engine("ax-denied");
    let mut ctx = context();
    ctx["permissions"]["accessibility"] = json!("denied");
    ctx.as_object_mut().unwrap().remove("accessibility");
    ctx["window"].as_object_mut().unwrap().remove("title");
    let o = engine.invoke(
        "representation.capture",
        &json!({"source": "screenshot", "dataBase64": xeet_engine::b64(&invoices()), "capturedAt": T, "context": ctx}),
        &all(),
    );
    assert!(o.ok, "{:?}", o.error);
    let doc = engine
        .invoke(
            "representation.inspect",
            &json!({"artifact": o.result.unwrap()["id"]}),
            &all(),
        )
        .result
        .unwrap()["document"]
        .clone();
    let a: xeet_artifact::Artifact = serde_json::from_value(doc).unwrap();
    let omitted: Vec<String> = a
        .limitations
        .omitted
        .iter()
        .map(|n| format!("{}: {}", n.what, n.why))
        .collect();
    assert!(
        omitted
            .iter()
            .any(|o| o.contains("accessibility tree: accessibility permission: denied")),
        "{omitted:?}"
    );
    assert!(
        omitted.iter().any(|o| o.starts_with("window title")),
        "{omitted:?}"
    );
    assert!(a
        .state
        .facts
        .iter()
        .any(|f| f.key == "permission.accessibility" && f.value == "denied"));
    // A screenshot without its context is refused.
    let o = engine.invoke("representation.capture", &json!({"source": "screenshot", "dataBase64": xeet_engine::b64(&invoices()), "capturedAt": T}), &all());
    assert!(o.error.unwrap().message.contains("capture context"));
}

#[test]
fn a_capture_reaches_text_only_and_vision_models_through_the_gateway() {
    let Some(t) = tesseract() else { return };
    let engine = engine("deliver");
    let ocr = OcrProvider::new(t);
    let g = Gateway::new(vec![&engine as &dyn Provider, &ocr]);
    let shot = Input::Bytes {
        bytes: invoices(),
        declared_type: None,
        name: None,
    };
    let captured = g.derive(&shot, &all(), T, Some(&context()));
    let uri = captured.artifact.unwrap();

    let profile = |name: &str| -> ModelProfile {
        serde_json::from_slice(
            &std::fs::read(root().join(format!("tests/fixtures/gateway/profile-{name}.json")))
                .unwrap(),
        )
        .unwrap()
    };
    let ask = "Which invoices are overdue?";
    // Text-only: the capture by reference, delivered as text with OCR and geometry.
    let p = g.prepare(
        &Task {
            instruction: ask.into(),
            inputs: vec![Input::Artifact(uri.clone())],
        },
        &profile("text-only"),
        &all(),
        T,
    );
    let text = match &p.request.messages[0].content[1] {
        Content::Text { text } => text.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        p.request.messages[0].content[0],
        Content::Text { text: ask.into() }
    );
    assert!(p.request.media_types().is_empty());
    assert!(
        text.contains("ocr.line")
            && text.contains("INV-1043")
            && text.contains("!inferred")
            && text.contains("@v0("),
        "{text}"
    );
    assert!(
        text.contains("screencapturekit") || text.contains("Safari"),
        "capture context reaches the model"
    );
    // Vision: the original pixels.
    let p = g.prepare(
        &Task {
            instruction: ask.into(),
            inputs: vec![shot],
        },
        &profile("vision"),
        &all(),
        T,
    );
    assert_eq!(p.request.media_types(), ["image/png"]);
}
