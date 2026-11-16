//! A content-addressed artifact store: `<root>/<hex>/artifact.json + assets/`.
//!
//! The directory name is the artifact id, so a reference can never point at
//! different content than it names. Nothing enters without passing
//! verification; reads re-check integrity.

use std::path::{Path, PathBuf};

use serde::Serialize;
use xeet_artifact::{verify, Package};

#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub uri: String,
    pub subject_type: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub captured_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
    /// Digests of the source inputs the artifact preserves.
    pub inputs: Vec<String>,
    /// How many elements carry inferred content (readings added by annotate).
    pub inferred: usize,
}

/// `artifact://sha256/<hex>`: the URI form of an artifact id.
pub fn uri(id: &str) -> String {
    format!("artifact://sha256/{}", id.trim_start_matches("sha256:"))
}

/// Parses `sha256:<hex>`, `artifact://sha256/<hex>[/…]` or a bare hex id.
pub fn parse_id(r: &str) -> Option<String> {
    let hex = r
        .strip_prefix("artifact://sha256/")
        .map(|rest| rest.split('/').next().unwrap_or(""))
        .or_else(|| r.strip_prefix("sha256:"))
        .unwrap_or(r);
    (hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    .then(|| format!("sha256:{hex}"))
}

impl Store {
    pub fn open(root: &Path) -> Result<Store, String> {
        std::fs::create_dir_all(root).map_err(|e| format!("store {}: {e}", root.display()))?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        Ok(Store { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id.trim_start_matches("sha256:"))
    }

    /// Stores a package that passes integrity verification. Idempotent.
    pub fn put(&self, p: &Package) -> Result<String, String> {
        let v = verify::verify(p, None);
        if !v.ok {
            return Err("refused: the package does not pass integrity verification".into());
        }
        let id = v.artifact.ok_or("no id")?;
        let dir = self.dir(&id);
        if !dir.join(xeet_artifact::DOCUMENT).exists() {
            let tmp = self
                .root
                .join(format!(".incoming-{}", id.trim_start_matches("sha256:")));
            let _ = std::fs::remove_dir_all(&tmp);
            p.write_dir(&tmp).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &dir).map_err(|e| e.to_string())?;
        }
        Ok(id)
    }

    pub fn get(&self, id: &str) -> Result<Package, String> {
        let dir = self.dir(id);
        if !dir.join(xeet_artifact::DOCUMENT).exists() {
            return Err(format!("no artifact {id} in the store"));
        }
        Package::read_dir(&dir)
    }

    pub fn list(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return out;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| parse_id(n).is_some())
            .collect();
        names.sort();
        for n in names {
            let Ok(p) = Package::read_dir(&self.root.join(&n)) else {
                continue;
            };
            let Ok(a) = p.artifact() else { continue };
            out.push(Entry {
                uri: uri(&a.id),
                id: a.id.clone(),
                subject_type: a.subject.kind.clone(),
                title: a.subject.title.clone(),
                reference: a.subject.reference.clone(),
                captured_at: a.provenance.capture.captured_at.clone(),
                derived_from: a.provenance.derived_from.clone(),
                inputs: a
                    .provenance
                    .inputs
                    .iter()
                    .map(|i| i.asset.clone())
                    .collect(),
                inferred: a
                    .semantic
                    .elements
                    .iter()
                    .filter(|e| e.basis == xeet_artifact::Basis::Inferred)
                    .count(),
            });
        }
        out
    }
}
