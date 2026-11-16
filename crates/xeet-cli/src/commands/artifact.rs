//! `xeet artifact …`: capture, inspect, verify, compare and project artifacts.
//!
//! An artifact is a directory: `artifact.json` plus content-addressed
//! `assets/`. Every subcommand except `capture` reads only that directory: no
//! network, no model. `capture` reads the source data the caller supplies; it
//! does not fetch either (use `xeet resolve` or your own GitHub client first).

use super::*;

use serde_json::Value;
use std::path::Path;
use xeet_artifact::{annotate, compare, inspect, project, verify, Artifact, Package};
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
artifact project DIR [--consumer text|vision] [--format text|json] [--focus ID]
         [--depth N] [--sections elements,layout,relations,state,capabilities,
         limitations,consumers] [--max-text N] [--max-chars N] [--compact-capabilities]
                                        a deterministic projection for a text-only or
                                        vision consumer; no image needed to read it
artifact invoke DIR CAPABILITY [--input JSON] -o OUT
                                        run a local capability (expand, retheme);
                                        refuses anything that needs outside authority
artifact annotate DIR ANNOTATION.json -o OUT
                                        add model-read content as declared inference,
                                        in a new artifact derived from DIR
artifact mcp                            moved to `xeet engine mcp`";

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
    "consumer",
    "format",
    "focus",
    "depth",
    "sections",
    "max-text",
    "max-chars",
    "compact-capabilities",
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
        ["invoke", dir, cap] => invoke(&args, dir, cap),
        ["project", dir] => project_cmd(&args, dir),
        ["annotate", dir, file] => annotate_cmd(&args, dir, file),
        ["mcp"] => Err("moved: the MCP server is `xeet engine mcp` (see `xeet engine --help`)".to_string()),
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

fn invoke(args: &Args, dir: &str, cap: &str) -> Result<u8, String> {
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
    let child = x::invoke(&a, &p, cap, &input)?;
    write(&child, out)
}

/// Builds a projection request from flags (CLI) or arguments (MCP).
pub fn projection_request(
    get: impl Fn(&str) -> Option<String>,
) -> Result<(project::Request, project::Form), String> {
    fn parse<T: serde::de::DeserializeOwned>(k: &str, v: String) -> Result<T, String> {
        serde_json::from_value(Value::String(v.clone()))
            .map_err(|_| format!("--{k}: unknown value `{v}`"))
    }
    let num = |k: &str| -> Result<Option<usize>, String> {
        get(k)
            .map(|v| v.parse().map_err(|_| format!("--{k} needs a whole number")))
            .transpose()
    };
    let mut req = project::Request::default();
    if let Some(c) = get("consumer") {
        req.consumer = parse("consumer", c)?;
    }
    if let Some(list) = get("sections") {
        for s in list.split(',').filter(|s| !s.is_empty()) {
            req.sections.push(parse("sections", s.trim().to_string())?);
        }
    }
    req.focus = get("focus");
    req.depth = num("depth")?;
    req.max_text = num("max-text")?;
    req.max_chars = num("max-chars")?;
    req.compact_capabilities = get("compact-capabilities").is_some_and(|v| v != "false");
    let form = match get("format").as_deref() {
        None | Some("text") => project::Form::Text,
        Some("json") => project::Form::Json,
        Some(other) => return Err(format!("--format: unknown value `{other}`")),
    };
    Ok((req, form))
}

fn project_cmd(args: &Args, dir: &str) -> Result<u8, String> {
    let (_, a) = read(dir)?;
    let (req, form) = projection_request(|k| args.value(k).map(str::to_string))?;
    print!("{}", project::render(&a, &req, form)?);
    Ok(EXIT_OK)
}

fn annotate_cmd(args: &Args, dir: &str, file: &str) -> Result<u8, String> {
    let out = out_dir(args)?;
    let p = Package::read_dir(Path::new(dir))?;
    let ann: annotate::Annotation =
        serde_json::from_slice(&read_bytes(file)?).map_err(|e| format!("{file}: {e}"))?;
    let child = annotate::annotate(&p, &ann)?;
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
