//! Image files → artifacts, without interpreting a single pixel.
//!
//! This adapter makes an image *addressable and describable*: its real format
//! (from magic bytes, never from a filename or declared type), its dimensions
//! (from the file header, without decoding), its digest, and the original
//! bytes as the view. What the image *shows* stays `visual-only` until a
//! reader (a vision model, OCR) adds it through `annotate`, where it is
//! recorded as inference.
//!
//! That is the honest division of labour: xeet can prove what file this is;
//! it cannot know what is in it.

use serde_json::json;
use xeet_artifact::*;

use crate::{el, fact, tool};

pub const ADAPTER: &str = "xeet-capture/image";
pub const INPUT_ROLE: &str = "source-image";
const VIEW: &str = "v0";

/// A detected image format and its pixel dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sniffed {
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
}

/// Identifies PNG, JPEG, GIF and WebP from their bytes and reads their
/// dimensions from the header. Needs only the first few KB for all but
/// JPEG, whose size marker can come after metadata segments.
pub fn sniff(b: &[u8]) -> Option<Sniffed> {
    let be16 = |i: usize| -> Option<u32> {
        Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32)
    };
    let le16 = |i: usize| -> Option<u32> {
        Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32)
    };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (w, h) = verify::png_size(b)?;
        return Some(Sniffed {
            media_type: "image/png",
            width: w,
            height: h,
        });
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some(Sniffed {
            media_type: "image/gif",
            width: le16(6)?,
            height: le16(8)?,
        });
    }
    if b.len() >= 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        let (w, h) = match &b[12..16] {
            b"VP8X" => {
                let w = 1 + (b[24] as u32 | (b[25] as u32) << 8 | (b[26] as u32) << 16);
                let h = 1 + (b[27] as u32 | (b[28] as u32) << 8 | (b[29] as u32) << 16);
                (w, h)
            }
            b"VP8L" => {
                let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                (1 + (bits & 0x3fff), 1 + ((bits >> 14) & 0x3fff))
            }
            b"VP8 " => (le16(26)? & 0x3fff, le16(28)? & 0x3fff),
            _ => return None,
        };
        return Some(Sniffed {
            media_type: "image/webp",
            width: w,
            height: h,
        });
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xff {
                return None;
            }
            let marker = b[i + 1];
            if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
                i += 2;
                continue;
            }
            let len = be16(i + 2)? as usize;
            // Start-of-frame markers carry the size (not DHT, JPG, DAC).
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return Some(Sniffed {
                    media_type: "image/jpeg",
                    width: be16(i + 7)?,
                    height: be16(i + 5)?,
                });
            }
            i += 2 + len;
        }
    }
    None
}

pub struct Capture<'a> {
    pub bytes: &'a [u8],
    pub captured_at: String,
    pub method: String,
    /// A name the caller gave (a filename). Recorded as declared, not trusted.
    pub declared_name: Option<String>,
    /// A media type the caller declared. Recorded and compared, not trusted.
    pub declared_type: Option<String>,
    /// For screenshots: the capture context JSON ([`CaptureContext`]) from the
    /// capturing application, preserved verbatim as a second input.
    pub context: Option<&'a [u8]>,
}

pub const CONTEXT_ROLE: &str = "capture-context";

