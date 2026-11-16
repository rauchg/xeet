//! `xeet engine …`: the representation engine's transports.
//!
//! Both the CLI and the MCP server are thin layers over `xeet_engine`: the
//! contracts, discovery, authority checks and execution live there, so a new
//! transport (HTTP, AppPort, another agent protocol) adds a layer, not a
//! second definition of any capability.
//!
//! Authority: the host grants scopes with `--grant` (default `store:read`)
//! and read roots with `--allow-read`. Nothing an agent sends can widen them.

use super::*;

use serde_json::{json, Value};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use xeet_engine::contract::{self, Manifest};
use xeet_engine::discover::{self, Query};
use xeet_engine::{store, Engine, Outcome, Provider, Session};

pub const USAGE: &str = "\
engine manifest                          the provider manifest (JSON)
engine discover [--outcome TEXT] [--accepts text/plain,…] [--has MODALITY,…]
                [--source TYPE] [--peer manifest.json,…]
                                         capabilities by outcome and modality;
                                         summaries only, grants nothing
engine describe ID[,ID…] [--peer manifest.json,…]
                                         full contracts (schemas, authority, limits)
engine call ID --input JSON|@file [session options]
                                         invoke one capability; prints the outcome
engine mcp [--tools progressive|all] [session options]
                                         MCP server on stdio
session options: --store DIR (default $XEET_STORE or .xeet-store)
                 --grant store:read,store:write,fs:read (default store:read)
                 --allow-read DIR,… (roots fs:read may read inside)";

const KNOWN: &[&str] = &[
    "outcome",
    "accepts",
    "has",
    "source",
    "peer",
    "input",
    "store",
    "grant",
    "allow-read",
    "tools",
    "limit",
    "help",
    "h",
];

pub fn run(argv: &[String]) -> u8 {
    let args = match Args::parse(argv) {
        Ok(a) => a,
        Err(e) => return usage(&e),
    };
    if args.has("help") || args.has("h") || args.positional.is_empty() {
        println!("{USAGE}");
        return EXIT_OK;
    }
    let unknown = args.unknown(KNOWN);
    if !unknown.is_empty() {
        return usage(&format!("unknown flag(s): {}", unknown.join(", ")));
    }
    let pos: Vec<&str> = args.positional.iter().map(String::as_str).collect();
    let result = match pos.as_slice() {
        ["manifest"] => {
            println!(
                "{}",
                xeet_artifact::canon::pretty(&serde_json::to_value(contract::manifest()).unwrap())
            );
            Ok(EXIT_OK)
        }
        ["discover"] => manifests(&args).map(|ms| {
            let q = query(|k| args.value(k).map(str::to_string));
            println!(
                "{}",
                serde_json::to_string_pretty(&discover::discover(&ms, &q)).unwrap()
            );
            EXIT_OK
        }),
        ["describe", ids] => manifests(&args).map(|ms| {
            let ids: Vec<String> = ids.split(',').map(str::to_string).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&discover::describe(&ms, &ids)).unwrap()
            );
            EXIT_OK
        }),
        ["call", id] => call(&args, id),
        ["mcp"] => mcp(&args),
        _ => return usage("unknown engine subcommand"),
    };
    match result {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xeet engine: {e}");
            EXIT_INPUT
        }
    }
}

fn usage(msg: &str) -> u8 {
    eprintln!("xeet engine: {msg}\n{USAGE}");
    EXIT_USAGE
}

