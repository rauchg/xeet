//! The `xeet` commands: `render`, `explain`, `sheet`.
//!
//! These modules are the argument parsing and I/O of the CLI. They hold no
//! rendering logic: everything goes through `xeet_core::render`, so the CLI
//! and the WASM package cannot drift apart.
//!
//! `src/main.rs` dispatches here for `render`, `explain`, and `sheet`; a second
//! binary, `xeet-render`, exposes the same module plus `serve --stdio` for the
//! render JSON-lines protocol (while `xeet serve --stdio` serves text layout).
//!
//! Exit codes, stable for scripts:
//!
//! | code | meaning |
//! |---|---|
//! | 0 | success |
//! | 2 | bad usage (unknown flag, missing argument) |
//! | 3 | the input is invalid or over a limit (HTTP 4xx) |
//! | 4 | the render itself failed (HTTP 5xx) |
//! | 5 | could not read or write a file |

pub mod artifact;
pub mod capture;
pub mod engine;
pub mod explain;
pub mod gateway;
pub mod render;
pub mod resolve;
pub mod serve;
pub mod sheet;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use xeet_core::limits::{self, Limits};
use xeet_core::model::{Post, RenderOptions, Surface, Theme};
use xeet_core::render::{self as xr, Error};

pub const EXIT_OK: u8 = 0;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_INPUT: u8 = 3;
pub const EXIT_RENDER: u8 = 4;
pub const EXIT_IO: u8 = 5;

/// The commands this module implements, in the order `--help` lists them.
pub const COMMANDS: &[&str] = &[
    "render", "explain", "sheet", "resolve", "serve", "artifact", "engine", "gateway", "capture",
];

/// True when `name` is one of [`COMMANDS`].
pub fn is_command(name: &str) -> bool {
    COMMANDS.contains(&name)
}

// ---------------------------------------------------------------------------
// Lazy font packs & XEET_FONT_DIR overrides
// ---------------------------------------------------------------------------
//
// Native CLI builds embed all faces (`has_lazy_packs() == false`), so when
// `XEET_FONT_DIR` is unset every helper below short-circuits with zero
// filesystem or serialization overhead. Setting `XEET_FONT_DIR` opts into
// runtime face overrides (either a `packs.json` bundle or individual
// `<FaceFile>.ttf`/`.otf` files inside `XEET_FONT_DIR`).

fn apply_env_font_dir_once() -> Result<(), String> {
    static ENV_OVERRIDE: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    ENV_OVERRIDE
        .get_or_init(|| {
            let Some(raw) = std::env::var_os("XEET_FONT_DIR") else {
                return Ok(());
            };
            if raw.is_empty() {
                return Ok(());
            }
            let dir = PathBuf::from(raw);
            xeet_core::fonts::load_overrides_from_dir(&dir).map(|_| ())
        })
        .clone()
}

/// The directory that holds `packs.json` and `<pack>.bin`, if this build needs
/// one. `XEET_FONT_DIR` wins, then a sibling `fonts/` directory, then (in debug
/// builds only) a `cargo run` from the repo root.
pub fn pack_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XEET_FONT_DIR") {
        if !dir.is_empty() {
            let dir = PathBuf::from(dir);
            return dir.join("packs.json").exists().then_some(dir);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin) = exe.parent() {
            for candidate in [
                bin.join("fonts"),
                bin.join("../fonts"),
                bin.join("../../fonts"),
            ] {
                if candidate.join("packs.json").exists() {
                    return Some(candidate);
                }
            }
        }
    }
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from("packages/xeet/wasm/fonts");
        if dev.join("packs.json").exists() {
            return Some(dev);
        }
    }
    None
}

/// Registers the packs `value` needs (and applies `XEET_FONT_DIR` once if set).
#[inline]
pub fn ensure_fonts_for_json(value: &serde_json::Value) -> Result<(), String> {
    apply_env_font_dir_once()?;
    if !xeet_core::fonts::has_lazy_packs() {
        return Ok(());
    }
    let json = serde_json::to_string(value)
        .map_err(|e| format!("xeet: could not serialize the font request: {e}"))?;
    ensure_fonts_for_json_text(&json)
}

