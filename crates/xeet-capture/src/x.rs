//! X posts → artifacts.
//!
//! The visual projection is xeet-core's X renderer, unchanged: same rules,
//! same layout, same bytes as `xeet render`. The semantic projection comes from
//! the same run: the `Report` (decisions: visible text, entities, hidden
//! ranges, which attachment won) and the frame's named boxes, which are the
//! geometry the parity harness gates against Chrome.
//!
//! Everything X-specific lives here, as `x.*` roles and attributes.

use serde_json::{json, Value};
use xeet_artifact::*;
use xeet_core::limits::Limits;
use xeet_core::model::{Post, RenderOptions, Report, Verified};
use xeet_core::render as xr;

use crate::{at, el, fact, tool};

pub const ADAPTER: &str = "xeet-capture/x";
pub const INPUT_ROLE: &str = "source-post";
const VIEW: &str = "v0";

/// What the caller hands the adapter.
pub struct Capture<'a> {
    /// The post JSON (`model::Post`) exactly as received. Preserved verbatim.
    pub post_json: &'a [u8],
    pub options: RenderOptions,
    /// ISO 8601. The adapter has no clock.
    pub captured_at: String,
    /// `draft`, `source-payload` (resolved from an X page), or `fixture`.
    pub method: String,
    pub source_url: Option<String>,
    pub source_id: Option<String>,
    /// Other source bytes worth preserving, with their role (for example the
    /// `xeet resolve` envelope the post was extracted from).
    pub extra_inputs: Vec<(Vec<u8>, String)>,
}

/// The role `xeet resolve` output is preserved under.
pub const ENVELOPE_ROLE: &str = "resolve-envelope";

/// What `xeet resolve` printed, split into the post to render and the source
/// it named. `None` when `raw` is a bare post.
pub struct Envelope {
    /// Canonical JSON of `envelope.post`: the exact bytes that get rendered.
    pub post_json: Vec<u8>,
    pub url: Option<String>,
    pub id: Option<String>,
}

