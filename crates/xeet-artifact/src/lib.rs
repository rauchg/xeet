//! `xeet-artifact`: a durable, self-describing representation of one observation.
//!
//! An artifact is not an image. It is a package of coordinated representations
//! of a captured observation — visual views for humans, a semantic graph for
//! machines, provenance, temporally grounded state, capability descriptions,
//! and the producer's own account of its limitations — with every byte it
//! refers to content-addressed.
//!
//! This crate is the universal core. It knows nothing about X, GitHub, or any
//! renderer, and it needs no network, no clock and no model:
//!
//! - [`model`] — the versioned contract.
//! - [`canon`] — canonical JSON and SHA-256 identity.
//! - [`Package`] — reading and writing an artifact directory.
//! - [`verify`] — deterministic integrity and consistency checks, with an
//!   optional [`Reproducer`] hook that re-derives views from preserved inputs.
//! - [`compare`] — what differs between two observations.
//! - [`inspect`] — a human-readable account.
//!
//! Source adapters (in other crates) produce artifacts; this crate consumes
//! and checks them, which is what keeps adapters from having to touch it.

#![forbid(unsafe_code)]

pub mod canon;
pub mod compare;
pub mod inspect;
pub mod model;
pub mod verify;

pub use model::*;

use std::collections::BTreeMap;
use std::path::Path;

/// The document file inside a package directory.
pub const DOCUMENT: &str = "artifact.json";

/// An artifact plus the bytes of its assets, in memory.
#[derive(Clone, Debug)]
pub struct Package {
    /// The document exactly as read (or as produced). Identity is computed over
    /// this value, so fields a newer producer added are covered by the hash.
    pub document: serde_json::Value,
    /// The document's text when it was read from disk. Identity is checked
    /// against this, with number tokens as written (see [`canon`]).
    pub text: Option<String>,
    /// Package-relative path → bytes, for every file found under `assets/`.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Package {
    /// The typed view of the document, if it matches this version's contract.
    pub fn artifact(&self) -> Result<Artifact, String> {
        serde_json::from_value(self.document.clone()).map_err(|e| e.to_string())
    }

    /// Bytes of an asset by its `sha256:` id.
    pub fn asset_bytes(&self, sha: &str) -> Option<&[u8]> {
        let artifact = self.artifact().ok()?;
        let asset = artifact.assets.iter().find(|a| a.sha256 == sha)?;
        self.files.get(&asset.path).map(Vec::as_slice)
    }

    /// Writes `artifact.json` (pretty, stable key order) and `assets/*`.
    pub fn write_dir(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir.join("assets"))?;
        let mut json = canon::pretty(&self.document);
        json.push('\n');
        std::fs::write(dir.join(DOCUMENT), json)?;
        for (path, bytes) in &self.files {
            std::fs::write(dir.join(path), bytes)?;
        }
        Ok(())
    }

    /// Reads a package directory. Assets are read as listed by the document and
    /// as found on disk; verification reports any mismatch between the two.
    pub fn read_dir(dir: &Path) -> Result<Package, String> {
        let text = std::fs::read_to_string(dir.join(DOCUMENT))
            .map_err(|e| format!("{}: {e}", dir.join(DOCUMENT).display()))?;
        let document: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{DOCUMENT}: {e}"))?;
        let mut files = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(dir.join("assets")) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let bytes = std::fs::read(entry.path()).map_err(|e| e.to_string())?;
                    files.insert(format!("assets/{name}"), bytes);
                }
            }
        }
        Ok(Package {
            document,
            text: Some(text),
            files,
        })
    }
}

/// Collects assets while an adapter builds an artifact.
#[derive(Clone, Debug, Default)]
pub struct AssetStore {
    assets: BTreeMap<String, (Asset, Vec<u8>)>,
}

impl AssetStore {
    /// Adds bytes and returns their `sha256:` id. Adding the same bytes twice
    /// is a no-op, which is what content addressing means.
    pub fn add(&mut self, bytes: Vec<u8>, media_type: &str) -> String {
        let sha = canon::sha256_id(&bytes);
        let path = asset_path(&sha, media_type);
        self.assets.entry(sha.clone()).or_insert_with(|| {
            (
                Asset {
                    sha256: sha.clone(),
                    media_type: media_type.to_string(),
                    bytes: bytes.len() as u64,
                    path,
                },
                bytes,
            )
        });
        sha
    }

    /// Fills `artifact.assets`, computes the id, and returns the package.
    pub fn seal(self, mut artifact: Artifact) -> Package {
        artifact.assets = self.assets.values().map(|(a, _)| a.clone()).collect();
        artifact.id = String::new();
        let mut document = serde_json::to_value(&artifact).expect("artifact serializes");
        let id = canon::document_id(&document);
        document["id"] = serde_json::Value::String(id);
        let files = self
            .assets
            .into_values()
            .map(|(a, bytes)| (a.path, bytes))
            .collect();
        Package {
            document,
            text: None,
            files,
        }
    }
}

/// `assets/<hex>.<ext>`: the path is a function of the content and its type.
pub fn asset_path(sha: &str, media_type: &str) -> String {
    let hex = sha.strip_prefix("sha256:").unwrap_or(sha);
    let ext = match media_type {
        "image/png" => "png",
        "application/json" => "json",
        "text/plain" => "txt",
        _ => "bin",
    };
    format!("assets/{hex}.{ext}")
}

/// Re-derives an artifact's views from its own preserved inputs.
///
/// The core cannot render anything; adapters that can implement this, and a
/// verifier that is given one checks that the views are reproducible from the
/// inputs the artifact claims they came from. That is the deterministic link
/// between the visual and the semantic representations: both are outputs of
/// declared, deterministic steps over the same input bytes.
pub trait Reproducer {
    /// The `provenance.adapter.name` values this reproducer understands.
    fn handles(&self, adapter: &str) -> bool;
    /// View id → regenerated view bytes.
    fn reproduce(
        &self,
        artifact: &Artifact,
        package: &Package,
    ) -> Result<BTreeMap<String, Vec<u8>>, String>;
}
