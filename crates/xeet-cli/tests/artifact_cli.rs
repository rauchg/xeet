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
fn mcp_server_lists_and_calls_tools() {
    let pr = root().join("tests/fixtures/github/pr-open-reviewed.json");
    let dir = tmp("mcp");
    let (code, _, err) = run(&[
        "artifact",
        "capture",
        "github",
        pr.to_str().unwrap(),
        "-o",
        dir.to_str().unwrap(),
        "--captured-at=2026-11-16T12:00:00Z",
    ]);
    assert_eq!(code, 0, "{err}");
    let mut child = Command::new(BIN)
        .args(["artifact", "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let p = dir.to_str().unwrap();
    let msgs = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"artifact_capabilities","arguments":{{"path":{p:?}}}}}}}"#),
        r#"{"jsonrpc":"2.0","id":4,"method":"nope"}"#.to_string(),
    ];
    {
        let stdin = child.stdin.as_mut().unwrap();
        for m in &msgs {
            writeln!(stdin, "{m}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let replies: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 4, "the notification gets no reply");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 6);
    let caps = &replies[2]["result"]["structuredContent"];
    assert_eq!(caps["capabilityPolicy"], "descriptive-only");
    assert_eq!(replies[3]["error"]["code"], -32601);
    let _ = std::fs::remove_dir_all(dir);
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

    // Over MCP, a vision consumer gets the image and the same text.
    let mut child = Command::new(BIN)
        .args(["artifact", "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for consumer in ["text", "vision"] {
            writeln!(stdin, r#"{{"jsonrpc":"2.0","id":"{consumer}","method":"tools/call","params":{{"name":"artifact_project","arguments":{{"path":{x_post:?},"consumer":"{consumer}","maxChars":4000}}}}}}"#).unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let replies: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let kinds = |r: &serde_json::Value| {
        r["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["type"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(&replies[0]), ["text"]);
    assert_eq!(kinds(&replies[1]), ["image", "text"]);
    assert!(
        replies[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 4000
    );
}
