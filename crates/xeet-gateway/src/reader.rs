//! A real `vision.read` provider: a vision-capable model, reached through an
//! OpenAI-compatible endpoint (Vercel AI Gateway), reads an image and its
//! reading becomes an annotation whose every element is `inferred`.
//!
//! This is the one place a model interprets pixels, and its output is treated
//! accordingly: non-deterministic (declared in the contract), carrying the
//! model's own confidence and stated uncertainty, attributed to the model,
//! the prompt digest and the time, and never promoted to fact. Boxes the model
//! gives outside the image are dropped, and the drop is recorded.

use std::cell::RefCell;

use serde_json::{json, Value};
use xeet_engine::contract::{self, modality, Constraints, Contract, Manifest, Mode};
use xeet_engine::{schema, Outcome, Provider, Session};

use crate::openai::Transport;

pub const PROMPT: &str = "You are reading an image for a system that records your reading as uncertain inference, never as fact. \
Return ONLY a JSON object: {\"summary\": string, \"items\": [{\"text\": string, \"kind\": \"heading\"|\"text\"|\"table-row\"|\"label\"|\"value\"|\"other\", \
\"box\": [x, y, width, height] in pixels of this WIDTHxHEIGHT image or null, \"confidence\": number from 0 to 1, \"uncertain\": null or a string saying what is unclear}]}. \
Transcribe visible text exactly as shown, one item per line or table row, in reading order. If text is cut off or illegible, transcribe only what is visible \
and explain in \"uncertain\"; do not complete or guess it.";

pub struct ModelReader<T: Transport> {
    manifest: Manifest,
    pub model: String,
    pub endpoint: String,
    transport: RefCell<T>,
}

impl<T: Transport> ModelReader<T> {
    pub fn new(model: &str, endpoint: &str, transport: T) -> Self {
        let mut m = contract::manifest();
        m.provider = contract::Provider {
            id: format!("model-reader:{model}"),
            name: format!("Vision reader backed by {model}"),
            version: env!("CARGO_PKG_VERSION").into(),
            description: format!("Reads images with the vision model {model} via {endpoint}; every result is inference."),
        };
        m.sources.clear();
        m.non_capabilities.clear();
        m.limitations = vec![
            "readings are model output: they can be wrong; confidence values are the model's own"
                .into(),
        ];
        m.authority.scopes = vec![contract::Scope {
            id: "vision:read".into(),
            meaning: format!("send image bytes to {model} via {endpoint}"),
        }];
        m.constraints = Constraints {
            network: endpoint.into(),
            deterministic: false,
            max_input_bytes: 16 << 20,
            notes: vec![],
        };
        m.capabilities = vec![Contract {
            id: "vision.read".into(),
            version: "0.1".into(),
            title: "Read an image with a vision model".into(),
            summary: format!("Transcribe and structure an image's visible content with {model}; returns an annotation of inferred elements with the model's confidence."),
            description: "The image is sent to a remote model. Results are inference: attributed, timestamped, with confidence and stated uncertainty.".into(),
            outcomes: vec!["read text from an image".into(), "ocr an arbitrary image".into(), "describe a picture".into(), "extract chart values from pixels".into()],
            input_modalities: vec![modality::PNG.into(), "image/jpeg".into(), "image/gif".into(), "image/webp".into()],
            modes: vec![Mode { output: vec![modality::JSON.into()], select: json!({}), note: "annotation".into() }],
            input_schema: json!({"type": "object", "required": ["image", "parent"], "properties": {
                "artifact": {"type": "string"}, "parent": {"type": "string"},
                "image": {"type": "object", "required": ["mediaType", "dataBase64"], "properties": {"mediaType": {"type": "string"}, "dataBase64": {"type": "string"}}}}}),
            output_schema: json!({"type": "object", "required": ["annotation"], "properties": {"annotation": {"type": "object", "required": ["parent", "by", "method", "at"]}}}),
            effects: "none".into(),
            consequential: false,
            authority: vec!["vision:read".into()],
            preconditions: vec!["the host accepts sending the image to the named model and endpoint".into()],
            guarantees: vec!["every element and fact returned is marked inferred and attributed".into()],
            limitations: vec![
                "non-deterministic; can misread; confidence is self-reported and uncalibrated".into(),
                "model-reported boxes are unreliable: in a live run (docs/GATEWAY.md) gpt-4o-mini placed every row about 85 px (84-86) above its real position at confidence 1.0; boxes are kept only as inference".into(),
            ],
            constraints: m.constraints.clone(),
            errors: vec!["invalid_input".into(), "unauthorized".into(), "upstream_error".into(), "unreadable_output".into()],
        }];
        ModelReader {
            manifest: m,
            model: model.into(),
            endpoint: endpoint.into(),
            transport: RefCell::new(transport),
        }
    }
}