/// What a capturing application observed alongside the pixels. Every field is
/// optional except `method` and `mode`: unknown is recorded as unknown.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureContext {
    /// The capturing application, e.g. `{"name": "Xeet Capture", "version": "0.1.0"}`.
    pub captured_by: Option<Tool>,
    /// `screencapturekit`, `screencapture-cli`, …
    pub method: String,
    /// `display`, `region` or `window`.
    pub mode: String,
    /// e.g. `macOS 15.1 (24B83)`.
    pub platform: Option<String>,
    pub display: Option<Display>,
    /// The captured rectangle in global points (all modes).
    pub frame: Option<[f64; 4]>,
    pub window: Option<Window>,
    pub frontmost_app: Option<App>,
    /// `screenRecording`, `accessibility` → `granted`, `denied`, `not-requested`, `unknown`.
    pub permissions: std::collections::BTreeMap<String, String>,
    /// The accessibility tree of the captured window, frames in global points.
    pub accessibility: Option<AxNode>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Display {
    pub id: u64,
    pub name: Option<String>,
    /// Global points.
    pub frame: [f64; 4],
    /// Pixels per point (2 on Retina).
    pub scale: f64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Window {
    pub id: u64,
    pub title: Option<String>,
    pub app: Option<App>,
    /// Global points.
    pub frame: [f64; 4],
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct App {
    pub name: Option<String>,
    pub bundle_id: Option<String>,
    pub pid: Option<i64>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AxNode {
    pub role: String,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub label: Option<String>,
    pub value: Option<String>,
    /// Global points.
    pub frame: Option<[f64; 4]>,
    pub children: Vec<AxNode>,
}

/// Accessibility nodes kept per capture; the rest are counted as omitted.
pub const AX_LIMIT: usize = 1500;

fn ax_kind(role: &str) -> &'static str {
    match role {
        "AXButton" | "AXMenuItem" | "AXMenuButton" | "AXLink" | "AXCheckBox" | "AXRadioButton"
        | "AXPopUpButton" | "AXTab" | "AXSlider" => "affordance",
        "AXStaticText" | "AXTextField" | "AXTextArea" | "AXSearchField" | "AXCell" => "text",
        "AXImage" => "media",
        "AXHeading" => "heading",
        "AXList" | "AXTable" | "AXOutline" => "list",
        _ => "group",
    }
}

/// Maps capture context onto the artifact: identity and geometry as
/// observations of the capturing app, the accessibility tree as a separate
/// source, permissions as facts, and every gap as a stated omission.
fn apply_context(
    ctx: &CaptureContext,
    img: (u32, u32),
    root: &mut Element,
    elements: &mut Vec<Element>,
    facts: &mut Vec<Fact>,
    lim: &mut Limitations,
) -> String {
    let observed = |id: &str, kind: &str, role: &str| {
        let mut e = el(id, kind, role);
        e.basis = Basis::Observed;
        e.visible = false;
        e
    };
    root.attrs.insert("capture.mode".into(), json!(ctx.mode));
    root.attrs
        .insert("capture.method".into(), json!(ctx.method));
    if let Some(f) = ctx.frame {
        root.attrs.insert("capture.frame".into(), json!(f));
    }
    facts.push(fact(
        "image",
        "captureMode",
        json!(ctx.mode),
        Basis::Observed,
        false,
    ));
    facts.push(fact(
        "image",
        "platform",
        json!(ctx.platform),
        Basis::Observed,
        false,
    ));
    for (k, v) in &ctx.permissions {
        facts.push(fact(
            "image",
            &format!("permission.{k}"),
            json!(v),
            Basis::Observed,
            true,
        ));
    }
    let mut title = String::new();
    if let Some(d) = &ctx.display {
        let mut e = observed("capture.display", "group", "capture.display");
        e.text = d.name.clone();
        e.attrs.insert("capture.displayId".into(), json!(d.id));
        e.attrs
            .insert("capture.displayFrame".into(), json!(d.frame));
        e.attrs.insert("capture.scale".into(), json!(d.scale));
        root.children.push(e.id.clone());
        elements.push(e);
        title = d
            .name
            .clone()
            .unwrap_or_else(|| format!("display {}", d.id));
    }
    if let Some(w) = &ctx.window {
        let mut e = observed("capture.window", "identity", "capture.window");
        e.text = w.title.clone();
        e.attrs.insert("capture.windowId".into(), json!(w.id));
        e.attrs.insert("capture.windowFrame".into(), json!(w.frame));
        if let Some(a) = &w.app {
            e.attrs
                .insert("capture.app".into(), serde_json::to_value(a).unwrap());
        }
        root.children.push(e.id.clone());
        elements.push(e);
        title = format!(
            "{}{}",
            w.app
                .as_ref()
                .and_then(|a| a.name.clone())
                .map(|n| format!("{n}: "))
                .unwrap_or_default(),
            w.title.clone().unwrap_or_else(|| "untitled window".into())
        );
    }
    if let Some(a) = &ctx.frontmost_app {
        root.attrs.insert(
            "capture.frontmostApp".into(),
            serde_json::to_value(a).unwrap(),
        );
    }
    if ctx.window.as_ref().is_some_and(|w| w.title.is_none()) {
        lim.omitted.push(Note::new(Some("capture.window"), "window title", "not reported by the platform (it needs screen-recording permission, or the window has none)"));
    }

    // Accessibility: a separate source of evidence from the pixels.
    let ax = ctx
        .permissions
        .get("accessibility")
        .map(String::as_str)
        .unwrap_or("unknown");
    match (&ctx.accessibility, ax) {
        (Some(tree), _) => {
            let (ox, oy) = ctx.frame.map(|f| (f[0], f[1])).unwrap_or((0.0, 0.0));
            let scale = ctx.display.as_ref().map(|d| d.scale).filter(|s| *s > 0.0).unwrap_or(1.0);
            let mut count = 0usize;
            let mut omitted = 0usize;
            let mut stack = vec![(tree, "ax".to_string(), None::<String>)];
            while let Some((n, id, parent)) = stack.pop() {
                if count >= AX_LIMIT {
                    omitted += 1;
                    stack.extend(n.children.iter().map(|c| (c, String::new(), None)));
                    continue;
                }
                count += 1;
                let mut e = el(&id, ax_kind(&n.role), &format!("ax.{}", n.role.trim_start_matches("AX")));
                e.basis = Basis::Source;
                let text: Vec<&str> = [n.title.as_deref(), n.label.as_deref()].into_iter().flatten().filter(|t| !t.is_empty()).collect();
                if !text.is_empty() {
                    e.text = Some(text.join(" — "));
                }
                if let Some(v) = n.value.as_deref().filter(|v| !v.is_empty()) {
                    e.value = Some(json!(v));
                }
                e.attrs.insert("ax.role".into(), json!(n.role));
                if let Some(sr) = &n.subrole {
                    e.attrs.insert("ax.subrole".into(), json!(sr));
                }
                e.visible = false;
                if let Some(f) = n.frame {
                    e.attrs.insert("ax.frame".into(), json!(f));
                    // Global points → image pixels; clipped to the image.
                    let (x0, y0) = ((f[0] - ox) * scale, (f[1] - oy) * scale);
                    let (x1, y1) = (x0 + f[2] * scale, y0 + f[3] * scale);
                    let (cx0, cy0, cx1, cy1) = (x0.max(0.0), y0.max(0.0), x1.min(img.0 as f64), y1.min(img.1 as f64));
                    if cx1 > cx0 && cy1 > cy0 {
                        e.visible = true;
                        let q = |v: f64| ((v * 64.0).round() / 64.0) as f32;
                        e.bounds.push(Bounds { view: VIEW.into(), x: q(cx0), y: q(cy0), w: q(cx1 - cx0), h: q(cy1 - cy0) });
                        if (cx0, cy0, cx1, cy1) != (x0, y0, x1, y1) {
                            e.attrs.insert("ax.clipped".into(), json!(true));
                        }
                    }
                }
                for (i, c) in n.children.iter().enumerate() {
                    stack.push((c, format!("{id}.{i}"), Some(id.clone())));
                }
                match parent {
                    Some(p) => {
                        if let Some(pe) = elements.iter_mut().find(|x| x.id == p) {
                            pe.children.push(id.clone());
                        }
                    }
                    None => root.children.push(id.clone()),
                }
                elements.push(e);
            }
            // Children were pushed in reverse; keep document order.
            for e in elements.iter_mut().filter(|e| e.id.starts_with("ax")) {
                e.children.sort_by_key(|a| natural(a));
            }
            lim.unverified.push(Note::new(Some("ax"), "the accessibility tree", "it is what the application reported through the platform's accessibility API, a separate source from the pixels; the two can disagree"));
            if omitted > 0 {
                lim.omitted.push(Note::new(Some("ax"), format!("{omitted} accessibility nodes"), format!("more than {AX_LIMIT} nodes")));
            }
        }
        (None, "granted") => lim.omitted.push(Note::new(Some("image"), "the accessibility tree", "accessibility permission was granted but no tree was captured (the window exposes none, or capture mode is not window)")),
        (None, status) => lim.omitted.push(Note::new(Some("image"), "the accessibility tree", format!("accessibility permission: {status}"))),
    }
    for n in &ctx.notes {
        lim.unverified.push(Note::new(
            None,
            n.clone(),
            "reported by the capturing application",
        ));
    }
    if title.is_empty() {
        title = format!("{} capture", ctx.mode);
    }
    format!("screenshot of {title} ({}x{})", img.0, img.1)
}

fn natural(id: &str) -> Vec<u64> {
    id.split('.').filter_map(|p| p.parse().ok()).collect()
}

pub fn capture(c: &Capture) -> Result<Package, String> {
    let s = sniff(c.bytes)
        .ok_or("not a recognized image: PNG, JPEG, GIF or WebP, identified by content")?;
    if s.width == 0 || s.height == 0 {
        return Err("image header reports zero dimensions".into());
    }
    let mut store = AssetStore::default();
    let sha = store.add(c.bytes.to_vec(), s.media_type);

    let mut root = el("image", "media", "image.file");
    root.basis = Basis::Observed;
    root.bounds.push(Bounds {
        view: VIEW.into(),
        x: 0.0,
        y: 0.0,
        w: s.width as f32,
        h: s.height as f32,
    });
    root.attrs
        .insert("image.format".into(), json!(s.media_type));
    root.attrs.insert("image.width".into(), json!(s.width));
    root.attrs.insert("image.height".into(), json!(s.height));
    root.attrs
        .insert("image.bytes".into(), json!(c.bytes.len()));
    if let Some(n) = &c.declared_name {
        root.attrs.insert("image.declaredName".into(), json!(n));
    }
    if let Some(t) = &c.declared_type {
        root.attrs.insert("image.declaredType".into(), json!(t));
    }

    let mut lim = Limitations::default();
    let mut extra: Vec<Element> = Vec::new();
    let mut context_facts: Vec<Fact> = Vec::new();
    let ctx: Option<CaptureContext> = match c.context {
        Some(b) => Some(serde_json::from_slice(b).map_err(|e| format!("capture context: {e}"))?),
        None => None,
    };
    let screenshot_title = ctx.as_ref().map(|x| {
        apply_context(
            x,
            (s.width, s.height),
            &mut root,
            &mut extra,
            &mut context_facts,
            &mut lim,
        )
    });
    lim.omitted.push(Note::new(
        Some("image"),
        "the image's content (text, objects, values, layout) as fact",
        "this adapter identifies and measures images and does not interpret pixels; a reader (OCR, a vision model) may add text and positions, always as inference (see limitations.inferred)",
    ));
    if let Some(t) = &c.declared_type {
        if t != s.media_type {
            lim.unverified.push(Note::new(
                Some("image"),
                format!("declared type {t}"),
                format!("the bytes are {}", s.media_type),
            ));
        }
    }
    lim.unverified.push(Note::new(
        None,
        "where the image came from",
        "the caller supplied the bytes; their origin is not established",
    ));

    let mut facts = vec![
        fact("image", "sha256", json!(sha), Basis::Observed, false),
        fact(
            "image",
            "format",
            json!(s.media_type),
            Basis::Observed,
            false,
        ),
        fact(
            "image",
            "pixelSize",
            json!([s.width, s.height]),
            Basis::Observed,
            false,
        ),
    ];
    facts.extend(context_facts);
    let mut inputs = vec![AssetUse {
        asset: sha.clone(),
        role: INPUT_ROLE.into(),
    }];
    if let Some(b) = c.context {
        inputs.push(AssetUse {
            asset: store.add(b.to_vec(), "application/json"),
            role: CONTEXT_ROLE.into(),
        });
    }
    let mut all = vec![root];
    all.extend(extra);
    let renderer = Tool {
        name: "none: the view is the source bytes".into(),
        version: String::new(),
        ruleset: None,
    };
    let artifact = Artifact {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION.into(),
        id: String::new(),
        subject: Subject {
            kind: if ctx.is_some() {
                "screenshot".into()
            } else {
                "image".into()
            },
            reference: None,
            title: screenshot_title.clone().unwrap_or_else(|| {
                format!(
                    "{} {}x{}{}",
                    s.media_type,
                    s.width,
                    s.height,
                    c.declared_name
                        .as_deref()
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default()
                )
            }),
            element: "image".into(),
        },
        provenance: Provenance {
            source: SourceRef {
                kind: if ctx.is_some() {
                    "screen".into()
                } else {
                    "file".into()
                },
                version: ctx.as_ref().and_then(|x| x.platform.clone()),
                ..SourceRef::default()
            },
            capture: xeet_artifact::Capture {
                captured_at: c.captured_at.clone(),
                method: ctx
                    .as_ref()
                    .map(|x| x.method.clone())
                    .unwrap_or_else(|| c.method.clone()),
                captured_by: ctx
                    .as_ref()
                    .and_then(|x| x.captured_by.clone())
                    .unwrap_or_else(|| tool("image")),
            },
            adapter: tool("image"),
            inputs,
            transformations: vec![Transformation {
                step: "identify-and-measure (magic bytes, header dimensions)".into(),
                tool: tool("image"),
                inputs: vec![sha.clone()],
                outputs: vec!["semantic".into(), "views.v0".into()],
                deterministic: true,
                at: None,
            }],
            authenticity: Authenticity::Unverified,
            derived_from: None,
        },
        views: vec![View {
            id: VIEW.into(),
            asset: sha,
            media_type: s.media_type.into(),
            pixel_width: s.width,
            pixel_height: s.height,
            scale: 1.0,
            layout_width: s.width as f32,
            layout_height: s.height as f32,
            renderer,
            config: json!({"original": true}),
            fidelity: Fidelity {
                target: "the source image itself".into(),
                claim: FidelityClaim::Original,
                evidence: Some("the view asset is the input asset (same sha256)".into()),
            },
        }],
        semantic: Semantic {
            root: "image".into(),
            elements: all,
            relations: vec![],
        },
        state: State {
            as_of: c.captured_at.clone(),
            temporal: "historical-observation".into(),
            facts,
        },
        capability_policy: CAPABILITY_POLICY.into(),
        capabilities: vec![],
        limitations: lim,
        coverage: [
            ("visual", Support::Supported),
            ("semantic", Support::NotSupported),
            ("bounds", Support::Partial),
            ("provenance", Support::Partial),
            ("state", Support::Partial),
            ("capabilities", Support::NotApplicable),
            ("reproduction", Support::Supported),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
        assets: vec![],
    };
    Ok(store.seal(artifact))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_content_not_name() {
        let png = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/artifacts/x-post/assets/54f5d325c3977984bea2837f0757b367c3aaeb892c8428a8c3b7ec3526614611.png")).unwrap();
        assert_eq!(
            sniff(&png),
            Some(Sniffed {
                media_type: "image/png",
                width: 390,
                height: 314
            })
        );
        let gif = b"GIF89a\x10\x00\x08\x00\x00\x00\x00";
        assert_eq!(
            sniff(gif).unwrap(),
            Sniffed {
                media_type: "image/gif",
                width: 16,
                height: 8
            }
        );
        // A minimal JPEG: SOI, APP0 (skipped), SOF0 with 0x0020 x 0x0040.
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend([0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x20, 0x00, 0x40, 0x03]);
        assert_eq!(
            sniff(&jpeg).unwrap(),
            Sniffed {
                media_type: "image/jpeg",
                width: 64,
                height: 32
            }
        );
        assert_eq!(sniff(b"%PDF-1.7"), None);
        assert_eq!(sniff(b"hello"), None);
    }

    #[test]
    fn an_image_artifact_verifies_and_says_its_content_is_unread() {
        let png = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/artifacts/x-post/assets/54f5d325c3977984bea2837f0757b367c3aaeb892c8428a8c3b7ec3526614611.png")).unwrap();
        let p = capture(&Capture {
            bytes: &png,
            captured_at: "2026-11-16T12:00:00Z".into(),
            method: "fixture".into(),
            declared_name: Some("shot.jpg".into()),
            declared_type: Some("image/jpeg".into()),
            context: None,
        })
        .unwrap();
        let v = verify::verify(&p, Some(&crate::Reproducers));
        assert!(v.ok, "{v:#?}");
        let a = p.artifact().unwrap();
        assert_eq!(
            a.views[0].asset, a.provenance.inputs[0].asset,
            "the view is the source"
        );
        assert!(a
            .limitations
            .unverified
            .iter()
            .any(|n| n.what == "declared type image/jpeg"));
        let cov = project::consumer_coverage(&a);
        assert_eq!(cov.visual_only, ["image"]);
    }
}
