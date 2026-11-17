//! Deterministic input inspection: what an input actually is.
//!
//! Types come from content (magic bytes, UTF-8 and JSON validity), never from
//! a filename or a declared type; declared values are recorded and compared.
//! Paths are checked against the host's read roots before anything is read,
//! only a header is read to classify, and the digest is computed in chunks so
//! a large file is never held in memory for inspection.

use std::io::Read;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use xeet_engine::{check_path, store, Error, Session};

/// One input the agent assembled for the model.
#[derive(Clone, Debug)]
pub enum Input {
    /// Plain text the agent includes as context.
    Text(String),
    /// Bytes in memory, with whatever the sender claimed about them.
    Bytes {
        bytes: Vec<u8>,
        declared_type: Option<String>,
        name: Option<String>,
    },
    /// A file on disk (needs `fs:read` inside a read root).
    Path(PathBuf),
    /// An artifact id or URI already in a store.
    Artifact(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    Text,
    Image,
    Document,
    Audio,
    Video,
    Structured,
    Artifact,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub index: usize,
    pub origin: String,
    pub class: Class,
    /// Detected from content.
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_name: Option<String>,
    /// The declared type disagrees with the content.
    pub declared_mismatch: bool,
    pub bytes: u64,
    /// `sha256:` of the content (for artifacts: the artifact id).
    pub digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<(u32, u32)>,
}

const HEAD: usize = 256 * 1024;

/// Classifies content from its first bytes (and, for text/JSON, the whole
/// buffer given).
pub fn classify(head: &[u8], complete: bool) -> (Class, String, Option<(u32, u32)>) {
    if let Some(s) = xeet_capture::image::sniff(head) {
        return (Class::Image, s.media_type.into(), Some((s.width, s.height)));
    }
    let at = |i: usize, m: &[u8]| head.get(i..i + m.len()) == Some(m);
    let (c, t): (Class, &str) = if at(0, b"%PDF-") {
        (Class::Document, "application/pdf")
    } else if at(0, b"ID3")
        || (head.len() > 1 && head[0] == 0xff && head[1] & 0xe0 == 0xe0 && head[1] & 0x06 != 0)
    {
        (Class::Audio, "audio/mpeg")
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        (Class::Audio, "audio/wav")
    } else if at(0, b"OggS") {
        (Class::Audio, "audio/ogg")
    } else if at(0, b"fLaC") {
        (Class::Audio, "audio/flac")
    } else if at(4, b"ftyp") {
        if at(8, b"M4A ") {
            (Class::Audio, "audio/mp4")
        } else {
            (Class::Video, "video/mp4")
        }
    } else if at(0, &[0x1a, 0x45, 0xdf, 0xa3]) {
        (Class::Video, "video/webm")
    } else if at(0, b"PK\x03\x04") {
        (Class::Document, "application/zip")
    } else if !head.contains(&0) && std::str::from_utf8(head).is_ok() {
        let t = std::str::from_utf8(head).unwrap_or("").trim_start();
        if complete
            && (t.starts_with('{') || t.starts_with('['))
            && serde_json::from_slice::<serde_json::Value>(head).is_ok()
        {
            (Class::Structured, "application/json")
        } else {
            (Class::Text, "text/plain")
        }
    } else {
        (Class::Unknown, "application/octet-stream")
    };
    (c, t.into(), None)
}

fn mismatch(declared: &Option<String>, detected: &str) -> bool {
    declared
        .as_deref()
        .is_some_and(|d| d.split(';').next().unwrap_or("").trim() != detected)
}

pub fn inspect(index: usize, input: &Input, session: &Session) -> Result<Inspection, Error> {
    let digest = |b: &[u8]| xeet_artifact::canon::sha256_id(b);
    Ok(match input {
        Input::Text(t) => Inspection {
            index,
            origin: "inline-text".into(),
            class: Class::Text,
            media_type: "text/plain".into(),
            declared_type: None,
            declared_name: None,
            declared_mismatch: false,
            bytes: t.len() as u64,
            digest: digest(t.as_bytes()),
            dimensions: None,
        },
        Input::Bytes {
            bytes,
            declared_type,
            name,
        } => {
            let (class, media_type, dimensions) = classify(bytes, true);
            Inspection {
                index,
                origin: "inline-bytes".into(),
                class,
                declared_mismatch: mismatch(declared_type, &media_type),
                media_type,
                declared_type: declared_type.clone(),
                declared_name: name.clone(),
                bytes: bytes.len() as u64,
                digest: digest(bytes),
                dimensions,
            }
        }
        Input::Path(p) => {
            let path = check_path(session, &p.to_string_lossy())?;
            if path.join(xeet_artifact::DOCUMENT).exists() {
                return Ok(Inspection {
                    index,
                    origin: "path".into(),
                    class: Class::Artifact,
                    media_type: "application/vnd.xeet-artifact+dir".into(),
                    declared_type: None,
                    declared_name: Some(p.to_string_lossy().into()),
                    declared_mismatch: false,
                    bytes: 0,
                    digest: String::new(),
                    dimensions: None,
                });
            }
            let io = |e: std::io::Error| xeet_engine::Error {
                code: "not_found".into(),
                message: e.to_string(),
                details: serde_json::Value::Null,
            };
            let mut f = std::fs::File::open(&path).map_err(io)?;
            let len = f.metadata().map_err(io)?.len();
            let mut head = vec![0u8; HEAD.min(len as usize)];
            f.read_exact(&mut head).map_err(io)?;
            let (class, media_type, dimensions) = classify(&head, len as usize <= HEAD);
            // Stream the rest into the digest.
            let mut h = Sha256::new();
            h.update(&head);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = f.read(&mut buf).map_err(io)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
            Inspection {
                index,
                origin: "path".into(),
                class,
                media_type,
                declared_type: None,
                declared_name: path.file_name().map(|n| n.to_string_lossy().into()),
                declared_mismatch: false,
                bytes: len,
                digest: format!("sha256:{hex}"),
                dimensions,
            }
        }
        Input::Artifact(r) => {
            let id = store::parse_id(r).ok_or_else(|| xeet_engine::Error {
                code: "invalid_input".into(),
                message: format!("{r} is not an artifact id or URI"),
                details: serde_json::Value::Null,
            })?;
            Inspection {
                index,
                origin: "artifact-ref".into(),
                class: Class::Artifact,
                media_type: "application/vnd.xeet-artifact+ref".into(),
                declared_type: None,
                declared_name: None,
                declared_mismatch: false,
                bytes: 0,
                digest: id,
                dimensions: None,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_decides_not_names() {
        assert_eq!(classify(b"%PDF-1.7 ...", true).1, "application/pdf");
        assert_eq!(classify(b"ID3\x04\x00", true).0, Class::Audio);
        assert_eq!(classify(b"\x00\x00\x00\x18ftypisom", true).1, "video/mp4");
        assert_eq!(classify(b"{\"a\": 1}", true).1, "application/json");
        assert_eq!(classify(b"{\"a\": 1", true).1, "text/plain");
        assert_eq!(classify(b"hello", true).0, Class::Text);
        assert_eq!(classify(&[0, 1, 2, 3], true).0, Class::Unknown);
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x02\x00\x00\x00\x03";
        assert_eq!(
            classify(png, true),
            (Class::Image, "image/png".into(), Some((2, 3)))
        );
        let i = inspect(
            0,
            &Input::Bytes {
                bytes: png.to_vec(),
                declared_type: Some("image/jpeg".into()),
                name: Some("a.jpg".into()),
            },
            &Session::default(),
        )
        .unwrap();
        assert!(i.declared_mismatch && i.media_type == "image/png");
    }
}
