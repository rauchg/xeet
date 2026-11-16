//! GitHub pull requests → artifacts.
//!
//! Input is a bundle of GitHub REST API responses the caller already fetched:
//!
//! ```json
//! { "apiVersion": "2022-11-28",
//!   "pull":     GET /repos/{o}/{r}/pulls/{n},
//!   "reviews":  GET /repos/{o}/{r}/pulls/{n}/reviews,
//!   "comments": GET /repos/{o}/{r}/issues/{n}/comments,
//!   "files":    GET /repos/{o}/{r}/pulls/{n}/files,
//!   "commits":  GET /repos/{o}/{r}/pulls/{n}/commits }
//! ```
//!
//! Only `pull` is required. The adapter never fetches: authentication and
//! network policy stay with the caller, and the bundle is preserved verbatim
//! as the artifact's input. The view is the source-independent [`view`]
//! renderer's document layout — the PR's content, not a replica of github.com.
//!
//! Everything GitHub-specific lives here, as `github.*` roles and attributes.

use serde_json::{json, Value};
use xeet_artifact::*;
use xeet_core::model::Theme;

use crate::view::{self, Badge, Block, Entry, Stat, Tone, ViewOptions};
use crate::{at, el, fact, tool};

pub const ADAPTER: &str = "xeet-capture/github";
pub const INPUT_ROLE: &str = "source-bundle";
const VIEW: &str = "v0";
/// Rows per section in the view; the rest stay in the semantic graph, not shown.
const MAX_ROWS: usize = 12;

pub struct Capture<'a> {
    /// The bundle JSON, exactly as assembled by the caller. Preserved verbatim.
    pub bundle_json: &'a [u8],
    pub width: f32,
    pub theme: Theme,
    pub scale: f32,
    pub captured_at: String,
    /// `api-response` for live REST responses; `fixture` for recorded or
    /// constructed data.
    pub method: String,
}

fn s<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(v, |v, k| v.get(*k))
        .and_then(Value::as_str)
}
fn n(v: &Value, path: &[&str]) -> Option<u64> {
    path.iter()
        .try_fold(v, |v, k| v.get(*k))
        .and_then(Value::as_u64)
}
fn b(v: &Value, path: &[&str]) -> Option<bool> {
    path.iter()
        .try_fold(v, |v, k| v.get(*k))
        .and_then(Value::as_bool)
}
fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn first_line(text: &str) -> String {
    crate::x::first_line(text)
}
fn day(ts: Option<&str>) -> String {
    ts.map(|t| t.chars().take(10).collect()).unwrap_or_default()
}

/// Everything derived from a bundle, before rendering. Pure.
struct Built {
    blocks: Vec<Block>,
    elements: Vec<Element>,
    relations: Vec<Relation>,
    facts: Vec<Fact>,
    caps: Vec<Capability>,
    lim: Limitations,
    subject: Subject,
    source: SourceRef,
}

fn parse(bundle_json: &[u8]) -> Result<Value, String> {
    let v: Value =
        serde_json::from_slice(bundle_json).map_err(|e| format!("bundle is not JSON: {e}"))?;
    if !v.get("pull").map(Value::is_object).unwrap_or(false) {
        return Err("bundle has no `pull` object (GET /repos/{o}/{r}/pulls/{n})".into());
    }
    Ok(v)
}

