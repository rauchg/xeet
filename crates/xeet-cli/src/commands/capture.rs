//! `xeet capture`: the front door for screenshots.
//!
//! The native capture app (apps/macos) captures pixels and context, then
//! calls this command, which makes the durable artifact and reads its text
//! locally — through the same derive path the modality gateway uses, so a
//! later model request reuses the result instead of reading again.
//!
//! ```text
//! xeet capture --image shot.png [--context context.json] [--ocr auto|tesseract|helper:PATH|off]
//!              [--store DIR] [--grant …] [--allow-read DIR] [--report FILE]
//! ```
//!
//! Prints one JSON report: the artifact, whether text was read and by which
//! engine, line counts, uncertain lines, omissions, and timings. No image
//! bytes and no recognized text are in the report.

use super::*;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;
use xeet_engine::{store, Engine, Provider, Session};
use xeet_gateway::gateway::Gateway;
use xeet_gateway::inspect::Input;

pub const USAGE: &str = "\
capture --image PATH [--context CONTEXT.json] [--ocr auto|tesseract|helper:PATH|off]
        [--store DIR] [--grant …] [--allow-read DIR,…] [--report FILE]
                                         make a durable artifact of a screenshot and read
                                         its text on this device; prints a capture report.
                                         --ocr auto: Apple Vision helper (xeet-vision-ocr
                                         beside this binary), else tesseract, else none";

const KNOWN: &[&str] = &[
    "image",
    "context",
    "ocr",
    "store",
    "grant",
    "allow-read",
    "report",
    "help",
    "h",
];

enum Ocr {
    Tesseract(xeet_ocr::OcrProvider<xeet_ocr::TesseractBackend>),
    Helper(xeet_ocr::OcrProvider<xeet_ocr::HelperBackend>),
}

impl Ocr {
    fn provider(&self) -> &dyn Provider {
        match self {
            Ocr::Tesseract(p) => p,
            Ocr::Helper(p) => p,
        }
    }
}

/// Picks the OCR engine, and says why when there is none.
fn pick(spec: &str) -> (Option<Ocr>, Vec<String>) {
    let mut notes = vec![];
    let helper_beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("xeet-vision-ocr")))
        .filter(|p| p.exists());
    let helper =
        |path: PathBuf, notes: &mut Vec<String>| match xeet_ocr::HelperBackend::new(path.clone()) {
            Ok(h) => Some(Ocr::Helper(xeet_ocr::OcrProvider::new(h))),
            Err(e) => {
                notes.push(format!("OCR helper {} unavailable: {e}", path.display()));
                None
            }
        };
    let tesseract = |notes: &mut Vec<String>| match xeet_ocr::TesseractBackend::new(None, &["eng"])
    {
        Ok(t) => Some(Ocr::Tesseract(xeet_ocr::OcrProvider::new(t))),
        Err(e) => {
            notes.push(format!("tesseract unavailable: {e}"));
            None
        }
    };
    let chosen = match spec {
        "off" => {
            notes.push("OCR turned off (--ocr off)".into());
            None
        }
        "tesseract" => tesseract(&mut notes),
        s if s.starts_with("helper:") => helper(PathBuf::from(&s[7..]), &mut notes),
        _ => match helper_beside {
            Some(p) => helper(p, &mut notes).or_else(|| tesseract(&mut notes)),
            None => tesseract(&mut notes),
        },
    };
    (chosen, notes)
}

pub fn run(argv: &[String]) -> u8 {
    let args = match Args::parse(argv) {
        Ok(a) => a,
        Err(e) => return usage(&e),
    };
    if args.has("help") || args.has("h") {
        println!("{USAGE}");
        return EXIT_OK;
    }
    let unknown = args.unknown(KNOWN);
    if !unknown.is_empty() {
        return usage(&format!("unknown flag(s): {}", unknown.join(", ")));
    }
    match go(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xeet capture: {e}");
            EXIT_INPUT
        }
    }
}

fn usage(msg: &str) -> u8 {
    eprintln!("xeet capture: {msg}\n{USAGE}");
    EXIT_USAGE
}