fn digest(v: &Value) -> String {
    xeet_artifact::canon::sha256_id(xeet_artifact::canon::canonical(v).as_bytes())
}

/// Turns the model's JSON reading into an annotation of inferred elements.
pub fn to_annotation(
    reading: &Value,
    parent: &str,
    w: u32,
    h: u32,
    by: &str,
    method: &str,
    at: &str,
) -> Result<Value, String> {
    let items = reading["items"]
        .as_array()
        .ok_or("the reading has no items array")?;
    let mut elements = vec![];
    let mut children = vec![];
    let mut dropped = 0;
    for (i, it) in items.iter().enumerate() {
        let text = it["text"].as_str().unwrap_or("").to_string();
        if text.trim().is_empty() {
            continue;
        }
        let k = it["kind"].as_str().unwrap_or("other");
        let kind = match k {
            "heading" => "heading",
            "table-row" => "group",
            "label" => "label",
            _ => "text",
        };
        let id = format!("reading.{i}");
        let mut e = json!({"id": id, "kind": kind, "role": format!("vision.{k}"), "text": text, "visible": true, "basis": "inferred", "attrs": {}});
        if let Some(c) = it["confidence"].as_f64() {
            e["attrs"]["vision.confidence"] = json!((c.clamp(0.0, 1.0) * 100.0).round() / 100.0);
        }
        if let Some(u) = it["uncertain"].as_str().filter(|u| !u.trim().is_empty()) {
            e["attrs"]["vision.uncertain"] = json!(u);
        }
        if let Some(b) = it["box"].as_array().filter(|b| b.len() == 4) {
            let n: Vec<f64> = b.iter().filter_map(Value::as_f64).collect();
            let inside = n.len() == 4
                && n[0] >= 0.0
                && n[1] >= 0.0
                && n[2] > 0.0
                && n[3] > 0.0
                && n[0] + n[2] <= w as f64 + 0.5
                && n[1] + n[3] <= h as f64 + 0.5;
            if inside {
                e["bounds"] = json!([{"view": "v0", "x": n[0].round(), "y": n[1].round(), "w": n[2].round(), "h": n[3].round()}]);
            } else {
                dropped += 1;
                e["attrs"]["vision.boxDropped"] = json!("the model's box fell outside the image");
            }
        }
        children.push(json!(id));
        elements.push(e);
    }
    let summary = reading["summary"].as_str().unwrap_or("").to_string();
    let mut root = json!({"id": "reading", "kind": "list", "role": "vision.reading", "text": summary, "visible": true, "basis": "inferred", "children": children, "attrs": {}});
    if dropped > 0 {
        root["attrs"]["vision.boxesDropped"] = json!(dropped);
    }
    let mut all = vec![root];
    all.extend(elements);
    Ok(
        json!({"parent": parent, "elements": all, "facts": [], "by": {"name": by, "version": by}, "method": method, "at": at}),
    )
}