fn build(bundle: &Value) -> Result<Built, String> {
    let pr = &bundle["pull"];
    let number = n(pr, &["number"]).ok_or("pull.number missing")?;
    let title = s(pr, &["title"]).unwrap_or("").to_string();
    let repo = s(pr, &["base", "repo", "full_name"]).map(str::to_string);
    let html_url = s(pr, &["html_url"]).map(str::to_string);
    let login = s(pr, &["user", "login"]).unwrap_or("").to_string();
    let merged = b(pr, &["merged"]).unwrap_or(false) || s(pr, &["merged_at"]).is_some();
    let draft = b(pr, &["draft"]).unwrap_or(false);
    let state = s(pr, &["state"]).unwrap_or("open");
    let (state_label, tone) = match (state, merged, draft) {
        (_, true, _) => ("Merged", Tone::Done),
        ("closed", _, _) => ("Closed", Tone::Danger),
        (_, _, true) => ("Draft", Tone::Neutral),
        _ => ("Open", Tone::Success),
    };
    let base_ref = s(pr, &["base", "ref"]).unwrap_or("").to_string();
    let head_ref = s(pr, &["head", "label"])
        .or(s(pr, &["head", "ref"]))
        .unwrap_or("")
        .to_string();
    let head_sha = s(pr, &["head", "sha"]).map(str::to_string);

    let mut elements = Vec::new();
    let mut relations = Vec::new();
    let mut facts = Vec::new();
    let mut caps = Vec::new();
    let mut lim = Limitations::default();
    let mut blocks = Vec::new();

    let mut root = el("pr", "record", "github.pull_request");
    root.text = Some(title.clone());
    root.value = Some(json!(number));
    root.attrs.insert("github.repository".into(), json!(repo));

    let reference = format!("{}#{number}", repo.clone().unwrap_or_default());
    let mut r = el("pr.ref", "reference", "github.pr.number");
    r.text = Some(reference.clone());
    r.value = html_url.clone().map(Value::String);
    blocks.push(Block::Meta {
        el: "pr.ref".into(),
        text: reference.clone(),
    });

    let mut t = el("pr.title", "heading", "github.pr.title");
    t.text = Some(title.clone());
    blocks.push(Block::Heading {
        el: "pr.title".into(),
        text: title.clone(),
    });

    let mut st = el("pr.state", "status", "github.pr.state");
    st.text = Some(state_label.into());
    st.value = Some(json!(state_label.to_lowercase()));
    st.basis = Basis::Derived;
    let mut badges = vec![Badge {
        el: "pr.state".into(),
        text: state_label.into(),
        tone,
    }];
    let mut label_ids = Vec::new();
    for l in arr(pr, "labels") {
        if let Some(name) = s(l, &["name"]) {
            let id = format!("label:{name}");
            let mut e = el(&id, "label", "github.label");
            e.text = Some(name.into());
            badges.push(Badge {
                el: id.clone(),
                text: name.into(),
                tone: Tone::Accent,
            });
            label_ids.push(id);
            elements.push(e);
        }
    }
    blocks.push(Block::Badges { items: badges });

    let mut author = el("pr.author", "identity", "github.user");
    author.text = Some(login.clone());
    author.value = Some(json!(login));
    author
        .attrs
        .insert("github.profile".into(), json!(s(pr, &["user", "html_url"])));
    let commits_n = n(pr, &["commits"]);
    let verb = if merged { "merged" } else { "wants to merge" };
    let summary = format!(
        "{login} {verb} {} into {base_ref} from {head_ref}",
        commits_n
            .map(|c| format!("{c} commit{}", if c == 1 { "" } else { "s" }))
            .unwrap_or_else(|| "commits".into())
    );
    let mut sum = el("pr.summary", "text", "github.pr.summary");
    sum.text = Some(summary.clone());
    sum.basis = Basis::Derived;
    sum.children = vec!["pr.author".into(), "pr.base".into(), "pr.head".into()];
    let mut base = el("pr.base", "reference", "github.ref.base");
    base.text = Some(base_ref.clone());
    let mut head = el("pr.head", "reference", "github.ref.head");
    head.text = Some(head_ref.clone());
    head.value = head_sha.clone().map(Value::String);
    // Tokens inside a sentence have no boxes of their own.
    for e in [&mut author, &mut base, &mut head] {
        e.visible = true;
    }
    blocks.push(Block::Meta {
        el: "pr.summary".into(),
        text: summary,
    });
    lim.omitted.push(Note::new(
        Some("pr.summary"),
        "bounds of the author and branch names inside the summary line",
        "the view records block boxes, not token boxes",
    ));
    relations.push(rel("pr", "pr.author", "authoredBy"));
    relations.push(rel("pr.head", "pr.base", "mergesInto"));

    // Stats.
    let mut stats = Vec::new();
    let mut stat_ids = Vec::new();
    for (id, key, unit, fmt, tone) in [
        (
            "stat.additions",
            "additions",
            "lines added",
            "+{}",
            Tone::Success,
        ),
        (
            "stat.deletions",
            "deletions",
            "lines deleted",
            "\u{2212}{}",
            Tone::Danger,
        ),
        (
            "stat.files",
            "changed_files",
            "files changed",
            "{} files",
            Tone::Neutral,
        ),
        (
            "stat.commits",
            "commits",
            "commits",
            "{} commits",
            Tone::Neutral,
        ),
    ] {
        if let Some(v) = n(pr, &[key]) {
            let mut e = el(id, "quantity", &format!("github.pr.{key}"));
            e.value = Some(json!(v));
            e.unit = Some(unit.into());
            let text = fmt.replace("{}", &v.to_string());
            e.text = Some(text.clone());
            stats.push(Stat {
                el: id.into(),
                text,
                tone,
            });
            stat_ids.push(id.to_string());
            elements.push(e);
        }
    }
    if !stats.is_empty() {
        blocks.push(Block::Stats { items: stats });
    }

    // Body.
    let body_text = s(pr, &["body"]).unwrap_or("").to_string();
    let mut body = el("pr.body", "text", "github.pr.body");
    body.attrs.insert("github.format".into(), json!("markdown"));
    if body_text.trim().is_empty() {
        body.visible = false;
        body.basis = Basis::Source;
    } else {
        body.text = Some(body_text.clone());
        blocks.push(Block::Paragraph {
            el: "pr.body".into(),
            text: body_text,
            max_lines: Some(14),
        });
        lim.approximated.push(Note::new(
            Some("pr.body"),
            "markdown shown as source text, not rendered",
            "the view renderer lays out text; it does not implement markdown",
        ));
    }

    // Lists: reviews, files, commits, comments.
    let mut sections: Vec<String> = Vec::new();
    let mut latest: std::collections::BTreeMap<String, String> = Default::default();
    let reviews = arr(bundle, "reviews");
    let review_ids = list_section(
        "reviews",
        "Reviews",
        "github.reviews",
        reviews,
        |i, r| {
            let id = format!(
                "review:{}",
                n(r, &["id"])
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| i.to_string())
            );
            let who = s(r, &["user", "login"]).unwrap_or("?");
            let state = s(r, &["state"]).unwrap_or("COMMENTED");
            let mut e = el(&id, "message", "github.review");
            e.text = s(r, &["body"]).map(str::to_string);
            e.value = Some(json!(state.to_lowercase()));
            e.attrs.insert("github.reviewer".into(), json!(who));
            e.attrs
                .insert("github.submittedAt".into(), json!(s(r, &["submitted_at"])));
            e.attrs
                .insert("github.commitId".into(), json!(s(r, &["commit_id"])));
            let tone = match state {
                "APPROVED" => Tone::Success,
                "CHANGES_REQUESTED" => Tone::Danger,
                _ => Tone::Neutral,
            };
            let mut sec = day(s(r, &["submitted_at"]));
            if let Some(body) = s(r, &["body"]).filter(|b| !b.trim().is_empty()) {
                sec = format!("{sec} · {}", first_line(body));
            }
            (
                e,
                Entry {
                    el: id,
                    primary: format!("{who} — {}", state.replace('_', " ").to_lowercase()),
                    secondary: Some(sec),
                    tone,
                },
            )
        },
        &mut elements,
        &mut blocks,
        &mut lim,
    );
    for r in reviews {
        if let (Some(who), Some(state)) = (s(r, &["user", "login"]), s(r, &["state"])) {
            if state == "APPROVED" || state == "CHANGES_REQUESTED" || state == "DISMISSED" {
                latest.insert(who.to_string(), state.to_string());
            }
        }
    }
    for id in &review_ids {
        relations.push(rel(id, "pr", "reviews"));
    }
    if !review_ids.is_empty() {
        sections.push("reviews".into());
    }

    let file_ids = list_section(
        "files",
        "Files changed",
        "github.files",
        arr(bundle, "files"),
        |i, f| {
            let path = s(f, &["filename"]).unwrap_or("").to_string();
            let id = if path.is_empty() {
                format!("file:{i}")
            } else {
                format!("file:{path}")
            };
            let mut e = el(&id, "reference", "github.file");
            e.text = Some(path.clone());
            e.attrs
                .insert("github.status".into(), json!(s(f, &["status"])));
            e.attrs
                .insert("github.additions".into(), json!(n(f, &["additions"])));
            e.attrs
                .insert("github.deletions".into(), json!(n(f, &["deletions"])));
            let sec = format!(
                "{} · +{} \u{2212}{}",
                s(f, &["status"]).unwrap_or("modified"),
                n(f, &["additions"]).unwrap_or(0),
                n(f, &["deletions"]).unwrap_or(0)
            );
            (
                e,
                Entry {
                    el: id,
                    primary: path,
                    secondary: Some(sec),
                    tone: Tone::Neutral,
                },
            )
        },
        &mut elements,
        &mut blocks,
        &mut lim,
    );
    if !file_ids.is_empty() {
        sections.push("files".into());
        lim.omitted.push(Note::new(Some("files"), "diff contents", "the bundle's file entries are summarized; patches are kept only in the preserved input"));
    }

    let commit_ids = list_section(
        "commits",
        "Commits",
        "github.commits",
        arr(bundle, "commits"),
        |i, c| {
            let sha = s(c, &["sha"]).unwrap_or("").to_string();
            let short: String = sha.chars().take(7).collect();
            let id = if sha.is_empty() {
                format!("commit:{i}")
            } else {
                format!("commit:{short}")
            };
            let msg = s(c, &["commit", "message"]).unwrap_or("");
            let mut e = el(&id, "reference", "github.commit");
            e.text = Some(first_line(msg));
            e.value = Some(json!(sha));
            e.attrs.insert("github.message".into(), json!(msg));
            e.attrs.insert(
                "github.author".into(),
                json!(s(c, &["commit", "author", "name"])),
            );
            e.attrs.insert(
                "github.date".into(),
                json!(s(c, &["commit", "author", "date"])),
            );
            let sec = format!(
                "{short} · {} · {}",
                s(c, &["commit", "author", "name"]).unwrap_or("?"),
                day(s(c, &["commit", "author", "date"]))
            );
            (
                e,
                Entry {
                    el: id,
                    primary: first_line(msg),
                    secondary: Some(sec),
                    tone: Tone::Neutral,
                },
            )
        },
        &mut elements,
        &mut blocks,
        &mut lim,
    );
    if !commit_ids.is_empty() {
        sections.push("commits".into());
    }

    let comment_ids = list_section(
        "comments",
        "Comments",
        "github.comments",
        arr(bundle, "comments"),
        |i, c| {
            let id = format!(
                "comment:{}",
                n(c, &["id"])
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| i.to_string())
            );
            let who = s(c, &["user", "login"]).unwrap_or("?");
            let body = s(c, &["body"]).unwrap_or("");
            let mut e = el(&id, "message", "github.comment");
            e.text = Some(body.into());
            e.attrs.insert("github.author".into(), json!(who));
            e.attrs
                .insert("github.createdAt".into(), json!(s(c, &["created_at"])));
            (
                e,
                Entry {
                    el: id,
                    primary: format!("{who} · {}", day(s(c, &["created_at"]))),
                    secondary: Some(first_line(body)),
                    tone: Tone::Neutral,
                },
            )
        },
        &mut elements,
        &mut blocks,
        &mut lim,
    );
    for id in &comment_ids {
        relations.push(rel(id, "pr", "repliesTo"));
    }
    if !comment_ids.is_empty() {
        sections.push("comments".into());
    }
    lim.omitted.push(Note::new(
        Some("pr"),
        "inline review threads, check runs and commit statuses",
        "not part of the bundle this adapter version reads",
    ));

    // --- state --------------------------------------------------------------
    let ts = |k: &str| s(pr, &[k]).map(str::to_string);
    let mut f = |key: &str, value: Value, basis: Basis, volatile: bool| {
        let mut x = fact("pr", key, value, basis, volatile);
        x.source_time = ts("updated_at");
        facts.push(x);
    };
    f("state", json!(state), Basis::Source, true);
    f("merged", json!(merged), Basis::Source, true);
    f("draft", json!(draft), Basis::Source, true);
    f(
        "mergeable",
        pr.get("mergeable").cloned().unwrap_or(Value::Null),
        Basis::Source,
        true,
    );
    f(
        "mergeableState",
        json!(ts("mergeable_state")),
        Basis::Source,
        true,
    );
    f("headSha", json!(head_sha), Basis::Source, true);
    f("updatedAt", json!(ts("updated_at")), Basis::Source, true);
    f(
        "requestedReviewers",
        json!(arr(pr, "requested_reviewers")
            .iter()
            .filter_map(|r| s(r, &["login"]))
            .collect::<Vec<_>>()),
        Basis::Source,
        true,
    );
    let decision = if reviews.is_empty() && bundle.get("reviews").is_none() {
        Value::Null
    } else if latest.values().any(|s| s == "CHANGES_REQUESTED") {
        json!("changes_requested")
    } else if latest.values().any(|s| s == "APPROVED") {
        json!("approved")
    } else {
        json!("none")
    };
    f("reviewDecision", decision, Basis::Derived, true);
    lim.derived.push(Note::new(Some("pr"), "reviewDecision", "latest approving/requesting review per reviewer in the bundle; ignores branch protection and CODEOWNERS, so it can differ from GitHub's own decision"));
    if pr.get("mergeable").map(Value::is_null).unwrap_or(true) {
        lim.unverified.push(Note::new(
            Some("pr"),
            "mergeability",
            "GitHub had not computed it (null) in the captured response",
        ));
    }

    // --- capabilities -----------------------------------------------------------
    if let Some(url) = &html_url {
        caps.push(nav("open-pr", "pr", "Open the pull request on GitHub", url));
    }
    for c in arr(bundle, "commits") {
        if let (Some(sha), Some(url)) = (s(c, &["sha"]), s(c, &["html_url"])) {
            let short: String = sha.chars().take(7).collect();
            caps.push(nav(
                &format!("open-commit.{short}"),
                &format!("commit:{short}"),
                "Open the commit",
                url,
            ));
        }
    }
    let api = repo.as_ref().map(|r| format!("api.github.com /repos/{r}"));
    if let (Some(api), true) = (&api, state == "open" && !merged) {
        caps.push(origin(
            "merge",
            CapabilityKind::Mutation,
            "Merge the pull request",
            &format!("PUT {api}/pulls/{number}/merge"),
            json!({"type": "object", "required": ["sha"], "properties": {
                "merge_method": {"enum": ["merge", "squash", "rebase"]},
                "commit_title": {"type": "string"}, "commit_message": {"type": "string"},
                "sha": {"const": head_sha, "description": "the head commit observed at capture; GitHub refuses the merge if the head has moved"}}}),
            vec!["head is merged into base; pull request becomes merged".into()],
            "repository contents: write",
            vec!["state is open and not draft (true at capture: see state)".into(), "mergeable (see state.mergeable; may be unknown)".into(), "head still at the captured headSha".into()],
            &format!("GET {api}/pulls/{number} reports merged: true"),
        ));
        caps.push(origin(
            "review",
            CapabilityKind::Communication,
            "Submit a review",
            &format!("POST {api}/pulls/{number}/reviews"),
            json!({"type": "object", "required": ["event"], "properties": {
                "event": {"enum": ["APPROVE", "REQUEST_CHANGES", "COMMENT"]}, "body": {"type": "string"},
                "commit_id": {"const": head_sha}}}),
            vec!["a review by the acting account".into()],
            "pull requests: write",
            vec!["state is open".into()],
            &format!("GET {api}/pulls/{number}/reviews lists the new review"),
        ));
        caps.push(origin(
            "close",
            CapabilityKind::Mutation,
            "Close without merging",
            &format!("PATCH {api}/pulls/{number}"),
            json!({"type": "object", "required": ["state"], "properties": {"state": {"const": "closed"}}}),
            vec!["pull request state becomes closed".into()],
            "pull requests: write",
            vec!["state is open".into()],
            &format!("GET {api}/pulls/{number} reports state: closed"),
        ));
    }
    if let Some(api) = &api {
        caps.push(origin(
            "comment",
            CapabilityKind::Communication,
            "Comment on the conversation",
            &format!("POST {api}/issues/{number}/comments"),
            json!({"type": "object", "required": ["body"], "properties": {"body": {"type": "string"}}}),
            vec!["a comment by the acting account".into()],
            "issues: write (or pull requests: write)",
            vec!["the conversation is not locked (not captured)".into()],
            &format!("GET {api}/issues/{number}/comments lists the new comment"),
        ));
    }

    lim.approximated.push(Note::new(
        Some(VIEW),
        "typeface and styling",
        "a generic document layout in xeet's embedded fonts; github.com's UI is not replicated",
    ));
    lim.unverified.push(Note::new(
        None,
        "source authenticity",
        "the bundle is accepted as given; the adapter does not verify it came from GitHub",
    ));

    // --- assemble the tree --------------------------------------------------------
    root.children = vec!["pr.ref".into(), "pr.title".into(), "pr.state".into()];
    root.children.extend(label_ids);
    root.children.push("pr.summary".into());
    root.children.extend(stat_ids);
    root.children.push("pr.body".into());
    root.children.extend(sections);
    let mut all = vec![root, r, t, st, sum, author, base, head, body];
    all.extend(elements);

    let mut timestamps = std::collections::BTreeMap::new();
    for k in ["created_at", "updated_at", "closed_at", "merged_at"] {
        if let Some(v) = ts(k) {
            timestamps.insert(k.to_string(), v);
        }
    }
    Ok(Built {
        blocks,
        elements: all,
        relations,
        facts,
        caps,
        lim,
        subject: Subject {
            kind: "github.pull_request".into(),
            reference: html_url.clone(),
            title: format!("{reference} {title}"),
            element: "pr".into(),
        },
        source: SourceRef {
            kind: "github".into(),
            reference: html_url,
            id: n(pr, &["id"]).map(|x| x.to_string()),
            version: s(bundle, &["apiVersion"]).map(|v| format!("rest-api@{v}")),
            timestamps,
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn list_section(
    id: &str,
    title: &str,
    role: &str,
    items: &[Value],
    map: impl Fn(usize, &Value) -> (Element, Entry),
    elements: &mut Vec<Element>,
    blocks: &mut Vec<Block>,
    lim: &mut Limitations,
) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }
    let mut list = el(id, "list", role);
    list.text = Some(title.into());
    let mut entries = Vec::new();
    let mut ids = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let (mut e, entry) = map(i, item);
        if i < MAX_ROWS {
            entries.push(entry);
        } else {
            e.visible = false;
        }
        list.children.push(e.id.clone());
        ids.push(e.id.clone());
        elements.push(e);
    }
    if items.len() > MAX_ROWS {
        lim.omitted.push(Note::new(
            Some(id),
            format!(
                "{} of {} {id} are not shown in the view",
                items.len() - MAX_ROWS,
                items.len()
            ),
            format!("the view lists at most {MAX_ROWS}; all are in the semantic graph"),
        ));
    }
    blocks.push(Block::Section {
        el: id.into(),
        title: format!("{title} ({})", items.len()),
        items: entries,
    });
    elements.push(list);
    ids
}