fn list(v: Option<String>) -> Vec<String> {
    v.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

fn query(get: impl Fn(&str) -> Option<String>) -> Query {
    Query {
        outcome: get("outcome"),
        accepts: list(get("accepts")),
        has: list(get("has")),
        source: get("source"),
        limit: get("limit").and_then(|l| l.parse().ok()),
    }
}

/// This provider's manifest plus any peers' (other providers' manifests).
fn manifests(args: &Args) -> Result<Vec<Manifest>, String> {
    let mut out = vec![contract::manifest()];
    for path in list(args.value("peer").map(str::to_string)) {
        let text = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
        let m: Manifest =
            serde_json::from_slice(&text).map_err(|e| format!("{path}: not a manifest: {e}"))?;
        if m.format != contract::MANIFEST_FORMAT {
            return Err(format!(
                "{path}: format {} is not {}",
                m.format,
                contract::MANIFEST_FORMAT
            ));
        }
        out.push(m);
    }
    Ok(out)
}

fn session(args: &Args) -> Session {
    let grants = args
        .value("grant")
        .map(str::to_string)
        .unwrap_or_else(|| "store:read".into());
    Session {
        grants: list(Some(grants)).into_iter().collect(),
        read_roots: list(args.value("allow-read").map(str::to_string))
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        now: Some(super::artifact::iso8601(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        )),
    }
}

fn engine(args: &Args) -> Result<Engine, String> {
    let root = args
        .value("store")
        .map(str::to_string)
        .or_else(|| std::env::var("XEET_STORE").ok())
        .unwrap_or_else(|| ".xeet-store".into());
    Ok(Engine::new(store::Store::open(Path::new(&root))?))
}

fn call(args: &Args, id: &str) -> Result<u8, String> {
    let raw = args.value("input").unwrap_or("{}");
    let text = match raw.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
        None => raw.to_string(),
    };
    let input: Value = serde_json::from_str(&text).map_err(|e| format!("--input: {e}"))?;
    let e = engine(args)?;
    let out = e.invoke(id, &input, &session(args));
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(if out.ok { EXIT_OK } else { EXIT_INPUT })
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

fn tool_name(capability: &str) -> String {
    capability.replace('.', "_")
}

fn meta_tools() -> Value {
    json!([
        {"name": "engine_discover", "title": "Find a capability by outcome",
         "description": "Describe what you want to achieve and which modalities you can consume; get matching capabilities (summaries only), the mode to use, and an honest 'not provided' when this provider cannot do it. Grants nothing.",
         "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
            "outcome": {"type": "string", "description": "e.g. 'understand a screenshot without vision'"},
            "accepts": {"type": "array", "items": {"type": "string"}, "description": "modalities you can take in, e.g. [\"text/plain\"]"},
            "has": {"type": "array", "items": {"type": "string"}},
            "source": {"type": "string"}, "limit": {"type": "integer", "minimum": 1}}},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "engine_describe", "title": "Get full capability contracts",
         "description": "Input/output schemas, modes, effects, authority, preconditions, guarantees, limitations and constraints for the named capabilities.",
         "inputSchema": {"type": "object", "additionalProperties": false, "required": ["capabilities"], "properties": {
            "capabilities": {"type": "array", "items": {"type": "string"}}}},
         "annotations": {"readOnlyHint": true, "openWorldHint": false}},
        {"name": "engine_invoke", "title": "Invoke a capability",
         "description": "Run one capability by id with input matching its contract. The session's authority is checked per call; errors name missing scopes or the exact invalid fields. Returns a verifiable outcome (input and result digests).",
         "inputSchema": {"type": "object", "additionalProperties": false, "required": ["capability"], "properties": {
            "capability": {"type": "string"}, "input": {"type": "object"},
            "provider": {"type": "string", "description": "optional: which provider, when several implement the capability"}}},
         "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}}
    ])
}

fn all_tools(m: &Manifest) -> Value {
    Value::Array(
        m.capabilities
            .iter()
            .map(|c| {
                json!({
                    "name": tool_name(&c.id),
                    "title": c.title,
                    "description": format!("{} Needs: {}. Effects: {}.", c.summary, c.authority.join(", "), c.effects),
                    "inputSchema": c.input_schema,
                    "annotations": {"readOnlyHint": c.effects == "none", "destructiveHint": false, "openWorldHint": false},
                })
            })
            .collect(),
    )
}

