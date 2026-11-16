//! `xeet gateway …`: the modality gateway from the command line.
//!
//! `prepare` builds the model request a target model can consume and prints
//! it with the decision record (media elided to sizes). `run` also sends it
//! through a model client — today `openai`, over curl, with the key read from
//! `OPENAI_API_KEY` and passed to curl on stdin, never on the command line.
//!
//! Providers here: this binary's engine. A vision provider is not built in;
//! without one, images are represented by what can be proven (format,
//! dimensions, digest) and their content is declared unknown.

use super::*;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use xeet_engine::{store, Engine, Provider, Session};
use xeet_gateway::gateway::{Gateway, Task};
use xeet_gateway::inspect::Input;
use xeet_gateway::model::ModelProfile;
use xeet_gateway::request::Content;

pub const USAGE: &str = "\
gateway prepare --model PROFILE.json [--host-config PROFILE.json] --instruction TEXT
                [--attach PATH,…] [--artifact ID,…] [session options] [--record FILE]
                                         the model request a target model can consume,
                                         plus the decision record (no content in it)
gateway run … --client openai [--max-tokens N]
                                         prepare and send; on a confirmed modality
                                         rejection, re-prepare once and resend
gateway expand ARTIFACT [--focus ID] [--sections a,b] [--max-chars N] [session options]
                                         more of an artifact, for a follow-up turn";

const KNOWN: &[&str] = &[
    "model",
    "host-config",
    "instruction",
    "attach",
    "artifact",
    "store",
    "grant",
    "allow-read",
    "record",
    "client",
    "max-tokens",
    "focus",
    "sections",
    "max-chars",
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
    let r = match pos.as_slice() {
        ["prepare"] => go(&args, false),
        ["run"] => go(&args, true),
        ["expand", artifact] => expand(&args, artifact),
        _ => return usage("unknown gateway subcommand"),
    };
    match r {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xeet gateway: {e}");
            EXIT_INPUT
        }
    }
}

fn usage(msg: &str) -> u8 {
    eprintln!("xeet gateway: {msg}\n{USAGE}");
    EXIT_USAGE
}

