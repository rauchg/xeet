//! `xeet gateway …`: the modality gateway from the command line.
//!
//! `prepare` builds the model request a target model can consume and prints
//! it with the decision record (media elided to sizes). `run` also sends it
//! through an OpenAI-compatible client: the OpenAI API or Vercel AI Gateway,
//! over curl, with the key read from the environment and passed to curl on
//! stdin, never on the command line.
//!
//! Providers: this binary's engine, plus — with `--reader ai-gateway:MODEL` —
//! a vision reader backed by that model. The reader is invoked only when the
//! host also grants `vision:read`. Without a reader, images are represented by
//! what can be proven (format, dimensions, digest) and their content is
//! declared unknown.

use super::*;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use xeet_engine::{store, Engine, Provider, Session};
use xeet_gateway::gateway::{Gateway, Task};
use xeet_gateway::inspect::Input;
use xeet_gateway::model::ModelProfile;
use xeet_gateway::request::Content;

pub const USAGE: &str = "\
gateway prepare --model PROFILE.json|ai-gateway:MODEL [--host-config PROFILE.json]
                --instruction TEXT [--attach PATH,…] [--artifact ID,…]
                [--reader ai-gateway:MODEL] [session options] [--record FILE]
                                         the model request a target model can consume,
                                         plus the decision record (no content in it)
gateway run … --client openai|ai-gateway [--max-tokens N]
                                         prepare and send; on a confirmed modality
                                         rejection, re-prepare once and resend
gateway profile ai-gateway:MODEL         a capability profile from AI Gateway metadata
gateway expand ARTIFACT [--focus ID] [--sections a,b] [--max-chars N] [session options]
                                         more of an artifact, for a follow-up turn
A --reader is used only if --grant includes vision:read.";

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
    "reader",
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
        ["profile", spec] => load_profile(spec).map(|p| {
            println!("{}", serde_json::to_string_pretty(&p).unwrap());
            EXIT_OK
        }),
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
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let p: ModelProfile = serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let problems = p.validate();
    if !problems.is_empty() {
        return Err(format!("{path}: {}", problems.join("; ")));
    }
    Ok(p)
}

/// `ai-gateway:MODEL` → a profile from Vercel AI Gateway's metadata
/// (provider-reported); anything else is a profile file.
fn load_profile(spec: &str) -> Result<ModelProfile, String> {
    let Some(id) = spec.strip_prefix("ai-gateway:") else {
        return read_profile(spec);
    };
    let (status, body) = Curl {
        key_env: "AI_GATEWAY_API_KEY",
    }
    .call("https://ai-gateway.vercel.sh/v1/models", None)?;
    if status != 200 {
        return Err(format!("ai-gateway /v1/models: HTTP {status}"));
    }
    let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let entry = v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["id"] == id)
        .ok_or_else(|| format!("ai-gateway lists no model {id}"))?;
    xeet_gateway::model::from_ai_gateway(entry)
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
    let spec = args
        .value("model")
        .ok_or("--model PROFILE.json or ai-gateway:MODEL is required")?;
    let mut profile = load_profile(spec)?;
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
    let reader = match args.value("reader") {
        Some(spec) => {
            let model = spec
                .strip_prefix("ai-gateway:")
                .ok_or("--reader takes ai-gateway:MODEL")?;
            Some(xeet_gateway::reader::ModelReader::new(
                model,
                xeet_gateway::openai::AI_GATEWAY_ENDPOINT,
                Curl {
                    key_env: "AI_GATEWAY_API_KEY",
                },
            ))
        }
        None => None,
    };
    let mut providers: Vec<&dyn Provider> = vec![&e];
    if let Some(r) = &reader {
        providers.push(r);
    }
    let g = Gateway::new(providers);
    let s = session(args);
    let t = now();
    if !send {
        let p = g.prepare(&task, &profile, &s, &t);
        for c in &p.request.messages[0].content {
            if let Content::Text { text } = c {
                println!("{text}\n");
            }
        }
        write_record(
            args,
            &json!({"request": redacted(&p.request), "prepared": p}),
        )?;
        return Ok(EXIT_OK);
    }
    let (endpoint, key_env) = match args
        .value("client")
        .ok_or("run needs --client (openai or ai-gateway)")?
    {
        "openai" => (xeet_gateway::openai::ENDPOINT, "OPENAI_API_KEY"),
        "ai-gateway" => (
            xeet_gateway::openai::AI_GATEWAY_ENDPOINT,
            "AI_GATEWAY_API_KEY",
        ),
        other => return Err(format!("unknown client {other}")),
    };
    let max = args
        .value("max-tokens")
        .map(|v| v.parse().map_err(|_| "--max-tokens needs a number"))
        .transpose()?;
    let mut c = xeet_gateway::openai::Client {
        transport: Curl { key_env },
        endpoint: endpoint.into(),
        max_completion_tokens: max.or(Some(400)),
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
/// configuration. The key (read from `key_env`) travels on curl's stdin
/// config, never in argv.
struct Curl {
    key_env: &'static str,
}

impl Curl {
    fn call(&self, url: &str, body: Option<&str>) -> Result<(u16, String), String> {
        use std::io::Write as _;
        let key =
            std::env::var(self.key_env).map_err(|_| format!("{} is not set", self.key_env))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let file = std::env::temp_dir().join(format!(
            "xeet-gateway-body-{}-{nanos}.json",
            std::process::id()
        ));
        let mut config = format!(
            "url = \"{url}\"\nheader = \"Authorization: Bearer {key}\"\nwrite-out = \"\\n%{{http_code}}\"\nsilent\nshow-error\nmax-time = 180\n"
        );
        if let Some(b) = body {
            std::fs::write(&file, b).map_err(|e| e.to_string())?;
            config.push_str(&format!(
                "header = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\n",
                file.display()
            ));
        }
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

impl xeet_gateway::openai::Transport for Curl {
    fn post_json(&mut self, url: &str, body: &str) -> Result<(u16, String), String> {
        self.call(url, Some(body))
    }
}