/// [`ensure_fonts_for_json`] for a request that is already JSON.
#[inline]
pub fn ensure_fonts_for_json_text(json: &str) -> Result<(), String> {
    apply_env_font_dir_once()?;
    if !xeet_core::fonts::has_lazy_packs() {
        return Ok(());
    }
    let needed = xeet_core::fonts::missing_packs_for_json(json);
    if needed.is_empty() {
        return Ok(());
    }
    let Some(dir) = pack_dir() else {
        return Err(format!(
            "xeet: this build needs the {} font pack(s), but no packs.json was found beside the binary or in XEET_FONT_DIR",
            needed.join(", ")
        ));
    };
    xeet_core::fonts::load_packs_for_json(&dir, json)?;
    Ok(())
}

/// Registers the packs a post and its options need.
#[inline]
pub fn ensure_fonts_for_post(post: &Post, options: &RenderOptions) -> Result<(), String> {
    apply_env_font_dir_once()?;
    if !xeet_core::fonts::has_lazy_packs() {
        return Ok(());
    }
    ensure_fonts_for_json(&serde_json::json!({ "post": post, "options": options }))
}

/// Loads every pack (and applies `XEET_FONT_DIR` overrides if set), for
/// commands that inspect the whole font set.
#[inline]
pub fn load_all_fonts() -> Result<(), String> {
    apply_env_font_dir_once()?;
    if !xeet_core::fonts::has_lazy_packs() {
        return Ok(());
    }
    let Some(dir) = pack_dir() else {
        return Ok(());
    };
    xeet_core::fonts::load_all_packs(&dir)?;
    Ok(())
}

/// Dispatches a command, returning the process exit code.
pub fn dispatch(name: &str, args: &[String]) -> u8 {
    match name {
        "render" => render::run(args),
        "explain" => explain::run(args),
        "sheet" => sheet::run(args),
        // The playground's long-lived render server (`serve --stdio`). It has no
        // place in `main.rs`, which the playground work owns; this is where the
        // `xeet-render` binary finds it.
        "serve" => serve::run(args),
        // The resolver (`xeet resolve`): the same crate the playground and the
        // WASM package call, with the default native host.
        "resolve" => resolve::run(args),
        // Durable artifacts: capture, inspect, verify, compare, project.
        "artifact" => artifact::run(args),
        // The representation engine as a provider: manifest, discovery, MCP.
        "engine" => engine::run(args),
        // The modality gateway: model-consumable requests from any input.
        "gateway" => gateway::run(args),
        // Screenshots: durable artifact + local OCR, the native app's entry point.
        "capture" => capture::run(args),
        _ => {
            eprintln!("xeet: unknown command `{name}`");
            eprintln!("{}", usage_for("xeet"));
            EXIT_USAGE
        }
    }
}

/// Runs `dispatch` and turns the code into the process's `ExitCode`.
pub fn main_for(name: &str, args: &[String]) -> ExitCode {
    ExitCode::from(dispatch(name, args))
}