fn list(v: Option<&str>) -> Vec<String> {
    v.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

fn now() -> String {
    super::artifact::iso8601(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

fn session(args: &Args) -> Session {
    Session {
        grants: list(Some(args.value("grant").unwrap_or("store:read")))
            .into_iter()
            .collect(),
        read_roots: list(args.value("allow-read"))
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        now: Some(now()),
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

fn read_profile(path: &str) -> Result<ModelProfile, String> {
    let p: ModelProfile =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("{path}: {e}"))?)
            .map_err(|e| format!("{path}: {e}"))?;
    let problems = p.validate();
    if !problems.is_empty() {
        return Err(format!("{path}: {}", problems.join("; ")));
    }
    Ok(p)
}

/// The request with media replaced by their sizes, for printing.
fn redacted(req: &xeet_gateway::ModelRequest) -> Value {
    let mut v = serde_json::to_value(req).unwrap();
    for m in v["messages"].as_array_mut().into_iter().flatten() {
        for c in m["content"].as_array_mut().into_iter().flatten() {
            if let Some(d) = c.get("dataBase64").and_then(Value::as_str) {
                c["dataBase64"] = json!(format!("({} base64 chars elided)", d.len()));
            }
        }
    }
    v
}

fn go(args: &Args, send: bool) -> Result<u8, String> {
    let mut profile = read_profile(
        args.value("model")
            .ok_or("--model PROFILE.json is required")?,
    )?;
    if let Some(h) = args.value("host-config") {
        profile = profile.with_host_config(&read_profile(h)?);
    }
    let instruction = args
        .value("instruction")
        .ok_or("--instruction is required")?
        .to_string();
    let mut inputs: Vec<Input> = list(args.value("attach"))
        .into_iter()
        .map(|p| Input::Path(PathBuf::from(p)))
        .collect();
    inputs.extend(
        list(args.value("artifact"))
            .into_iter()
            .map(Input::Artifact),
    );
    let task = Task {
        instruction,
        inputs,
    };
    let e = engine(args)?;
    let g = Gateway::new(vec![&e as &dyn Provider]);
    let s = session(args);
    let t = now();
    if !send {
        let p = g.prepare(&task, &profile, &s, &t);
        for c in &p.request.messages[0].content {
            if let Content::Text { text } = c {
                println!("{text}\n");
            }
        }
        let out = json!({"request": redacted(&p.request), "prepared": p});
        write_record(args, &out)?;
        return Ok(EXIT_OK);
    }
    let client = args.value("client").ok_or("run needs --client (openai)")?;
    if client != "openai" {
        return Err(format!("unknown client {client}"));
    }
    let max = args
        .value("max-tokens")
        .map(|v| v.parse().map_err(|_| "--max-tokens needs a number"))
        .transpose()?;
    let mut c = xeet_gateway::openai::Client {
        transport: Curl,
        max_completion_tokens: max.or(Some(300)),
    };
    let run = g.run(&task, &profile, &mut c, &s, &t);
    match &run.response {
        Some(r) => println!("{}", r.text),
        None => eprintln!(
            "xeet gateway: not accepted: {}",
            run.record
                .attempts
                .last()
                .map(|a| format!(
                    "HTTP {:?} {:?}: {}",
                    a.status,
                    a.classification.as_ref().map(|c| c.kind),
                    a.error_excerpt.clone().unwrap_or_default()
                ))
                .unwrap_or_default()
        ),
    }
    write_record(args, &serde_json::to_value(&run.record).unwrap())?;
    Ok(if run.response.is_some() {
        EXIT_OK
    } else {
        EXIT_RENDER
    })
}

fn write_record(args: &Args, v: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(v).unwrap();
    match args.value("record") {
        Some(path) => std::fs::write(path, text).map_err(|e| format!("{path}: {e}")),
        None => {
            eprintln!("{text}");
            Ok(())
        }
    }
}

fn expand(args: &Args, artifact: &str) -> Result<u8, String> {
    let e = engine(args)?;
    let g = Gateway::new(vec![&e as &dyn Provider]);
    let sections = list(args.value("sections"));
    let sections: Vec<&str> = sections.iter().map(String::as_str).collect();
    let max = args
        .value("max-chars")
        .map(|v| v.parse().map_err(|_| "--max-chars needs a number"))
        .transpose()?;
    let text = g
        .expand(
            artifact,
            args.value("focus"),
            &sections,
            max,
            &session(args),
        )
        .map_err(|e| e.to_string())?;
    print!("{text}");
    Ok(EXIT_OK)
}

/// HTTPS through the system `curl`, which honors this host's proxy and CA
/// configuration. The key travels on curl's stdin config, not in argv.
struct Curl;

impl xeet_gateway::openai::Transport for Curl {
    fn post_json(&mut self, url: &str, body: &str) -> Result<(u16, String), String> {
        use std::io::Write as _;
        let key =
            std::env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY is not set".to_string())?;
        let file =
            std::env::temp_dir().join(format!("xeet-gateway-body-{}.json", std::process::id()));
        std::fs::write(&file, body).map_err(|e| e.to_string())?;
        let config = format!(
            "url = \"{url}\"\nheader = \"Authorization: Bearer {key}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\nwrite-out = \"\\n%{{http_code}}\"\nsilent\nshow-error\nmax-time = 120\n",
            file.display()
        );
        let mut child = std::process::Command::new("curl")
            .args(["-K", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl: {e}"))?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(config.as_bytes())
            .map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&file);
        if !out.status.success() {
            return Err(format!(
                "curl: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, code) = text.rsplit_once('\n').ok_or("no status from curl")?;
        Ok((
            code.trim().parse().map_err(|_| "bad status from curl")?,
            body.to_string(),
        ))
    }
}