/// Renders an outcome as MCP content. Text first; images only when the
/// caller asked for pixels (render, or understand with consumer=vision).
fn content(out: &Outcome) -> Value {
    let mut content = Vec::new();
    let mut structured = serde_json::to_value(out).unwrap();
    if let Some(r) = &out.result {
        if let (Some(data), Some(mt)) = (
            r.get("data").and_then(Value::as_str),
            r.get("mediaType").and_then(Value::as_str),
        ) {
            content.push(json!({"type": "image", "mimeType": mt, "data": data}));
            structured["result"]["data"] = json!("(in the image content block)");
        }
        if let Some(images) = r.get("images").and_then(Value::as_array) {
            for (i, img) in images.iter().enumerate() {
                content.push(
                    json!({"type": "image", "mimeType": img["mediaType"], "data": img["data"]}),
                );
                structured["result"]["images"][i]["data"] = json!("(in an image content block)");
            }
        }
        // Content prepared for a model (modality.adapt): its text parts, as text.
        for part in r
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                content.push(json!({"type": "text", "text": text}));
            }
        }
        if let Some(Value::String(text)) = r.get("projection") {
            content.push(json!({"type": "text", "text": text}));
        }
    }
    let summary = serde_json::to_string_pretty(&structured).unwrap();
    content.push(json!({"type": "text", "text": summary}));
    json!({"content": content, "structuredContent": structured, "isError": !out.ok})
}

fn resource_read(e: &Engine, s: &Session, uri: &str) -> Result<Value, String> {
    let id = store::parse_id(uri).ok_or_else(|| format!("not an artifact URI: {uri}"))?;
    let rest = uri
        .strip_prefix("artifact://sha256/")
        .and_then(|r| r.split_once('/'))
        .map(|(_, x)| x)
        .unwrap_or("");
    let fail = |o: &Outcome| {
        o.error
            .as_ref()
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_default()
    };
    if rest.is_empty() {
        let o = e.invoke("representation.inspect", &json!({"artifact": id}), s);
        let doc = o.result.as_ref().ok_or_else(|| fail(&o))?;
        return Ok(
            json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": xeet_artifact::canon::pretty(&doc["document"])}]}),
        );
    }
    if let Some(consumer) = rest.strip_prefix("projection/") {
        let o = e.invoke(
            "representation.understand",
            &json!({"artifact": id, "consumer": consumer}),
            s,
        );
        let r = o.result.as_ref().ok_or_else(|| fail(&o))?;
        return Ok(
            json!({"contents": [{"uri": uri, "mimeType": "text/plain", "text": r["projection"]}]}),
        );
    }
    if let Some(view) = rest.strip_prefix("view/") {
        let o = e.invoke(
            "representation.render",
            &json!({"artifact": id, "view": view}),
            s,
        );
        let r = o.result.as_ref().ok_or_else(|| fail(&o))?;
        return Ok(
            json!({"contents": [{"uri": uri, "mimeType": r["mediaType"], "blob": r["data"]}]}),
        );
    }
    Err(format!("unknown resource path: {rest}"))
}

