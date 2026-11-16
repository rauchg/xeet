//! `xeet artifact …`: capture, inspect, verify, compare and project artifacts.
//!
//! An artifact is a directory: `artifact.json` plus content-addressed
//! `assets/`. Every subcommand except `capture` reads only that directory: no
//! network, no model. `capture` reads the source data the caller supplies; it
//! does not fetch either (use `xeet resolve` or your own GitHub client first).

use super::*;

use serde_json::Value;
use std::io::BufRead;
use std::path::Path;
use xeet_artifact::{compare, inspect, verify, Artifact, Package};
use xeet_capture::{github, x, Reproducers};

pub const USAGE: &str = "\
artifact capture x <post.json|resolve-output.json|-> -o DIR [render options]
         [--captured-at ISO8601] [--method draft|source-payload|fixture] [--url URL] [--id ID]
artifact capture github <bundle.json|-> -o DIR [--width N] [--theme light|dark] [--dpr N]
         [--captured-at ISO8601] [--method api-response|fixture]
artifact inspect DIR [--json]           human-readable account (or the document)
artifact verify DIR [--no-reproduce] [--json]
                                        integrity, consistency, and byte-for-byte
                                        reproduction of views from preserved inputs
artifact compare A B [--json]           what differs between two observations
artifact capabilities DIR [--json]      what could be done, and with whose authority
artifact project DIR CAPABILITY [--input JSON] -o OUT
                                        run a local projection (expand, retheme);
                                        refuses anything that needs outside authority
artifact mcp                            MCP server on stdio (JSON-RPC, one message
                                        per line): read-only artifact tools";

const KNOWN: &[&str] = &[
    "o",
    "out",
    "surface",
    "width",
    "theme",
    "dpr",
    "font-scale",
    "expanded",
    "captured-at",
    "method",
    "url",
    "id",
    "json",
    "no-reproduce",
    "input",
    "help",
    "h",
];

pub fn run(argv: &[String]) -> u8 {
    let args = match Args::parse(argv) {
        Ok(a) => a,
        Err(e) => return usage_error(&e),
    };
    if args.has("help") || args.has("h") || args.positional.is_empty() {
        println!("{USAGE}");
        return if args.positional.is_empty() && !args.has("help") && !args.has("h") {
            EXIT_USAGE
        } else {
            EXIT_OK
        };
    }
    let unknown = args.unknown(KNOWN);
    if !unknown.is_empty() {
        return usage_error(&format!("unknown flag(s): {}", unknown.join(", ")));
    }
    let pos: Vec<&str> = args.positional.iter().map(String::as_str).collect();
    let result = match pos.as_slice() {
        ["capture", "x", input] => capture_x(&args, input),
        ["capture", "github", input] => capture_github(&args, input),
        ["inspect", dir] => read(dir).map(|(p, a)| {
            if args.has("json") {
                println!("{}", xeet_artifact::canon::pretty(&p.document));
            } else {
                print!("{}", inspect::text(&a));
            }
            EXIT_OK
        }),
        ["verify", dir] => verify_cmd(&args, dir),
        ["compare", a, b] => read(a).and_then(|(_, a)| {
            let (_, b) = read(b)?;
            let c = compare::compare(&a, &b);
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&c).unwrap());
            } else {
                print_comparison(&c);
            }
            Ok(EXIT_OK)
        }),
        ["capabilities", dir] => read(dir).map(|(_, a)| {
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "capabilityPolicy": a.capability_policy,
                    "capabilities": a.capabilities,
                })).unwrap());
            } else {
                for c in &a.capabilities {
                    let auth = if c.authority.required.is_empty() { "none".to_string() } else { c.authority.required.join(", ") };
                    println!("{}\n  {:?} on {} via {:?} {}\n  availability {:?}; authority {auth}\n  verify: {}", c.id, c.kind, c.target, c.executor.kind, c.executor.reference, c.availability, c.verification);
                }
            }
            EXIT_OK
        }),
        ["project", dir, cap] => project(&args, dir, cap),
        ["mcp"] => mcp(),
        _ => return usage_error("unknown artifact subcommand"),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("xeet artifact: {e}");
            EXIT_INPUT
        }
    }
}