pub fn envelope(raw: &[u8]) -> Option<Envelope> {
    let value: Value = serde_json::from_slice(raw).ok()?;
    if value.get("post").is_none() || value.get("text").is_some() {
        return None;
    }
    let src = value.get("source");
    let s = |k: &str| {
        src.and_then(|v| v.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Some(Envelope {
        post_json: xeet_artifact::canon::canonical(&value["post"]).into_bytes(),
        url: s("url"),
        id: s("id"),
    })
}

fn parse(post_json: &[u8], opts: &RenderOptions) -> Result<Post, String> {
    let text = std::str::from_utf8(post_json).map_err(|e| format!("post is not UTF-8: {e}"))?;
    xr::load_post(text, opts, &Limits::standard()).map_err(|e| e.to_string())
}

/// Renders the view for `config` (a `RenderOptions` JSON) from the preserved
/// post bytes. Used by capture and by reproduction, so both run one path.
pub fn render_view(post_json: &[u8], config: &Value) -> Result<Vec<u8>, String> {
    let opts: RenderOptions = serde_json::from_value(config.clone()).map_err(|e| e.to_string())?;
    let post = parse(post_json, &opts)?;
    xr::render(&post, &opts)
        .map(|(png, _)| png)
        .map_err(|e| e.to_string())
}

pub fn capture(c: &Capture) -> Result<Package, String> {
    let opts = &c.options;
    let post = parse(c.post_json, opts)?;
    let (png, report) = xr::render(&post, opts).map_err(|e| e.to_string())?;
    let boxes = xr::frame_boxes(&post, opts);
    let ruleset = report.ruleset.clone();

    let mut store = AssetStore::default();
    let input = store.add(c.post_json.to_vec(), "application/json");
    let mut inputs = vec![AssetUse {
        asset: input.clone(),
        role: INPUT_ROLE.into(),
    }];
    let mut transformations = Vec::new();
    for (bytes, role) in &c.extra_inputs {
        let asset = store.add(bytes.clone(), "application/json");
        if role == ENVELOPE_ROLE {
            transformations.push(Transformation {
                step: "extract-post (canonical JSON of envelope.post)".into(),
                tool: tool("x"),
                inputs: vec![asset.clone()],
                outputs: vec![input.clone()],
                deterministic: true,
                at: None,
            });
        }
        inputs.push(AssetUse {
            asset,
            role: role.clone(),
        });
    }
    let view_asset = store.add(png.clone(), "image/png");
    let (pw, ph) = verify::png_size(&png).ok_or("renderer produced no PNG")?;

    let box_at = |name: &str| boxes.get(name).map(|r| at(VIEW, *r));
    let mut elements: Vec<Element> = Vec::new();
    let mut relations: Vec<Relation> = Vec::new();
    let mut facts: Vec<Fact> = Vec::new();
    let mut caps: Vec<Capability> = Vec::new();
    let mut lim = Limitations::default();

    // --- author -------------------------------------------------------------
    let a = &post.author;
    let mut author = el("author", "identity", "x.author");
    author.text = Some(a.name.clone());
    author.value = Some(json!(a.handle));
    author.attrs.insert("x.handle".into(), json!(a.handle));
    author.attrs.insert(
        "x.verified".into(),
        serde_json::to_value(a.verified).unwrap(),
    );
    author
        .attrs
        .insert("x.protected".into(), json!(a.protected));
    // No bounds of its own: the header row also holds the timestamp and the
    // menu, so its box would overclaim. The children carry exact boxes.
    let mut name = el("author.name", "text", "x.author.name");
    name.text = Some(a.name.clone());
    name.bounds.extend(box_at("name"));
    let mut handle = el("author.handle", "text", "x.author.handle");
    handle.text = Some(format!("@{}", a.handle));
    handle.bounds.extend(box_at("handle"));
    let mut avatar = el("author.avatar", "media", "x.author.avatar");
    avatar
        .attrs
        .insert("x.hasBytes".into(), json!(a.avatar.is_some()));
    avatar.bounds.extend(box_at("avatar"));
    if a.avatar.is_none() {
        lim.approximated.push(Note::new(
            Some("author.avatar"),
            "avatar drawn as a neutral placeholder",
            "no avatar bytes in the source data",
        ));
    }
    author.children = vec![
        "author.name".into(),
        "author.handle".into(),
        "author.avatar".into(),
    ];
    let mut author_elems = vec![name, handle, avatar];
    if a.verified != Verified::None {
        let mut badge = el("author.badge", "status", "x.verified");
        badge.value = Some(serde_json::to_value(a.verified).unwrap());
        badge.bounds.extend(box_at("badge"));
        author.children.push("author.badge".into());
        author_elems.push(badge);
    }
    facts.push(fact(
        "author",
        "verified",
        serde_json::to_value(a.verified).unwrap(),
        Basis::Source,
        true,
    ));

    // --- post root ------------------------------------------------------------
    let mut root = el("post", "record", "x.post");
    root.text = Some(report.text.visible.clone());
    root.value = c.source_id.clone().map(Value::String);
    root.attrs.insert("x.ruleset".into(), json!(ruleset));
    root.attrs.insert(
        "x.surface".into(),
        serde_json::to_value(opts.surface).unwrap(),
    );
    root.bounds.extend(box_at("cell"));
    root.children.push("author".into());
    relations.push(rel("post", "author", "authoredBy", Basis::Source));

    if let Some(ctx) = post.social_context {
        let mut row = el("socialContext", "status", "x.socialContext");
        row.text = Some(ctx.label().to_string());
        row.bounds.extend(box_at("pinnedRow"));
        root.children.push("socialContext".into());
        elements.push(row);
    }

    // --- timestamp ------------------------------------------------------------
    if let Some(created) = &post.created_at {
        let mut ts = el("timestamp", "timestamp", "x.createdAt");
        ts.value = Some(json!(created));
        let label = if opts.surface.is_ios() {
            xeet_core::frame::time::label_ios(
                Some(created),
                post.now.as_deref(),
                opts.surface.is_detail(),
            )
        } else {
            xeet_core::frame::time::label(
                Some(created),
                post.now.as_deref(),
                opts.surface.is_detail(),
            )
        };
        ts.text = Some(label);
        ts.attrs.insert("x.relativeTo".into(), json!(post.now));
        ts.bounds.extend(box_at("timestamp"));
        root.children.push("timestamp".into());
        elements.push(ts);
        lim.derived.push(Note::new(
            Some("timestamp"),
            "the displayed label is computed from createdAt relative to the post's `now`",
            "xeet-core has no clock; `now` is an input",
        ));
    } else {
        lim.omitted.push(Note::new(
            Some("post"),
            "post time",
            "not in the source data; rendered as a just-sent post",
        ));
    }

    // --- body + entities --------------------------------------------------------
    let mut body = el("body", "text", "x.body");
    body.text = Some(report.text.visible.clone());
    body.attrs.insert("x.authoredText".into(), json!(post.text));
    body.attrs
        .insert("x.lines".into(), json!(report.text.lines));
    body.basis = Basis::Derived;
    body.visible = !report.text.visible.is_empty();
    if body.visible {
        body.bounds.extend(box_at("text"));
    }
    for (i, e) in report.entities.iter().enumerate() {
        let id = format!("entity.{i}");
        let (kind, role) = match e.kind.as_str() {
            "url" => ("link", "x.url"),
            "mention" => ("reference", "x.mention"),
            "hashtag" => ("label", "x.hashtag"),
            "cashtag" => ("label", "x.cashtag"),
            _ => ("reference", "x.entity"),
        };
        let mut en = el(&id, kind, role);
        let shown = e.display.clone().unwrap_or_else(|| e.text.clone());
        en.text = Some(shown.clone());
        en.value = Some(json!(e.text));
        en.attrs.insert("x.range".into(), json!(e.range));
        en.attrs.insert("x.rangeUnit".into(), json!("utf16"));
        en.basis = Basis::Derived;
        en.visible = report.text.visible.contains(shown.as_str());
        body.children.push(id.clone());
        if e.kind == "url" && en.visible {
            caps.push(navigate(
                &format!("open-link.{i}"),
                &id,
                &format!("Open {shown}"),
                &e.text,
            ));
        }
        elements.push(en);
    }
    for h in &report.hidden {
        lim.omitted.push(Note::new(
            Some("body"),
            format!("authored text range {:?} (utf16) is not shown", h.range),
            h.why.clone(),
        ));
    }
    if report.text.truncated || report.text.show_more.is_some() {
        lim.omitted.push(Note::new(
            Some("body"),
            "the tail of the body is cut behind \"Show more\"",
            "X's timeline rules; the full text is in body.attrs[\"x.authoredText\"]",
        ));
    }
    if !report.entities.is_empty() {
        lim.omitted.push(Note::new(
            Some("body"),
            "per-entity bounds",
            "the frame records line boxes, not entity boxes; entities carry text ranges instead",
        ));
    }
    root.children.push("body".into());

    // --- attachment --------------------------------------------------------------
    attachment(
        &post,
        &report,
        &box_at("attachment"),
        &mut root,
        &mut elements,
        &mut relations,
        &mut lim,
    );

    // --- action row -------------------------------------------------------------
    let mut row = el("actions", "group", "x.actionRow");
    row.bounds.extend(box_at("actionRow"));
    let counts = post.counts.as_ref();
    for (id, label, pill, count) in [
        (
            "action.reply",
            "reply",
            "replyPill",
            counts.map(|c| c.replies),
        ),
        (
            "action.repost",
            "repost",
            "retweetPill",
            counts.map(|c| c.reposts),
        ),
        ("action.like", "like", "likePill", counts.map(|c| c.likes)),
        (
            "action.views",
            "views",
            "viewsPill",
            counts.map(|c| c.views),
        ),
        (
            "action.bookmark",
            "bookmark",
            "bookmark",
            counts.map(|c| c.bookmarks),
        ),
        ("action.share", "share", "share", None),
    ] {
        let mut a = el(id, "affordance", &format!("x.action.{label}"));
        a.value = count.map(|n| json!(n));
        if count.is_some() {
            a.unit = Some(label.to_string());
        }
        a.bounds.extend(box_at(pill));
        a.visible = !a.bounds.is_empty();
        row.children.push(id.to_string());
        elements.push(a);
        if let Some(n) = count {
            facts.push(fact(
                "post",
                &format!("{label}Count"),
                json!(n),
                Basis::Source,
                true,
            ));
        }
    }
    if counts.is_none() {
        lim.omitted.push(Note::new(
            Some("actions"),
            "engagement counts",
            "not in the source data; the row is drawn without numbers",
        ));
    }
    root.children.push("actions".into());

    // --- state --------------------------------------------------------------------
    facts.push(fact(
        "post",
        "createdAt",
        json!(post.created_at),
        Basis::Source,
        false,
    ));
    facts.push(fact(
        "post",
        "inReplyTo",
        json!(post.reply_to),
        Basis::Source,
        false,
    ));
    facts.push(fact(
        "post",
        "pinned",
        json!(post.social_context.is_some()),
        Basis::Source,
        true,
    ));
    facts.push(fact(
        "post",
        "longPost",
        json!(post
            .long_post
            .unwrap_or(report.length.weighted > report.length.limit)),
        if post.long_post.is_some() {
            Basis::Source
        } else {
            Basis::Derived
        },
        false,
    ));
    facts.push(fact(
        "post",
        "weightedLength",
        json!(report.length.weighted),
        Basis::Derived,
        false,
    ));
    facts.push(fact(
        "post",
        "lengthValid",
        json!(report.length.valid),
        Basis::Derived,
        false,
    ));
    // `none` is a decision, not an absence of knowledge.
    facts.push(fact(
        "body",
        "showMore",
        json!(report.text.show_more.as_deref().unwrap_or("none")),
        Basis::Derived,
        false,
    ));
    let existed = if c.method == "source-payload" {
        json!(true)
    } else {
        Value::Null
    };
    facts.push(fact("post", "existsAtSource", existed, Basis::Source, true));

    // --- capabilities ---------------------------------------------------------------
    let post_url = c.source_url.clone().or_else(|| {
        c.source_id
            .as_ref()
            .map(|id| format!("https://x.com/{}/status/{id}", post.author.handle))
    });
    if let Some(url) = &post_url {
        caps.push(navigate("open-post", "post", "Open the post on X", url));
    }
    if !post.author.handle.is_empty() {
        caps.push(navigate(
            "open-author",
            "author",
            "Open the author's profile",
            &format!("https://x.com/{}", post.author.handle),
        ));
    }
    if report.text.has_show_more && !opts.expanded {
        caps.push(projection(
            "expand",
            "body",
            "Show the full post body (\"Show more\")",
            json!({"type": "object", "additionalProperties": false, "properties": {}}),
            "a derived artifact whose view has `expanded: true`",
        ));
    }
    caps.push(projection(
        "retheme",
        "post",
        "Render the same observation in another theme",
        json!({"type": "object", "required": ["theme"], "additionalProperties": false,
               "properties": {"theme": {"enum": ["light", "dark"]}}}),
        "a derived artifact whose view uses the given theme",
    ));
    if post_url.is_some() {
        for (id, kind, title, effect) in [
            (
                "reply",
                CapabilityKind::Communication,
                "Reply to the post",
                "a reply post by the acting account",
            ),
            (
                "repost",
                CapabilityKind::Mutation,
                "Repost",
                "the post is reposted by the acting account; repost count +1",
            ),
            (
                "like",
                CapabilityKind::Mutation,
                "Like",
                "the post is liked by the acting account; like count +1",
            ),
            (
                "bookmark",
                CapabilityKind::Mutation,
                "Bookmark",
                "the post is bookmarked privately by the acting account",
            ),
        ] {
            caps.push(Capability {
                id: id.to_string(),
                kind,
                title: title.to_string(),
                target: "post".to_string(),
                input_schema: if id == "reply" {
                    json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}})
                } else {
                    json!({"type": "object", "properties": {}})
                },
                effects: vec![effect.to_string()],
                authority: Authority { required: vec!["x.com signed-in account".to_string()], checked_by: "x.com".to_string() },
                preconditions: vec![
                    "the post still exists (true at capture only if existsAtSource)".to_string(),
                    "the acting account is permitted by the author's settings (not captured)".to_string(),
                ],
                executor: Executor { kind: ExecutorKind::Origin, reference: post_url.clone().unwrap() },
                verification: "re-capture the post from X and compare state; not verifiable from this artifact".to_string(),
                availability: Availability::Delegated,
            });
        }
    } else {
        lim.omitted.push(Note::new(
            Some("post"),
            "interaction capabilities (reply, repost, like)",
            "no source id or URL: this is not a post that exists on X",
        ));
    }

    // --- limitations & coverage ------------------------------------------------------
    for w in &report.warnings {
        lim.approximated
            .push(Note::new(None, w.clone(), "renderer warning"));
    }
    lim.derived.push(Note::new(
        Some("post"),
        "visible text, entities, hidden ranges, the Show more cut and the attachment choice",
        format!("decided by ruleset {ruleset}"),
    ));
    lim.derived.push(Note::new(None, "element bounds", "the frame's named layout boxes for this render (the geometry the repository gates against Chrome)"));
    lim.unverified.push(Note::new(
        None,
        "source authenticity",
        "the adapter maps the data it is given; it does not prove the post came from X",
    ));
    lim.unverified.push(Note::new(
        Some(VIEW),
        "pixel parity of this specific view against Chrome",
        "not compared at capture; the ruleset is oracle-gated in the repository's CI",
    ));
    if c.method == "draft" {
        lim.unverified.push(Note::new(
            Some("post"),
            "existence on X",
            "a draft: never observed on x.com",
        ));
    }

    let verified = report.verified_for.contains(&opts.surface);
    let fidelity = Fidelity {
        target: format!("x.com {} in Chrome", serde_json::to_value(opts.surface).unwrap().as_str().unwrap_or("")),
        claim: if verified { FidelityClaim::OracleVerified } else { FidelityClaim::None },
        evidence: verified.then(|| format!("ruleset {ruleset} lists this surface in verifiedFor; gates: crates/xeet-core/examples/frame_parity.rs, tests/golden/e2e.json")),
    };
    let renderer = Tool {
        name: "xeet-core".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        ruleset: Some(ruleset.clone()),
    };

    let mut all = vec![root];
    all.push(author);
    all.extend(author_elems);
    all.push(body);
    all.push(row);
    all.extend(elements);

    let artifact = Artifact {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION.into(),
        id: String::new(),
        subject: Subject {
            kind: "x.post".into(),
            reference: post_url.clone(),
            title: format!(
                "@{}: {}",
                post.author.handle,
                first_line(&report.text.visible)
            ),
            element: "post".into(),
        },
        provenance: Provenance {
            source: SourceRef {
                kind: "x".into(),
                reference: post_url,
                id: c.source_id.clone(),
                version: post.app_version.clone(),
                timestamps: post
                    .created_at
                    .iter()
                    .map(|t| ("createdAt".to_string(), t.clone()))
                    .collect(),
            },
            capture: xeet_artifact::Capture {
                captured_at: c.captured_at.clone(),
                method: c.method.clone(),
                captured_by: tool("x"),
            },
            adapter: tool("x"),
            inputs,
            transformations: transformations
                .into_iter()
                .chain([
                    Transformation {
                        step: "parse-and-validate".into(),
                        tool: renderer.clone(),
                        inputs: vec![input.clone()],
                        outputs: vec!["model::Post".into()],
                        deterministic: true,
                        at: None,
                    },
                    Transformation {
                        step: "decide-and-layout".into(),
                        tool: renderer.clone(),
                        inputs: vec!["model::Post".into(), "views.v0.config".into()],
                        outputs: vec!["report".into(), "geometry".into()],
                        deterministic: true,
                        at: None,
                    },
                    Transformation {
                        step: "rasterize".into(),
                        tool: renderer.clone(),
                        inputs: vec!["display-list".into()],
                        outputs: vec![view_asset.clone()],
                        deterministic: true,
                        at: None,
                    },
                    Transformation {
                        step: "map-semantics".into(),
                        tool: tool("x"),
                        inputs: vec!["report".into(), "geometry".into()],
                        outputs: vec!["semantic".into(), "state".into(), "capabilities".into()],
                        deterministic: true,
                        at: None,
                    },
                ])
                .collect(),
            authenticity: Authenticity::Unverified,
            derived_from: None,
        },
        views: vec![View {
            id: VIEW.into(),
            asset: view_asset,
            media_type: "image/png".into(),
            pixel_width: pw,
            pixel_height: ph,
            scale: opts.dpr,
            layout_width: report.size.width,
            layout_height: report.size.height,
            renderer,
            config: serde_json::to_value(opts).unwrap(),
            fidelity,
        }],
        semantic: Semantic {
            root: "post".into(),
            elements: all,
            relations,
        },
        state: State {
            as_of: c.captured_at.clone(),
            temporal: "historical-observation".into(),
            facts,
        },
        capability_policy: CAPABILITY_POLICY.into(),
        capabilities: caps,
        limitations: lim,
        coverage: [
            ("visual", Support::Supported),
            ("semantic", Support::Partial),
            ("bounds", Support::Partial),
            ("provenance", Support::Supported),
            ("state", Support::Partial),
            ("capabilities", Support::Partial),
            ("reproduction", Support::Supported),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        assets: Vec::new(),
    };
    Ok(store.seal(artifact))
}

