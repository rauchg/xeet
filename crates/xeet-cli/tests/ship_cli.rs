//! The CLI as a user runs it: the `xeet-render` binary, its flags, its output
//! files and its exit codes.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_xeet-render");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/fixtures/posts").join(name)
}

fn scratch(name: &str) -> PathBuf {
    let dir = root().join("tmp").join("ship-cli-tests");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(name)
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("the xeet-render binary runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn render_writes_a_png_and_reports_the_size() {
    let out_path = scratch("render-basic.png");
    let report_path = scratch("render-basic.json");
    let post = fixture("text-simple.json");
    let out = run(&[
        "render",
        post.to_str().unwrap(),
        "--width",
        "390",
        "--theme",
        "dark",
        "--dpr",
        "3",
        "-o",
        out_path.to_str().unwrap(),
        "--report",
        report_path.to_str().unwrap(),
        "--time",
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let png = std::fs::read(&out_path).expect("the PNG was written");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(report["size"]["width"].as_f64().unwrap(), 390.0);
    assert!(report["size"]["height"].as_f64().unwrap() > 0.0);
    assert!(
        stderr(&out).contains("390"),
        "--time prints the size: {}",
        stderr(&out)
    );
}

#[test]
fn explain_prints_json_on_stdout() {
    let post = fixture("links-card.json");
    let out = run(&["explain", post.to_str().unwrap(), "--compact", "--quiet"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v: serde_json::Value =
        serde_json::from_str(stdout(&out).trim()).expect("valid JSON on stdout");
    assert!(v["size"]["height"].as_f64().unwrap() > 0.0);
    assert!(v["ruleset"].as_str().unwrap().starts_with("x-web@"));
    // Pretty is the default.
    let out = run(&["explain", post.to_str().unwrap(), "--quiet"]);
    assert!(stdout(&out).contains("\n  \"size\""));
}

#[test]
fn sheet_writes_one_png_for_the_grid() {
    let out_path = scratch("sheet-basic.png");
    let post = fixture("text-simple.json");
    let out = run(&[
        "sheet",
        post.to_str().unwrap(),
        "--widths",
        "320,390",
        "--themes",
        "both",
        "--dpr",
        "1",
        "-o",
        out_path.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let png = std::fs::read(&out_path).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    assert!(w > 390, "the sheet is {w} wide, wider than one cell");
}

#[test]
fn invalid_input_is_exit_3_and_a_clear_message() {
    let bad = scratch("too-many-media.json");
    let items = vec![r#"{"width":10,"height":10}"#; 21].join(",");
    std::fs::write(&bad, format!(r#"{{"text":"x","media":[{items}]}}"#)).unwrap();
    let out = run(&[
        "render",
        bad.to_str().unwrap(),
        "-o",
        scratch("never.png").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("post.media"),
        "the message names the field: {err}"
    );
    assert!(!err.contains("panicked"));
}

#[test]
fn bad_usage_is_exit_2() {
    let post = fixture("text-simple.json");
    for args in [
        vec!["render", post.to_str().unwrap(), "--nonsense"],
        vec!["render", post.to_str().unwrap(), "--theme", "dim"],
        vec!["render", post.to_str().unwrap(), "--width", "wide"],
        vec!["sheet", post.to_str().unwrap(), "--widths", "320,abc"],
        vec!["no-such-command"],
    ] {
        let out = run(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} → stderr: {}",
            stderr(&out)
        );
    }
}

#[test]
fn missing_file_is_reported_not_panicked() {
    let out = run(&["explain", "/nonexistent/post.json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("could not read"));
    assert!(!stderr(&out).contains("panicked"));
}

#[test]
fn stdin_and_stdout_work_together() {
    let post = std::fs::read(fixture("text-simple.json")).unwrap();
    let mut child = Command::new(BIN)
        .args(["render", "-", "--width", "390", "--dpr", "1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    use std::io::Write;
    child.stdin.as_mut().unwrap().write_all(&post).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        &out.stdout[..8],
        b"\x89PNG\r\n\x1a\n",
        "the PNG goes to stdout when there is no -o"
    );
}

#[test]
fn help_lists_the_commands() {
    let out = run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    for expected in [
        "render",
        "explain",
        "sheet",
        "--dpr",
        "--theme",
        "Exit codes",
    ] {
        assert!(text.contains(expected), "help is missing {expected}");
    }
}

#[test]
fn xeet_font_dir_allows_file_override_and_rejects_bad_dir() {
    let post = fixture("text-simple.json");

    // Bad XEET_FONT_DIR fails loudly instead of silently rendering with embedded defaults.
    let bad = Command::new(BIN)
        .env("XEET_FONT_DIR", "/nonexistent/xeet-font-override-dir")
        .args(["render", post.to_str().unwrap(), "--dpr", "1"])
        .output()
        .expect("spawn");
    assert_ne!(bad.status.code(), Some(0));
    assert!(
        stderr(&bad).contains("XEET_FONT_DIR"),
        "expected XEET_FONT_DIR error, got: {}",
        stderr(&bad)
    );

    // Valid XEET_FONT_DIR with a standalone font file overrides cleanly.
    let tmp = std::env::temp_dir().join(format!("xeet-font-override-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let src_font = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/vendor/fonts/NotoSansSC-Regular.otf");
    std::fs::copy(&src_font, tmp.join("NotoSansSC-Regular.otf")).unwrap();

    let ok = Command::new(BIN)
        .env("XEET_FONT_DIR", &tmp)
        .args(["render", post.to_str().unwrap(), "--dpr", "1"])
        .output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "valid XEET_FONT_DIR failed: {}",
        stderr(&ok)
    );
    assert_eq!(&ok.stdout[..8], b"\x89PNG\r\n\x1a\n");
}