impl<T: Transport> Provider for ModelReader<T> {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn invoke(&self, cap: &str, input: &Value, session: &Session) -> Outcome {
        let c = &self.manifest.capabilities[0];
        let mut out = Outcome {
            capability: cap.into(),
            version: c.version.clone(),
            provider: self.manifest.provider.id.clone(),
            provider_version: self.manifest.provider.version.clone(),
            ok: false,
            result: None,
            error: None,
            input_digest: digest(input),
            result_digest: None,
            artifacts: vec![],
            authority_checked: c.authority.clone(),
        };
        let fail = |code: &str, m: String| {
            Some(xeet_engine::Error {
                code: code.into(),
                message: m,
                details: Value::Null,
            })
        };
        if cap != c.id {
            out.error = fail("not_found", cap.into());
            return out;
        }
        if !schema::validate(&c.input_schema, input).is_empty() {
            out.error = fail("invalid_input", "input does not match the contract".into());
            return out;
        }
        if !session.grants.contains("vision:read") {
            out.error = fail("unauthorized", "needs vision:read".into());
            return out;
        }
        let mt = input["image"]["mediaType"].as_str().unwrap_or("image/png");
        let data = input["image"]["dataBase64"].as_str().unwrap_or("");
        let (w, h) = xeet_engine::unb64(data)
            .and_then(|b| xeet_capture::image::sniff(&b))
            .map(|s| (s.width, s.height))
            .unwrap_or((0, 0));
        let prompt = PROMPT.replace("WIDTHxHEIGHT", &format!("{w}x{h}"));
        let body = json!({
            "model": self.model, "temperature": 0, "max_tokens": 2000,
            "response_format": {"type": "json_object"},
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": format!("data:{mt};base64,{data}")}}]}],
        });
        let (status, text) = match self
            .transport
            .borrow_mut()
            .post_json(&self.endpoint, &body.to_string())
        {
            Ok(r) => r,
            Err(e) => {
                out.error = fail("upstream_error", format!("transport: {e}"));
                return out;
            }
        };
        if !(200..300).contains(&status) {
            out.error = fail(
                "upstream_error",
                format!(
                    "HTTP {status}: {}",
                    text.chars().take(300).collect::<String>()
                ),
            );
            return out;
        }
        let content = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v["choices"][0]["message"]["content"]
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let json_part = content
            .find('{')
            .and_then(|a| content.rfind('}').map(|b| &content[a..=b]))
            .unwrap_or("");
        let Ok(reading) = serde_json::from_str::<Value>(json_part) else {
            out.error = fail(
                "unreadable_output",
                format!(
                    "the model did not return JSON: {}",
                    content.chars().take(200).collect::<String>()
                ),
            );
            return out;
        };
        let method = format!(
            "{} via {}; prompt {}; temperature 0",
            self.model,
            self.endpoint,
            xeet_artifact::canon::sha256_id(PROMPT.as_bytes())
        );
        let at = session.now.clone().unwrap_or_default();
        match to_annotation(
            &reading,
            input["parent"].as_str().unwrap_or("image"),
            w,
            h,
            &self.model,
            &method,
            &at,
        ) {
            Ok(a) => {
                let r = json!({"annotation": a});
                out.result_digest = Some(digest(&r));
                out.result = Some(r);
                out.ok = true;
            }
            Err(e) => out.error = fail("unreadable_output", e),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_becomes_inferred_elements_and_bad_boxes_are_dropped() {
        let reading = json!({"summary": "a table", "items": [
            {"text": "Invoices", "kind": "heading", "box": [16, 44, 80, 24], "confidence": 0.99, "uncertain": null},
            {"text": "INV-1045 · Due 2026-10-20 · Overd", "kind": "table-row", "box": [16, 306, 900, 34], "confidence": 0.6, "uncertain": "status cut off"},
            {"text": "  ", "kind": "text"}]});
        let a = to_annotation(
            &reading,
            "image",
            480,
            356,
            "m",
            "how",
            "2026-11-16T00:00:00Z",
        )
        .unwrap();
        let els = a["elements"].as_array().unwrap();
        assert_eq!(els.len(), 3, "empty items skipped");
        assert!(els.iter().all(|e| e["basis"] == "inferred"));
        assert_eq!(els[1]["bounds"][0]["w"], 80.0);
        assert!(els[2].get("bounds").is_none());
        assert_eq!(els[2]["attrs"]["vision.uncertain"], "status cut off");
        assert_eq!(els[0]["attrs"]["vision.boxesDropped"], 1);
    }
}