/// Runs a local projection capability: a new artifact of the same observation
/// with different view options, linked to its parent. The source is never
/// contacted; the parent's preserved input is the only input.
pub fn invoke(
    parent: &Artifact,
    package: &Package,
    capability: &str,
    input: &Value,
) -> Result<Package, String> {
    let cap = parent
        .capabilities
        .iter()
        .find(|c| c.id == capability)
        .ok_or_else(|| format!("no capability {capability}"))?;
    if cap.availability != Availability::LocalExecutable || cap.executor.kind != ExecutorKind::Local
    {
        return Err(format!(
            "{capability} is {:?} via {:?} {}: it needs authority this artifact cannot grant",
            cap.availability, cap.executor.kind, cap.executor.reference
        ));
    }
    let input_use = parent
        .provenance
        .inputs
        .iter()
        .find(|i| i.role == INPUT_ROLE)
        .ok_or("no preserved post")?;
    let bytes = package
        .asset_bytes(&input_use.asset)
        .ok_or("preserved post missing")?;
    let view = parent.views.first().ok_or("no view")?;
    let mut opts: RenderOptions =
        serde_json::from_value(view.config.clone()).map_err(|e| e.to_string())?;
    match capability {
        "expand" => opts.expanded = true,
        "retheme" => {
            opts.theme = serde_json::from_value(
                input
                    .get("theme")
                    .cloned()
                    .ok_or("retheme needs {\"theme\"}")?,
            )
            .map_err(|e| format!("theme: {e}"))?
        }
        other => return Err(format!("{other} has no local executor in {ADAPTER}")),
    }
    let child = capture(&Capture {
        post_json: bytes,
        options: opts,
        captured_at: parent.provenance.capture.captured_at.clone(),
        method: parent.provenance.capture.method.clone(),
        source_url: parent.provenance.source.reference.clone(),
        source_id: parent.provenance.source.id.clone(),
        extra_inputs: parent
            .provenance
            .inputs
            .iter()
            .filter(|i| i.role != INPUT_ROLE)
            .filter_map(|i| Some((package.asset_bytes(&i.asset)?.to_vec(), i.role.clone())))
            .collect(),
    })?;
    let mut artifact = child.artifact()?;
    artifact.provenance.derived_from = Some(parent.id.clone());
    artifact.provenance.transformations.insert(
        0,
        Transformation {
            step: format!("local-projection:{capability}"),
            tool: tool("x"),
            inputs: vec![parent.id.clone(), input.to_string()],
            outputs: vec!["views.v0.config".into()],
            deterministic: true,
            at: None,
        },
    );
    let mut store = AssetStore::default();
    for asset in &artifact.assets {
        store.add(child.files[&asset.path].clone(), &asset.media_type);
    }
    Ok(store.seal(artifact))
}