fn usage_error(msg: &str) -> u8 {
    eprintln!("xeet artifact: {msg}\n{USAGE}");
    EXIT_USAGE
}

fn read(dir: &str) -> Result<(Package, Artifact), String> {
    let p = Package::read_dir(Path::new(dir))?;
    let a = p
        .artifact()
        .map_err(|e| format!("{dir}: not a readable artifact: {e}"))?;
    Ok((p, a))
}

fn read_bytes(input: &str) -> Result<Vec<u8>, String> {
    if input == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| format!("stdin: {e}"))?;
        Ok(buf)
    } else {
        std::fs::read(input).map_err(|e| format!("{input}: {e}"))
    }
}

fn out_dir(args: &Args) -> Result<&str, String> {
    args.value("o")
        .or(args.value("out"))
        .ok_or_else(|| "-o DIR is required".to_string())
}

fn write(p: &Package, dir: &str) -> Result<u8, String> {
    p.write_dir(Path::new(dir))
        .map_err(|e| format!("{dir}: {e}"))?;
    println!("{} {}", p.document["id"].as_str().unwrap_or(""), dir);
    Ok(EXIT_OK)
}

/// Capture time: `--captured-at`, else the system clock (UTC, seconds).
fn captured_at(args: &Args) -> String {
    if let Some(t) = args.value("captured-at") {
        return t.to_string();
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso8601(secs)
}

/// Seconds since the epoch → `YYYY-MM-DDTHH:MM:SSZ` (proleptic Gregorian).
pub fn iso8601(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn capture_x(args: &Args, input: &str) -> Result<u8, String> {
    let dir = out_dir(args)?;
    let options = render_options(args)?;
    let raw = read_bytes(input)?;
    // `xeet resolve` output: the adapter keeps the envelope as an input and
    // records the extraction; a bare post is rendered as given.
    let env = x::envelope(&raw);
    let is_envelope = env.is_some();
    let (post_json, mut url, mut id, mut method) = match env {
        Some(e) => {
            let method = if e.url.is_some() {
                "source-payload"
            } else {
                "draft"
            };
            (e.post_json, e.url, e.id, method.to_string())
        }
        None => (raw.clone(), None, None, "draft".to_string()),
    };
    if let Some(u) = args.value("url") {
        url = Some(u.to_string());
    }
    if let Some(i) = args.value("id") {
        id = Some(i.to_string());
    }
    if let Some(m) = args.value("method") {
        method = m.to_string();
    }
    ensure_fonts_for_json_text(std::str::from_utf8(&post_json).unwrap_or(""))?;
    let mut extra = Vec::new();
    if is_envelope {
        extra.push((raw, x::ENVELOPE_ROLE.to_string()));
    }
    let p = x::capture(&x::Capture {
        post_json: &post_json,
        options,
        captured_at: captured_at(args),
        method,
        source_url: url,
        source_id: id,
        extra_inputs: extra,
    })?;
    write(&p, dir)
}

fn capture_github(args: &Args, input: &str) -> Result<u8, String> {
    let dir = out_dir(args)?;
    let bytes = read_bytes(input)?;
    let num = |k: &str, d: f32| -> Result<f32, String> {
        args.value(k)
            .map(|v| v.parse().map_err(|_| format!("--{k} needs a number")))
            .unwrap_or(Ok(d))
    };
    let theme = args
        .value("theme")
        .map(parse_theme)
        .transpose()?
        .unwrap_or(Theme::Light);
    let p = github::capture(&github::Capture {
        bundle_json: &bytes,
        width: num("width", 600.0)?,
        theme,
        scale: num("dpr", 2.0)?,
        captured_at: captured_at(args),
        method: args.value("method").unwrap_or("api-response").to_string(),
    })?;
    write(&p, dir)
}

fn verify_cmd(args: &Args, dir: &str) -> Result<u8, String> {
    let p = Package::read_dir(Path::new(dir))?;
    if let Ok(a) = p.artifact() {
        if let Some(input) = a
            .provenance
            .inputs
            .first()
            .and_then(|i| p.asset_bytes(&i.asset))
        {
            ensure_fonts_for_json_text(std::str::from_utf8(input).unwrap_or(""))?;
        }
    }
    let r = Reproducers;
    let v = verify::verify(
        &p,
        if args.has("no-reproduce") {
            None
        } else {
            Some(&r)
        },
    );
    if args.has("json") {
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else {
        println!(
            "{} {}",
            if v.ok { "ok  " } else { "FAIL" },
            v.artifact.as_deref().unwrap_or("(no id)")
        );
        for c in &v.checks {
            println!("  {:<13} {:?}", c.name, c.status);
            for d in &c.detail {
                println!("      {d}");
            }
        }
        if !v.requires_external_authority.is_empty() {
            println!(
                "  needs outside authority: {}",
                v.requires_external_authority.join(", ")
            );
        }
        for s in &v.cannot_establish {
            println!("  cannot establish: {s}");
        }
    }
    Ok(if v.ok { EXIT_OK } else { EXIT_INPUT })
}

fn project(args: &Args, dir: &str, cap: &str) -> Result<u8, String> {
    let out = out_dir(args)?;
    let (p, a) = read(dir)?;
    let input: Value = match args.value("input") {
        Some(s) => serde_json::from_str(s).map_err(|e| format!("--input: {e}"))?,
        None => Value::Object(Default::default()),
    };
    if a.provenance.adapter.name != x::ADAPTER {
        return Err(format!(
            "{cap}: no local executor for adapter {}",
            a.provenance.adapter.name
        ));
    }
    let child = x::project(&a, &p, cap, &input)?;
    write(&child, out)
}

fn print_comparison(c: &compare::Comparison) {
    println!("identical observation: {}", c.identical);
    println!("same subject:          {}", c.same_subject);
    println!(
        "captured at:           {} vs {}",
        c.captured_at[0], c.captured_at[1]
    );
    for (id, d) in &c.views {
        println!("view {id}: {d:?}");
    }
    for id in &c.elements_added {
        println!("+ {id}");
    }
    for id in &c.elements_removed {
        println!("- {id}");
    }
    for ch in &c.elements_changed {
        println!("~ {}.{}: {} → {}", ch.id, ch.field, ch.before, ch.after);
    }
    for ch in &c.facts_changed {
        println!("state {}.{}: {} → {}", ch.id, ch.field, ch.before, ch.after);
    }
    for id in &c.capabilities_added {
        println!("+ capability {id}");
    }
    for id in &c.capabilities_removed {
        println!("- capability {id}");
    }
}

// ---------------------------------------------------------------------------
// MCP: the same read-only operations, discoverable by agents.
// ---------------------------------------------------------------------------

fn mcp_tools() -> Value {
    let path = serde_json::json!({"type": "string", "description": "Path to an artifact directory (holds artifact.json)"});
    serde_json::json!([
        {"name": "artifact_inspect", "title": "Inspect an artifact",
         "description": "Returns the artifact document: subject, provenance, views, semantic elements with bounds, state as of capture, capabilities, limitations and coverage. Read-only, offline.",
         "inputSchema": {"type": "object", "required": ["path"], "additionalProperties": false, "properties": {"path": path}},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "artifact_verify", "title": "Verify an artifact",
         "description": "Deterministic checks: identity hash, asset hashes, view dimensions, semantic graph references, basis honesty, temporal grounding, capability authority rules, and (by default) byte-for-byte reproduction of views from the preserved inputs. Also lists what verification cannot establish.",
         "inputSchema": {"type": "object", "required": ["path"], "additionalProperties": false, "properties": {"path": path, "reproduce": {"type": "boolean", "default": true}}},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "artifact_compare", "title": "Compare two artifacts",
         "description": "What differs between two observations: views (by hash), elements, state facts, capabilities. Identical pixels do not imply identical meaning; both are reported.",
         "inputSchema": {"type": "object", "required": ["a", "b"], "additionalProperties": false, "properties": {"a": path, "b": path}},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "artifact_capabilities", "title": "List capabilities",
         "description": "What could be done with the observed object, with input schemas, executors, required authority and verification methods. Descriptions only: this server executes none of them.",
         "inputSchema": {"type": "object", "required": ["path"], "additionalProperties": false, "properties": {"path": path}},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "artifact_view", "title": "Show a view",
         "description": "Returns one visual projection (PNG) of the artifact as an image, with its view metadata.",
         "inputSchema": {"type": "object", "required": ["path"], "additionalProperties": false, "properties": {"path": path, "view": {"type": "string", "default": "v0"}}},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}}
    ])
}

fn mcp_call(name: &str, a: &Value) -> Result<Value, String> {
    let arg = |k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing `{k}`"))
    };
    let text = |v: &Value| serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(v).unwrap()}], "structuredContent": v});
    match name {
        "artifact_inspect" => {
            let (p, _) = read(arg("path")?)?;
            Ok(text(&p.document))
        }
        "artifact_verify" => {
            let p = Package::read_dir(Path::new(arg("path")?))?;
            let reproduce = a.get("reproduce").and_then(Value::as_bool).unwrap_or(true);
            let r = Reproducers;
            let v = verify::verify(&p, if reproduce { Some(&r) } else { None });
            Ok(text(&serde_json::to_value(v).unwrap()))
        }
        "artifact_compare" => {
            let (_, x) = read(arg("a")?)?;
            let (_, y) = read(arg("b")?)?;
            Ok(text(
                &serde_json::to_value(compare::compare(&x, &y)).unwrap(),
            ))
        }
        "artifact_capabilities" => {
            let (_, x) = read(arg("path")?)?;
            Ok(text(
                &serde_json::json!({"capabilityPolicy": x.capability_policy, "capabilities": x.capabilities}),
            ))
        }
        "artifact_view" => {
            let (p, x) = read(arg("path")?)?;
            let id = a.get("view").and_then(Value::as_str).unwrap_or("v0");
            let view = x
                .views
                .iter()
                .find(|v| v.id == id)
                .ok_or_else(|| format!("no view {id}"))?;
            let bytes = p.asset_bytes(&view.asset).ok_or("view asset missing")?;
            let meta = serde_json::to_value(view).unwrap();
            Ok(serde_json::json!({"content": [
                {"type": "image", "mimeType": view.media_type, "data": xeet_resolve::b64(bytes)},
                {"type": "text", "text": serde_json::to_string(&meta).unwrap()}
            ], "structuredContent": meta}))
        }
        other => Err(format!("unknown tool {other}")),
    }
}

fn mcp() -> Result<u8, String> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}});
                writeln!(stdout, "{err}").map_err(|e| e.to_string())?;
                continue;
            }
        };
        let Some(id) = msg.get("id").cloned() else {
            continue;
        }; // notification
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match msg.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(serde_json::json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "xeet-artifact", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Artifacts are historical observations. Capabilities are descriptions, never grants; this server executes none of them."
            })),
            "ping" => Ok(serde_json::json!({})),
            "tools/list" => Ok(serde_json::json!({"tools": mcp_tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                Ok(mcp_call(name, &args).unwrap_or_else(|e| {
                    serde_json::json!({"content": [{"type": "text", "text": e}], "isError": true})
                }))
            }
            m => Err(
                serde_json::json!({"code": -32601, "message": format!("method not found: {m}")}),
            ),
        };
        let reply = match result {
            Ok(r) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => serde_json::json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        writeln!(stdout, "{reply}").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
    Ok(EXIT_OK)
}