fn go(args: &Args) -> Result<u8, String> {
    let t0 = Instant::now();
    let image = args.value("image").ok_or("--image PATH is required")?;
    let context: Option<Value> = match args.value("context") {
        Some(p) => Some(
            serde_json::from_slice(&std::fs::read(p).map_err(|e| format!("{p}: {e}"))?)
                .map_err(|e| format!("{p}: {e}"))?,
        ),
        None => None,
    };
    let root = args
        .value("store")
        .map(str::to_string)
        .or_else(|| std::env::var("XEET_STORE").ok())
        .unwrap_or_else(|| ".xeet-store".into());
    let engine = Engine::new(store::Store::open(Path::new(&root))?);
    let (ocr, mut notes) = pick(args.value("ocr").unwrap_or("auto"));
    let mut providers: Vec<&dyn Provider> = vec![&engine];
    if let Some(o) = &ocr {
        providers.push(o.provider());
    }
    let list = |v: Option<&str>| -> Vec<String> {
        v.map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
    };
    let session = Session {
        grants: list(Some(
            args.value("grant")
                .unwrap_or("store:read,store:write,fs:read,ocr:local"),
        ))
        .into_iter()
        .collect(),
        read_roots: {
            let mut r: Vec<PathBuf> = list(args.value("allow-read"))
                .into_iter()
                .map(PathBuf::from)
                .collect();
            if r.is_empty() {
                // Default: the image's own directory, and nothing else.
                r.push(
                    Path::new(image)
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from(".")),
                );
            }
            r
        },
        now: Some(super::artifact::iso8601(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        )),
    };
    let now = session.now.clone().unwrap_or_default();
    let image_path = Path::new(image)
        .canonicalize()
        .map_err(|e| format!("{image}: {e}"))?;
    let g = Gateway::new(providers);
    let t1 = Instant::now();
    let rec = g.derive(&Input::Path(image_path), &session, &now, context.as_ref());
    let derive_ms = t1.elapsed().as_secs_f64() * 1e3;

    let read = rec.steps.iter().find(|s| s.purpose == "read");
    let ocr_result = read.and_then(|s| s.selected.as_ref()).map(
        |c| json!({"provider": c.provider, "contract": format!("{}@{}", c.capability, c.version)}),
    );
    if let Some(s) = read {
        for (p, why) in &s.skipped {
            notes.push(format!("{p} skipped: {why}"));
        }
        if let Some(e) = &s.error {
            notes.push(format!(
                "text not read: {}",
                e["message"].as_str().unwrap_or("")
            ));
        }
    }
    // Line counts from the delivered artifact (no text in the report).
    let mut lines = None;
    if let Some(uri) = &rec.artifact {
        let o = engine.invoke(
            "representation.inspect",
            &json!({"artifact": uri}),
            &session,
        );
        if let Some(doc) = o.result.as_ref().map(|r| &r["document"]) {
            let els = doc["semantic"]["elements"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let ocr_lines: Vec<&Value> = els.iter().filter(|e| e["role"] == "ocr.line").collect();
            let uncertain = ocr_lines
                .iter()
                .filter(|e| e["attrs"].get("ocr.uncertain").is_some())
                .count();
            let ax = els
                .iter()
                .filter(|e| {
                    e["id"]
                        .as_str()
                        .is_some_and(|i| i == "ax" || i.starts_with("ax."))
                })
                .count();
            lines = Some(
                json!({"ocrLines": ocr_lines.len(), "uncertainLines": uncertain, "accessibilityNodes": ax,
                "omitted": doc["limitations"]["omitted"].as_array().map(|a| a.iter().map(|n| n["what"].clone()).collect::<Vec<_>>())}),
            );
        }
    }
    let report = json!({
        "format": "xeet-capture.report",
        "version": "0.1",
        "ok": rec.artifact.is_some(),
        "artifact": rec.artifact,
        "reused": rec.reused,
        "textRead": rec.content_read,
        "ocr": ocr_result,
        "summary": lines,
        "steps": rec.steps.iter().map(|s| json!({"purpose": s.purpose, "ok": s.ok, "provider": s.selected.as_ref().map(|c| c.provider.clone()), "resultDigest": s.result_digest})).collect::<Vec<_>>(),
        "imageDigest": rec.inspection.digest,
        "imageSize": rec.inspection.dimensions,
        "notes": notes,
        "reason": if rec.artifact.is_none() { Some(rec.reason.clone()) } else { None },
        "timingMs": {"derive": (derive_ms * 10.0).round() / 10.0, "total": (t0.elapsed().as_secs_f64() * 1e4).round() / 10.0},
    });
    let text = serde_json::to_string_pretty(&report).unwrap();
    match args.value("report") {
        Some(p) => std::fs::write(p, &text).map_err(|e| format!("{p}: {e}"))?,
        None => println!("{text}"),
    }
    Ok(if rec.artifact.is_some() {
        EXIT_OK
    } else {
        EXIT_INPUT
    })
}
