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
    lim.omitted.push(Note::new(
        Some("image"),
        "the image's content: text, objects, values, layout",
        "this adapter identifies and measures images; it does not interpret pixels. Content stays visual-only until a reader annotates it, as inference",
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

    let facts = vec![
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
            kind: "image".into(),
            reference: None,
            title: format!(
                "{} {}x{}{}",
                s.media_type,
                s.width,
                s.height,
                c.declared_name
                    .as_deref()
                    .map(|n| format!(" ({n})"))
                    .unwrap_or_default()
            ),
            element: "image".into(),
        },
        provenance: Provenance {
            source: SourceRef {
                kind: "file".into(),
                ..SourceRef::default()
            },
            capture: xeet_artifact::Capture {
                captured_at: c.captured_at.clone(),
                method: c.method.clone(),
                captured_by: tool("image"),
            },
            adapter: tool("image"),
            inputs: vec![AssetUse {
                asset: sha.clone(),
                role: INPUT_ROLE.into(),
            }],
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
            elements: vec![root],
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