fn attachment(
    post: &Post,
    report: &Report,
    slot: &Option<Bounds>,
    root: &mut Element,
    elements: &mut Vec<Element>,
    relations: &mut Vec<Relation>,
    lim: &mut Limitations,
) {
    let shown = report.attachment.as_ref().map(|a| a.shown.as_str());
    // Media first: shown as the attachment, or present in the data and not shown.
    if !post.media.is_empty() {
        let visible = shown == Some("media");
        let mut group = el("media", "group", "x.media");
        group.visible = visible;
        if visible {
            group.bounds.extend(slot.clone());
        } else {
            lim.omitted.push(Note::new(
                Some("media"),
                "attached media is not shown",
                report
                    .attachment
                    .as_ref()
                    .map(|a| a.why.clone())
                    .unwrap_or_else(|| "another attachment won".into()),
            ));
        }
        for (i, m) in post.media.iter().enumerate() {
            let id = format!("media.{i}");
            let mut e = el(&id, "media", "x.media.item");
            e.visible = visible;
            e.text = m.alt.clone();
            e.attrs
                .insert("x.kind".into(), serde_json::to_value(m.kind).unwrap());
            e.attrs.insert("x.width".into(), json!(m.width));
            e.attrs.insert("x.height".into(), json!(m.height));
            e.attrs.insert("x.hasBytes".into(), json!(m.data.is_some()));
            if visible && m.data.is_none() {
                lim.approximated.push(Note::new(
                    Some(&id),
                    "drawn as a labeled placeholder with the correct geometry",
                    "no image bytes in the source data",
                ));
            }
            group.children.push(id.clone());
            relations.push(rel(&id, "post", "attachedTo", Basis::Source));
            elements.push(e);
        }
        root.children.push("media".into());
        elements.push(group);
    }
    match shown {
        Some("card") => {
            let url = report
                .attachment
                .as_ref()
                .and_then(|a| a.url.clone())
                .unwrap_or_default();
            let meta = post.links.get(&url).cloned().unwrap_or_default();
            let mut card = el("card", "link", "x.card");
            card.value = Some(json!(url));
            card.text = meta.title.clone();
            card.attrs.insert("x.cardType".into(), json!(meta.card));
            card.attrs
                .insert("x.description".into(), json!(meta.description));
            card.attrs.insert("x.domain".into(), json!(meta.domain));
            card.bounds.extend(slot.clone());
            if meta.image.as_ref().is_some_and(|i| i.data.is_none()) {
                lim.approximated.push(Note::new(
                    Some("card"),
                    "card image drawn as a placeholder with the correct geometry",
                    "no image bytes in the source data",
                ));
            }
            root.children.push("card".into());
            relations.push(rel("card", "post", "attachedTo", Basis::Derived));
            elements.push(card);
        }
        Some("quote") => {
            if let Some(q) = &post.quote {
                let mut quote = el("quote", "record", "x.quote");
                quote.text = Some(q.text.clone());
                quote.bounds.extend(slot.clone());
                let mut qa = el("quote.author", "identity", "x.author");
                qa.text = Some(q.author.name.clone());
                qa.value = Some(json!(q.author.handle));
                let mut qb = el("quote.body", "text", "x.body");
                qb.text = Some(q.text.clone());
                quote.children = vec!["quote.author".into(), "quote.body".into()];
                root.children.push("quote".into());
                relations.push(rel("post", "quote", "quotes", Basis::Source));
                relations.push(rel("quote", "quote.author", "authoredBy", Basis::Source));
                lim.omitted.push(Note::new(
                    Some("quote"),
                    "the quoted post's own visible-text decisions, entities and inner bounds",
                    "the adapter models one level; the quote's authored text is given instead",
                ));
                elements.extend([quote, qa, qb]);
            }
        }
        Some("poll") => {
            if let Some(p) = &post.poll {
                let mut poll = el("poll", "group", "x.poll");
                poll.bounds.extend(slot.clone());
                for (i, choice) in p.choices.iter().enumerate() {
                    let id = format!("poll.choice.{i}");
                    let mut e = el(&id, "text", "x.poll.choice");
                    e.text = Some(choice.clone());
                    if let Some(v) = p.votes.as_ref().and_then(|v| v.get(i)) {
                        e.value = Some(json!(v));
                        e.unit = Some("votes".into());
                    }
                    poll.children.push(id);
                    elements.push(e);
                }
                root.children.push("poll".into());
                elements.push(poll);
            }
        }
        _ => {}
    }
}

