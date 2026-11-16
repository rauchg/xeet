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
        "project",
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
        "project",
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
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 5);
    let caps = &replies[2]["result"]["structuredContent"];
    assert_eq!(caps["capabilityPolicy"], "descriptive-only");
    assert_eq!(replies[3]["error"]["code"], -32601);
    let _ = std::fs::remove_dir_all(dir);
}