/// The usage block shared by both binaries. `prog` is the binary name, so the
/// help text matches whatever the user typed.
pub fn usage_for(prog: &str) -> String {
    format!(
        "\
{prog} render <post.json|-> [options] [-o out.png]
{prog} explain <post.json|-> [options]
{prog} sheet <post.json|-> --widths 320,390,600 --themes light,dark [-o sheet.png]
{prog} resolve <text|x.com post URL|post.json|-> [--media] [--width N] [--dpr N]

Options for render and explain:
  --surface web-timeline|web-detail|ios-timeline|ios-detail
                                      which view of X (default web-timeline)
  --width N                           viewport width in CSS pixels (default 600)
  --theme light|dark                  theme (default light)
  --dpr N                             device pixel ratio 1..4 (default 2)
  --font-scale N                      X's user font-size setting (default 1.0)
  --expanded                          render with 'Show more' toggled on

render:
  -o, --out PATH                      PNG destination (default: stdout)
  --report PATH                       also write the JSON report ('-' for stderr)
  --svg PATH                          also write the display list as SVG
  --geometry PATH                     also write every box as JSON (the oracle seam)
  --time                              print elapsed time to stderr

explain:
  --compact                           one-line JSON instead of pretty
  --quiet                             no warnings on stderr

sheet:
  --widths 320,390,600                columns (default 390)
  --themes light,dark                 rows (default light,dark)
  --dpr N                             device pixel ratio (default 2)
  --surface web-timeline|web-detail

resolve:
  --media                             inline image bytes (post media, avatars,
                                      badges, card images) as the renderer needs them
  --width N --dpr N                   the cell the variants are chosen for (600, 2)
  --max-links N                       how many of the post's URLs to resolve (4)
  --no-links                          skip the network entirely
  --cache DIR                         keep resolved links in DIR (none by default)
  --compact                           one-line JSON (default: pretty)
  --quiet                             no warnings on stderr

serve --stdio:                        one JSON request per line, one JSON reply per line
  ops: render (PNG, report, geometry), explain (report), info (version)

{artifact}
{engine}
{gateway}
{capture}

Any command: --limits standard|unlimited (default standard), --help.

Exit codes: 0 ok, 2 usage, 3 invalid input, 4 render failed, 5 I/O.",
        artifact = artifact::USAGE
            .lines()
            .map(|l| format!("{prog} {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        engine = engine::USAGE
            .lines()
            .map(|l| format!("{prog} {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        gateway = gateway::USAGE
            .lines()
            .map(|l| format!("{prog} {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        capture = capture::USAGE
            .lines()
            .map(|l| format!("{prog} {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Flags that take no value. Everything else consumes the next token, which
/// is what every user expects of `-o out.png` and `--report r.json`.
///
/// `stdio` is here for `serve --stdio` (the playground's render server): it is
/// the only transport, and it must not swallow the next flag as its value.
pub const BOOLEAN_FLAGS: &[&str] = &[
    "compact",
    "quiet",
    "time",
    "help",
    "h",
    "version",
    "V",
    "stdio",
    "media",
    "no-media",
    "no-links",
    "refresh",
    "offline",
    "expanded",
    "json",
    "no-reproduce",
    "compact-capabilities",
];

/// A tiny, forgiving argument parser: positional arguments plus `--flags` and
/// `-s` shorts. `--flag=value` and `--flag value` both work. Unknown flags are
/// reported by the command, not silently ignored.
pub struct Args {
    pub positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    pub fn parse(argv: &[String]) -> Result<Args, String> {
        let mut positional = Vec::new();
        let mut flags: Vec<(String, Option<String>)> = Vec::new();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            let is_long = a.starts_with("--");
            let is_short = !is_long && a.starts_with('-') && a.len() > 1;
            if !is_long && !is_short {
                positional.push(a.clone());
                i += 1;
                continue;
            }
            let (name, inline) = if is_long {
                let body = &a[2..];
                match body.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (body.to_string(), None),
                }
            } else {
                let body = &a[1..];
                match body.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (body.to_string(), None),
                }
            };
            if inline.is_some() || BOOLEAN_FLAGS.contains(&name.as_str()) {
                flags.push((name, inline));
                i += 1;
                continue;
            }
            match argv.get(i + 1) {
                Some(next) => {
                    flags.push((name, Some(next.clone())));
                    i += 2;
                }
                None => {
                    flags.push((name, None));
                    i += 1;
                }
            }
        }
        Ok(Args { positional, flags })
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }

    /// Flags present that the command did not declare, with the leading dash
    /// form the user typed.
    pub fn unknown(&self, known: &[&str]) -> Vec<String> {
        self.flags
            .iter()
            .map(|(n, _)| n.clone())
            .filter(|n| !known.contains(&n.as_str()))
            .map(|n| {
                if n.len() == 1 {
                    format!("-{n}")
                } else {
                    format!("--{n}")
                }
            })
            .collect()
    }
}

/// Parses the options every command shares into `RenderOptions`.
pub fn render_options(args: &Args) -> Result<RenderOptions, String> {
    let mut o = RenderOptions::default();
    if let Some(s) = args.value("surface") {
        o.surface = match s {
            "web-timeline" | "timeline" => Surface::WebTimeline,
            "web-detail" | "detail" => Surface::WebDetail,
            "ios-timeline" | "ios" => Surface::IosTimeline,
            "ios-detail" => Surface::IosDetail,
            other => {
                return Err(format!(
                    "unknown surface `{other}`; expected web-timeline, web-detail, ios-timeline or ios-detail"
                ))
            }
        };
    }
    if let Some(w) = args.value("width") {
        o.width = w
            .parse()
            .map_err(|_| format!("--width needs a number, got `{w}`"))?;
    }
    if let Some(t) = args.value("theme") {
        o.theme = parse_theme(t)?;
    }
    if let Some(d) = args.value("dpr") {
        o.dpr = d
            .parse()
            .map_err(|_| format!("--dpr needs a number, got `{d}`"))?;
    }
    if let Some(f) = args.value("font-scale").or_else(|| args.value("fontScale")) {
        o.font_scale = f
            .parse()
            .map_err(|_| format!("--font-scale needs a number, got `{f}`"))?;
    }
    if args.has("expanded") {
        o.expanded = true;
    }
    Ok(o)
}

pub fn parse_theme(s: &str) -> Result<Theme, String> {
    match s {
        "light" => Ok(Theme::Light),
        "dark" => Ok(Theme::Dark),
        other => Err(format!("unknown theme `{other}`; expected light or dark")),
    }
}

/// Parses `--limits`. Standard everywhere; `unlimited` exists for the oracle
/// and for debugging a post that is over a limit, never for a server.
pub fn limits(args: &Args) -> Result<Limits, String> {
    match args.value("limits") {
        None | Some("standard") => Ok(Limits::standard()),
        Some("unlimited") => Ok(Limits::unlimited()),
        Some(other) => Err(format!(
            "unknown --limits `{other}`; expected standard or unlimited"
        )),
    }
}

/// Reads the post JSON from a path, or from stdin for `-`. The path is echoed
/// in errors so a caller with several files knows which one failed.
pub fn read_input(args: &Args) -> Result<(String, String), String> {
    let path = args
        .positional
        .first()
        .ok_or_else(|| "expected a post JSON path (or - for stdin)".to_string())?;
    if args.positional.len() > 1 {
        return Err(format!(
            "unexpected extra argument `{}`",
            args.positional[1]
        ));
    }
    let mut text = String::new();
    let source = if path == "-" {
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("could not read stdin: {e}"))?;
        "<stdin>".to_string()
    } else {
        text = std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?;
        path.clone()
    };
    Ok((unwrap_resolved(text), source))
}

/// `xeet resolve` prints `{post, links, media, source, warnings}`; the render
/// commands take that as-is and use its `post`, so
/// `xeet resolve <url> --media | xeet render -` works. Anything else, including
/// a post that merely has a field named `post`, is passed through unchanged.
fn unwrap_resolved(text: String) -> String {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        return text;
    };
    let is_envelope = map.contains_key("post")
        && !map.contains_key("text")
        && map.keys().all(|k| {
            matches!(
                k.as_str(),
                "post" | "links" | "media" | "source" | "warnings"
            )
        });
    match (is_envelope, map.get("post")) {
        (true, Some(post @ serde_json::Value::Object(_))) => post.to_string(),
        _ => text,
    }
}

#[cfg(test)]
mod unwrap_tests {
    use super::unwrap_resolved;

    #[test]
    fn a_resolve_envelope_becomes_its_post() {
        let out = unwrap_resolved(
            r#"{"post":{"text":"hi"},"links":{},"media":{},"source":{},"warnings":[]}"#.into(),
        );
        assert_eq!(out, r#"{"text":"hi"}"#);
    }

    #[test]
    fn a_post_is_left_alone() {
        let post = r#"{"text":"hi","post":1}"#.to_string();
        assert_eq!(unwrap_resolved(post.clone()), post);
        let bad = "not json".to_string();
        assert_eq!(unwrap_resolved(bad.clone()), bad);
    }
}

/// Writes bytes to a path, or to stdout for `-`/absent.
pub fn write_output(path: Option<&str>, bytes: &[u8]) -> Result<(), String> {
    match path {
        Some(p) if p != "-" => {
            std::fs::write(p, bytes).map_err(|e| format!("could not write {p}: {e}"))
        }
        _ => {
            let mut out = std::io::stdout().lock();
            out.write_all(bytes)
                .and_then(|_| out.flush())
                .map_err(|e| format!("could not write stdout: {e}"))
        }
    }
}

/// Prints a report or error payload to a path, or to stderr for `-`.
pub fn write_text(path: &str, text: &str) -> Result<(), String> {
    if path == "-" {
        eprint!("{text}");
        if !text.ends_with('\n') {
            eprintln!();
        }
        Ok(())
    } else {
        std::fs::write(path, text).map_err(|e| format!("could not write {path}: {e}"))
    }
}

/// Prints an error the way the CLI does: one line, the code included, and a
/// pointer at `--limits` when the request was simply too big.
pub fn report_error(what: &str, e: &Error) -> u8 {
    eprintln!("xeet: {what}: {e}");
    if let Error::Limit(le) = e {
        if !le.is_client_error() {
            eprintln!("xeet: this request is within the input limits but needs more work than one render may spend");
        }
    }
    if e.is_client_error() {
        EXIT_INPUT
    } else {
        EXIT_RENDER
    }
}

/// Prints the report's warnings, which are the most useful part for an agent
/// (PLAN.md §6), one per line.
pub fn warn_report(warnings: &[String]) {
    for w in warnings {
        eprintln!("xeet: warning: {w}");
    }
}

/// A limit summary for `--help` output and error footers.
pub fn limits_line(l: &Limits) -> String {
    limits::summary(l)
}

/// Renders the display list as SVG. Paint owns the conversion; this keeps the
/// CLI from growing a second one.
pub fn svg_for(post: &xeet_core::model::Post, opts: &RenderOptions) -> Result<String, Error> {
    xr::svg(post, opts).map(|(svg, _)| svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn flags_take_values_and_positionals_stay() {
        let a = Args::parse(&argv("post.json --width 390 --theme dark --dpr 3")).unwrap();
        assert_eq!(a.positional, vec!["post.json"]);
        assert_eq!(a.value("width"), Some("390"));
        assert_eq!(a.value("theme"), Some("dark"));
        let a = Args::parse(&argv("--width 390 post.json")).unwrap();
        assert_eq!(a.positional, vec!["post.json"]);
        let a = Args::parse(&argv("post.json --compact")).unwrap();
        assert!(a.has("compact"));
        assert_eq!(a.value("compact"), None);
    }

    #[test]
    fn equals_form_and_short_flags() {
        let a = Args::parse(&argv("post.json --width=320 -o out.png")).unwrap();
        assert_eq!(a.positional, vec!["post.json"]);
        assert_eq!(a.value("width"), Some("320"));
        assert_eq!(a.value("o"), Some("out.png"));
        let a = Args::parse(&argv("--report r.json post.json --compact")).unwrap();
        assert_eq!(a.value("report"), Some("r.json"));
        assert_eq!(a.positional, vec!["post.json"]);
        assert!(a.has("compact"));
    }

    #[test]
    fn options_parse_and_reject_garbage() {
        let a = Args::parse(&argv(
            "p.json --surface web-detail --width 390 --theme dark --dpr 3 --font-scale 1.25",
        ))
        .unwrap();
        let o = render_options(&a).unwrap();
        assert_eq!(o.surface, Surface::WebDetail);
        assert_eq!(o.width, 390.0);
        assert_eq!(o.theme, Theme::Dark);
        assert_eq!(o.dpr, 3.0);
        assert_eq!(o.font_scale, 1.25);
        let a = Args::parse(&argv("p.json --theme dim")).unwrap();
        assert!(render_options(&a).unwrap_err().contains("light"));
        let a = Args::parse(&argv("p.json --width wide")).unwrap();
        assert!(render_options(&a).unwrap_err().contains("--width"));
    }

    #[test]
    fn unknown_flags_are_listed() {
        let a = Args::parse(&argv("p.json --widht 390")).unwrap();
        assert_eq!(a.unknown(&["width", "theme"]), vec!["--widht"]);
    }

    #[test]
    fn limits_flag() {
        let a = Args::parse(&argv("p.json --limits unlimited")).unwrap();
        assert_eq!(
            limits(&a).unwrap().max_text_chars,
            Limits::unlimited().max_text_chars
        );
        let a = Args::parse(&argv("p.json --limits huge")).unwrap();
        assert!(limits(&a).unwrap_err().contains("standard"));
    }
}