fn rel(from: &str, to: &str, kind: &str) -> Relation {
    Relation {
        from: from.into(),
        to: to.into(),
        kind: kind.into(),
        basis: Basis::Source,
    }
}

fn nav(id: &str, target: &str, title: &str, url: &str) -> Capability {
    Capability {
        id: id.into(),
        kind: CapabilityKind::Navigate,
        title: title.into(),
        target: target.into(),
        input_schema: json!({"type": "object", "properties": {}}),
        effects: vec!["none at the source".into()],
        authority: Authority {
            required: vec![],
            checked_by: "none".into(),
        },
        preconditions: vec!["the URL still resolves and the viewer can read the repository".into()],
        executor: Executor {
            kind: ExecutorKind::UserAgent,
            reference: url.into(),
        },
        verification: "the user agent reaches the URL".into(),
        availability: Availability::Delegated,
    }
}

#[allow(clippy::too_many_arguments)]
fn origin(
    id: &str,
    kind: CapabilityKind,
    title: &str,
    operation: &str,
    schema: Value,
    effects: Vec<String>,
    scope: &str,
    preconditions: Vec<String>,
    verification: &str,
) -> Capability {
    Capability {
        id: id.into(),
        kind,
        title: title.into(),
        target: "pr".into(),
        input_schema: schema,
        effects,
        authority: Authority {
            required: vec![format!("GitHub token with {scope} on the repository")],
            checked_by: "api.github.com".into(),
        },
        preconditions,
        executor: Executor {
            kind: ExecutorKind::Origin,
            reference: operation.into(),
        },
        verification: verification.into(),
        availability: Availability::Delegated,
    }
}

