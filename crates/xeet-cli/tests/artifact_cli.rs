//! `xeet artifact`: the CLI and MCP surfaces over the artifact format.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_xeet");

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("xeet-artifact-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN).args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn capture_verify_inspect_compare_project() {
    let post = root().join("tests/fixtures/posts/links-card.json");
    let pr = root().join("tests/fixtures/github/pr-open-reviewed.json");
    let (a, b, c) = (tmp("a"), tmp("b"), tmp("c"));
    let t = "--captured-at=2026-11-16T12:00:00Z";

    let (code, out, err) = run(&[
        "artifact",
        "capture",
        "x",
        post.to_str().unwrap(),
        "-o",
        a.to_str().unwrap(),
        t,
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("sha256:"));
    let (code, out, _) = run(&["artifact", "verify", a.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("reproduction  Pass"), "{out}");

    let (code, _, err) = run(&[
        "artifact",
        "capture",
        "github",
        pr.to_str().unwrap(),
        "-o",
        b.to_str().unwrap(),
        t,
        "--method",
        "fixture",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = run(&["artifact", "verify", b.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert!(v["requiresExternalAuthority"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "merge"));

    let (code, out, _) = run(&["artifact", "inspect", b.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.contains("github.pull_request") && out.contains("nothing here is current state"));

    // A remote action cannot be run from an artifact; a local projection can.
    let (code, _, err) = run(&[
        "artifact",
        "invoke",
        a.to_str().unwrap(),
        "like",
        "-o",
        c.to_str().unwrap(),
    ]);
    assert_ne!(code, 0);
    assert!(
        err.contains("capability") || err.contains("authority"),
        "{err}"
    );
    let (code, _, err) = run(&[
        "artifact",
        "invoke",
        a.to_str().unwrap(),
        "retheme",
        "--input",
        r#"{"theme":"dark"}"#,
        "-o",
        c.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, _) = run(&["artifact", "verify", c.to_str().unwrap()]);
    assert_eq!(code, 0);
    let (_, out, _) = run(&[
        "artifact",
        "compare",
        a.to_str().unwrap(),
        c.to_str().unwrap(),
        "--json",
    ]);
    let cmp: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(cmp["views"]["v0"], "different");
    // Another projection of the same observation: new pixels, same meaning.
    assert_eq!(cmp["elementsChanged"], serde_json::json!([]), "{cmp}");
    assert_eq!(cmp["factsChanged"], serde_json::json!([]), "{cmp}");

    // Tampering with the document fails verification with exit 3.
    let doc = a.join("artifact.json");
    let text =
        std::fs::read_to_string(&doc)
            .unwrap()
            .replacen("Guillermo Rauch", "Someone Else", 1);
    std::fs::write(&doc, text).unwrap();
    let (code, out, _) = run(&["artifact", "verify", a.to_str().unwrap()]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("identity      Fail"));

    for d in [a, b, c] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn text_only_and_vision_consumers_and_annotation() {
    let x_post = root().join("docs/artifacts/x-post");
    let x_post = x_post.to_str().unwrap();

    // A text-only consumer: no image, everything else.
    let (code, out, err) = run(&["artifact", "project", x_post]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("image not included")
            && out.contains("LAYOUT")
            && out.contains("[visual-only]")
    );
    let (code, budgeted, _) = run(&[
        "artifact",
        "project",
        x_post,
        "--max-chars",
        "3000",
        "--format",
        "json",
    ]);
    assert_eq!(code, 0);
    assert!(budgeted.chars().count() <= 3000);
    let p: serde_json::Value = serde_json::from_str(&budgeted).unwrap();
    assert_eq!(
        p["omitted"]["reasons"],
        serde_json::json!(["budget"]),
        "{p}"
    );
    let (code, _, err) = run(&["artifact", "project", x_post, "--consumer", "smell"]);
    assert_eq!(code, 3, "{err}");

    // Model-read content enters only as inference, in a derived artifact.
    let dir = tmp("annotated");
    let ann = tmp("annotation.json");
    std::fs::write(
        &ann,
        r#"{"parent":"author.avatar","by":{"name":"example-vision-model","version":"1"},
            "method":"read from view v0","at":"2026-11-16T13:00:00Z",
            "elements":[{"id":"author.avatar.reading","kind":"text","role":"description",
                         "text":"a grey placeholder silhouette","visible":true,"basis":"inferred"}]}"#,
    )
    .unwrap();
    let (code, _, err) = run(&[
        "artifact",
        "annotate",
        x_post,
        ann.to_str().unwrap(),
        "-o",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = run(&["artifact", "verify", dir.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    let (_, out, _) = run(&[
        "artifact",
        "project",
        dir.to_str().unwrap(),
        "--focus",
        "author",
    ]);
    assert!(
        out.contains("\"a grey placeholder silhouette\" !inferred"),
        "{out}"
    );
    let source_claim = std::fs::read_to_string(&ann)
        .unwrap()
        .replace("\"inferred\"", "\"source\"");
    std::fs::write(&ann, source_claim).unwrap();
    let (code, _, err) = run(&[
        "artifact",
        "annotate",
        x_post,
        ann.to_str().unwrap(),
        "-o",
        tmp("refused").to_str().unwrap(),
    ]);
    assert_eq!(code, 3);
    assert!(err.contains("only add inferred"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_file(ann);
}

/// One MCP session: send messages, return one reply per request.
fn mcp(extra: &[&str], msgs: &[String]) -> Vec<serde_json::Value> {
    let mut args = vec!["engine", "mcp"];
    args.extend_from_slice(extra);
    let mut child = Command::new(BIN)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for m in msgs {
            writeln!(stdin, "{m}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call(id: u32, tool: &str, args: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}}).to_string()
}

#[test]
fn a_text_only_agent_discovers_and_uses_the_engine_over_mcp() {
    let store = tmp("mcp-store");
    let store_s = store.to_str().unwrap().to_string();
    let post: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/fixtures/posts/media-four.json")).unwrap(),
    )
    .unwrap();
    let capture_input =
        serde_json::json!({"source": "x.post", "data": post, "capturedAt": "2026-11-16T12:00:00Z"});
    let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#.to_string();
    let msgs = vec![
        init.clone(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_string(),
        call(
            2,
            "engine_discover",
            serde_json::json!({"outcome": "understand a screenshot without vision", "accepts": ["text/plain"]}),
        ),
        call(
            3,
            "engine_describe",
            serde_json::json!({"capabilities": ["representation.capture"]}),
        ),
        call(
            4,
            "engine_invoke",
            serde_json::json!({"capability": "representation.capture", "input": capture_input}),
        ),
    ];
    // Read-only session (the default): capture is refused, with the missing scope named.
    let r = mcp(&["--store", &store_s], &msgs);
    assert_eq!(r.len(), 5, "the notification gets no reply");
    assert!(r[0]["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("engine_discover"));
    let tools: Vec<&str> = r[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        tools,
        ["engine_discover", "engine_describe", "engine_invoke"],
        "progressive by default"
    );
    assert!(
        r[1].to_string().len() < 4000,
        "the initial tool list stays small: {}",
        r[1].to_string().len()
    );
    let top = &r[2]["result"]["structuredContent"]["matches"][0];
    assert_eq!(top["capability"], "representation.understand");
    assert_eq!(top["mode"]["select"]["consumer"], "text");
    assert!(r[3]["result"]["structuredContent"]["contracts"][0]["inputSchema"].is_object());
    assert_eq!(r[4]["result"]["isError"], true);
    assert_eq!(
        r[4]["result"]["structuredContent"]["error"]["details"]["missing"],
        serde_json::json!(["store:write"])
    );

    // With the host's grant: capture, then read it as text, as resources, and as an image only on request.
    let r = mcp(
        &["--store", &store_s, "--grant", "store:read,store:write"],
        &msgs[..],
    );
    let created = &r[4]["result"]["structuredContent"];
    assert_eq!(created["ok"], true, "{created}");
    let id = created["result"]["id"].as_str().unwrap().to_string();
    let uri = created["result"]["artifact"].as_str().unwrap().to_string();
    let digest = created["resultDigest"].clone();
    let msgs = vec![
        init,
        call(1, "engine_invoke", serde_json::json!({"capability": "representation.understand", "input": {"artifact": id, "maxChars": 3000}})),
        r#"{"jsonrpc":"2.0","id":2,"method":"resources/list"}"#.to_string(),
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": format!("{uri}/projection/text")}}).to_string(),
        serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "resources/read", "params": {"uri": format!("{uri}/view/v0")}}).to_string(),
        call(5, "engine_invoke", serde_json::json!({"capability": "representation.understand", "input": {"artifact": id, "consumer": "vision", "maxChars": 3000}})),
        call(6, "engine_invoke", serde_json::json!({"capability": "representation.verify", "input": {"artifact": id}})),
    ];
    let r = mcp(&["--store", &store_s], &msgs);
    let kinds = |v: &serde_json::Value| {
        v["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["type"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        kinds(&r[1]),
        ["text", "text"],
        "a text-only consumer receives no image"
    );
    let projection = r[1]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        projection.contains("[visual-only]") && projection.chars().count() <= 3000,
        "{projection}"
    );
    assert!(r[2]["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["uri"] == uri.as_str()));
    assert!(r[3]["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("ARTIFACT "));
    assert!(r[4]["result"]["contents"][0]["blob"]
        .as_str()
        .unwrap()
        .starts_with("iVBOR"));
    assert_eq!(
        kinds(&r[5]),
        ["image", "text", "text"],
        "a vision consumer asked for pixels"
    );
    assert_eq!(r[6]["result"]["structuredContent"]["result"]["ok"], true);

    // Transport independence: the CLI transport returns the same outcome digest.
    let input = tmp("capture-input.json");
    std::fs::write(&input, capture_input.to_string()).unwrap();
    let (code, out, err) = run(&[
        "engine",
        "call",
        "representation.capture",
        "--input",
        &format!("@{}", input.display()),
        "--store",
        &store_s,
        "--grant",
        "store:write",
    ]);
    assert_eq!(code, 0, "{err}");
    let cli: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(cli["resultDigest"], digest);

    // Every capability as its own tool, for clients that prefer that.
    let r = mcp(
        &["--store", &store_s, "--tools", "all"],
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_string(),
            call(
                2,
                "representation_verify",
                serde_json::json!({"artifact": id}),
            ),
        ],
    );
    assert_eq!(
        r[0]["result"]["tools"].as_array().unwrap().len(),
        11,
        "engine + gateway contracts"
    );
    assert_eq!(r[1]["result"]["structuredContent"]["ok"], true);

    let _ = std::fs::remove_dir_all(store);
    let _ = std::fs::remove_file(input);
}

#[test]
fn an_agent_finds_and_uses_the_modality_gateway_over_mcp() {
    let store = tmp("mcp-gateway");
    let store_s = store.to_str().unwrap().to_string();
    let png = std::fs::read(root().join("tests/fixtures/gateway/invoices.png")).unwrap();
    let b64 = {
        // Standard base64, enough for a test.
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in png.chunks(3) {
            let n = (c[0] as u32) << 16
                | (*c.get(1).unwrap_or(&0) as u32) << 8
                | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                out.push(if i <= c.len() {
                    T[(n >> (18 - 6 * i) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        out
    };
    let profile: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/fixtures/gateway/profile-text-only.json")).unwrap(),
    )
    .unwrap();
    let ask = "Which invoices are overdue?";
    let r = mcp(
        &["--store", &store_s, "--grant", "store:read,store:write"],
        &[
            call(
                1,
                "engine_discover",
                serde_json::json!({"outcome": "the model does not support images", "accepts": ["text/plain"]}),
            ),
            call(
                2,
                "engine_invoke",
                serde_json::json!({"capability": "modality.adapt", "input": {
                "instruction": ask, "model": profile, "now": "2026-11-16T12:00:00Z",
                "inputs": [{"dataBase64": b64, "name": "invoices.png"}]}}),
            ),
        ],
    );
    let top = &r[0]["result"]["structuredContent"]["matches"][0];
    assert_eq!(
        (top["provider"].as_str(), top["capability"].as_str()),
        (Some("xeet-gateway"), Some("modality.adapt")),
        "{}",
        r[0]
    );
    let out = &r[1]["result"];
    assert_eq!(out["isError"], false, "{out}");
    let blocks: Vec<&str> = out["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["type"].as_str().unwrap())
        .collect();
    assert!(
        blocks.iter().all(|t| *t == "text"),
        "no image blocks for a text-only profile: {blocks:?}"
    );
    let content = &out["structuredContent"]["result"]["content"];
    assert_eq!(
        content[0],
        serde_json::json!({"type": "text", "text": ask}),
        "instruction first, unchanged"
    );
    assert!(
        content[1]["text"]
            .as_str()
            .unwrap()
            .contains("[visual-only]"),
        "no reader here: content declared unknown"
    );
    assert!(
        !out.to_string().contains("iVBOR"),
        "no image bytes returned to a text-only client"
    );
    let _ = std::fs::remove_dir_all(store);
}