fn rel(from: &str, to: &str, kind: &str, basis: Basis) -> Relation {
    Relation {
        from: from.into(),
        to: to.into(),
        kind: kind.into(),
        basis,
    }
}

fn navigate(id: &str, target: &str, title: &str, url: &str) -> Capability {
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
        preconditions: vec!["the URL still resolves (true at capture time at most)".into()],
        executor: Executor {
            kind: ExecutorKind::UserAgent,
            reference: url.into(),
        },
        verification: "the user agent reaches the URL".into(),
        availability: Availability::Delegated,
    }
}

fn projection(id: &str, target: &str, title: &str, schema: Value, effect: &str) -> Capability {
    Capability {
        id: id.into(),
        kind: CapabilityKind::Projection,
        title: title.into(),
        target: target.into(),
        input_schema: schema,
        effects: vec![effect.into(), "nothing at the source".into()],
        authority: Authority { required: vec![], checked_by: "none".into() },
        preconditions: vec!["the artifact's source-post input is present".into()],
        executor: Executor { kind: ExecutorKind::Local, reference: format!("{ADAPTER} (xeet-core render)") },
        verification: "the derived artifact verifies, reproduces, and names this artifact in provenance.derivedFrom".into(),
        availability: Availability::LocalExecutable,
    }
}

pub(crate) fn first_line(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let short: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        format!("{short}…")
    } else {
        short
    }
}