fn view_options(config: &Value) -> Result<ViewOptions, String> {
    let layout = config.get("layoutVersion").and_then(Value::as_str);
    if layout != Some(view::LAYOUT_VERSION) {
        return Err(format!(
            "view layoutVersion {layout:?} is not reproducible by layout {}",
            view::LAYOUT_VERSION
        ));
    }
    serde_json::from_value(config.clone()).map_err(|e| e.to_string())
}

/// Renders the view for `config` from the preserved bundle bytes.
pub fn render_view(bundle_json: &[u8], config: &Value) -> Result<Vec<u8>, String> {
    let built = build(&parse(bundle_json)?)?;
    Ok(view::render(&built.blocks, &view_options(config)?).png)
}

pub fn capture(c: &Capture) -> Result<Package, String> {
    let bundle = parse(c.bundle_json)?;
    let mut built = build(&bundle)?;
    let opts = ViewOptions {
        width: c.width,
        theme: c.theme,
        scale: c.scale,
    };
    let rendered = view::render(&built.blocks, &opts);
    let mut config = serde_json::to_value(&opts).unwrap();
    config["layoutVersion"] = json!(view::LAYOUT_VERSION);

    for e in &mut built.elements {
        if let Some(r) = rendered.bounds.get(&e.id) {
            e.bounds.push(at(VIEW, *r));
        }
    }
    for id in &rendered.clamped {
        built.lim.omitted.push(Note::new(
            Some(id),
            "text clamped in the view",
            "full text is in the element",
        ));
    }

    let mut store = AssetStore::default();
    let input = store.add(c.bundle_json.to_vec(), "application/json");
    let png_asset = store.add(rendered.png.clone(), "image/png");
    let (pw, ph) = verify::png_size(&rendered.png).ok_or("view produced no PNG")?;
    let renderer = Tool {
        name: view::RENDERER.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        ruleset: Some(format!("layout@{}", view::LAYOUT_VERSION)),
    };

    let artifact = Artifact {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION.into(),
        id: String::new(),
        subject: built.subject,
        provenance: Provenance {
            source: built.source,
            capture: xeet_artifact::Capture {
                captured_at: c.captured_at.clone(),
                method: c.method.clone(),
                captured_by: tool("github"),
            },
            adapter: tool("github"),
            inputs: vec![AssetUse {
                asset: input.clone(),
                role: INPUT_ROLE.into(),
            }],
            transformations: vec![
                Transformation {
                    step: "map-semantics".into(),
                    tool: tool("github"),
                    inputs: vec![input.clone()],
                    outputs: vec![
                        "semantic".into(),
                        "state".into(),
                        "capabilities".into(),
                        "view-blocks".into(),
                    ],
                    deterministic: true,
                },
                Transformation {
                    step: "layout-and-rasterize".into(),
                    tool: renderer.clone(),
                    inputs: vec!["view-blocks".into(), "views.v0.config".into()],
                    outputs: vec![png_asset.clone(), "bounds".into()],
                    deterministic: true,
                },
            ],
            authenticity: Authenticity::Unverified,
            derived_from: None,
        },
        views: vec![View {
            id: VIEW.into(),
            asset: png_asset,
            media_type: "image/png".into(),
            pixel_width: pw,
            pixel_height: ph,
            scale: rendered_scale(&opts),
            layout_width: rendered.width,
            layout_height: rendered.height,
            renderer,
            config,
            fidelity: Fidelity {
                target:
                    "the pull request's content as a generic document; not a replica of github.com"
                        .into(),
                claim: FidelityClaim::Generic,
                evidence: None,
            },
        }],
        semantic: Semantic {
            root: "pr".into(),
            elements: built.elements,
            relations: built.relations,
        },
        state: State {
            as_of: c.captured_at.clone(),
            temporal: "historical-observation".into(),
            facts: built.facts,
        },
        capability_policy: CAPABILITY_POLICY.into(),
        capabilities: built.caps,
        limitations: built.lim,
        coverage: [
            ("visual", Support::Partial),
            ("semantic", Support::Supported),
            ("bounds", Support::Partial),
            ("provenance", Support::Supported),
            ("state", Support::Partial),
            ("capabilities", Support::Supported),
            ("reproduction", Support::Supported),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        assets: Vec::new(),
    };
    Ok(store.seal(artifact))
}

fn rendered_scale(opts: &ViewOptions) -> f32 {
    if opts.scale.is_finite() && opts.scale > 0.0 {
        opts.scale.clamp(1.0, 3.0)
    } else {
        2.0
    }
}