fn mcp(args: &Args) -> Result<u8, String> {
    let e = engine(args)?;
    let s = session(args);
    // The engine and the modality gateway (over the engine) are served side
    // by side; both are reached only through their contracts.
    let gw = xeet_gateway::provider::GatewayProvider::new(vec![&e as &dyn Provider]);
    let providers: Vec<&dyn Provider> = vec![&e, &gw];
    let mut peers = manifests(args)?;
    peers.insert(1, gw.manifest().clone());
    let route = |cap: &str, provider: Option<&str>| -> Option<&dyn Provider> {
        providers.iter().copied().find(|p| {
            provider.is_none_or(|id| p.manifest().provider.id == id)
                && p.manifest().capabilities.iter().any(|c| c.id == cap)
        })
    };
    let progressive = args.value("tools").unwrap_or("progressive") != "all";
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(err) => {
                writeln!(stdout, "{}", json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": err.to_string()}})).map_err(|e| e.to_string())?;
                continue;
            }
        };
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let m = e.manifest();
        let result: Result<Value, Value> = match msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}, "resources": {}},
                "serverInfo": {"name": m.provider.id, "title": m.provider.name, "version": m.provider.version},
                "instructions": format!(
                    "{} Start with engine_discover (describe the outcome you need and the modalities you accept), then engine_describe, then engine_invoke. Text-only models: use representation.understand; no image is needed. Sources: {}. Artifacts are resources at artifact://sha256/<hex> (+ /projection/text, /view/v0). {}",
                    m.provider.description,
                    m.sources.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>().join(", "),
                    m.authority.summary
                ),
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let all = || {
                    Value::Array(
                        providers
                            .iter()
                            .flat_map(|p| {
                                all_tools(p.manifest())
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default()
                            })
                            .collect(),
                    )
                };
                Ok(json!({"tools": if progressive { meta_tools() } else { all() }}))
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let a = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match name {
                    "engine_discover" => {
                        let q: Query = serde_json::from_value(a).unwrap_or_default();
                        let d = serde_json::to_value(discover::discover(&peers, &q)).unwrap();
                        json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&d).unwrap()}], "structuredContent": d})
                    }
                    "engine_describe" => {
                        let ids: Vec<String> =
                            serde_json::from_value(a["capabilities"].clone()).unwrap_or_default();
                        let d = json!({"contracts": discover::describe(&peers, &ids)});
                        json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&d).unwrap()}], "structuredContent": d})
                    }
                    "engine_invoke" => {
                        let cap = a.get("capability").and_then(Value::as_str).unwrap_or("");
                        let input = a.get("input").cloned().unwrap_or(json!({}));
                        match route(cap, a.get("provider").and_then(Value::as_str)) {
                            Some(p) => content(&p.invoke(cap, &input, &s)),
                            // Let the engine produce its standard not_found outcome.
                            None => content(&e.invoke(cap, &input, &s)),
                        }
                    }
                    other => {
                        let cap = providers
                            .iter()
                            .flat_map(|p| p.manifest().capabilities.iter())
                            .find(|c| tool_name(&c.id) == other)
                            .map(|c| c.id.clone());
                        match cap.as_deref().and_then(|c| route(c, None).map(|p| (c, p))) {
                            Some((cap, p)) if !progressive => content(&p.invoke(cap, &a, &s)),
                            _ => {
                                json!({"content": [{"type": "text", "text": format!("unknown tool {other}; use engine_discover")}], "isError": true})
                            }
                        }
                    }
                })
            }
            "resources/list" => Ok(
                json!({"resources": e.store.list().into_iter().map(|x| json!({
                "uri": x.uri, "name": x.title, "mimeType": "application/json",
                "description": format!("{} observed {}", x.subject_type, x.captured_at)})).collect::<Vec<_>>()}),
            ),
            "resources/templates/list" => Ok(json!({"resourceTemplates": [
                {"uriTemplate": "artifact://sha256/{hex}", "name": "artifact document", "mimeType": "application/json"},
                {"uriTemplate": "artifact://sha256/{hex}/projection/{consumer}", "name": "projection for a text or vision consumer", "mimeType": "text/plain"},
                {"uriTemplate": "artifact://sha256/{hex}/view/{view}", "name": "visual view", "mimeType": "image/png"}]})),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                resource_read(&e, &s, uri).map_err(|m| json!({"code": -32002, "message": m}))
            }
            other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
        };
        let reply = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(err) => json!({"jsonrpc": "2.0", "id": id, "error": err}),
        };
        writeln!(stdout, "{reply}").map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
    Ok(EXIT_OK)
}
