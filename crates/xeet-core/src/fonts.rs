//! The embedded font set and per-character face selection.
//!
//! X serves Chirp as four weights, each split into seven files by Unicode
//! range. Chrome treats those files as one family and, for every character,
//! uses the file whose range contains it. We reproduce that here so shaping
//! runs split exactly where Chrome's do.
//!
//! Beyond Chirp the selection follows X's font stack, which on Apple devices
//! is `TwitterChirp, -apple-system, …, Helvetica, Arial, Apple Color Emoji, …,
//! sans-serif`. Chrome walks it per character and takes the first family with
//! a glyph, so a family only ever wins the characters no earlier family
//! covers. The faces we can bundle therefore carry the unicode-range Chrome
//! reaches *them* for (derived by `tools/oracle/font-ranges.mjs`), and the
//! manifest lists them in stack order: Arial, the emoji font, then the open
//! stand-ins for the system fallback that cannot be bundled (SF, Helvetica,
//! Apple Symbols, Zapf Dingbats, Menlo, Hiragino).
//!
//! Measured against Chrome on macOS over 6,376 rows — every code point in the
//! ranges posts use, plain and with VS15/VS16 — characters Chrome draws with
//! Chirp, Arial, the emoji font or STIX agree on family and advance 94% of the
//! time. Most of the remaining 6% are VS15 requests on characters only the
//! emoji font has, where our symbol stand-ins cover more than Apple's text
//! fonts do. Characters Chrome gives to a family that cannot be bundled keep
//! that device's advances and match ours only ~7% of the time; "★", "✈", "☺",
//! "♥", "─" and the letterlike capitals ("ℍ", "ℝ") are the visible ones.

use harfrust::font::Font;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaceKind {
    /// One of X's Chirp files.
    Chirp,
    /// Open-licensed stand-in for characters Chirp lacks.
    Fallback,
    /// Color emoji.
    Emoji,
}

pub struct FaceData {
    pub kind: FaceKind,
    pub weight: u16,
    pub family: &'static str,
    pub subset: &'static str,
    /// Basename of the vendored file, for clients that render with the exact
    /// face the engine chose.
    pub file: &'static str,
    /// The lazy pack this face belongs to (`core`, `emoji`, `cjk`, …). `core`
    /// is always embedded; the rest are fetched and registered by the host.
    pub pack: &'static str,
    pub ranges: &'static [(u32, u32)],
    /// The face's bytes. Empty in a lazy build: the file was left out of the
    /// module and is looked up in [`RUNTIME`] instead.
    pub data: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/assets_gen.rs"));

pub type FaceId = u16;

pub struct Face {
    pub id: FaceId,
    /// The parsed face. An unavailable face keeps the primary Chirp font as a
    /// placeholder so `Face` stays cheap to index; [`Face::available`] is what
    /// selection and painting check before using it.
    pub font: Font,
    pub kind: FaceKind,
    pub weight: u16,
    pub family: &'static str,
    pub subset: &'static str,
    pub file: &'static str,
    pub pack: &'static str,
    /// False until the pack that carries this file has been registered.
    pub available: bool,
    pub upem: f32,
    /// Ascender and descender in font units, both positive.
    pub ascent: f32,
    pub descent: f32,
    ranges: &'static [(u32, u32)],
    /// Lowest and highest code point `ranges` covers, so a code point outside
    /// them is rejected by two comparisons instead of a scan.
    min_cp: u32,
    max_cp: u32,
}

impl Face {
    #[inline]
    pub fn in_range(&self, cp: u32) -> bool {
        if !self.available || cp < self.min_cp || cp > self.max_cp {
            return false;
        }
        self.ranges.iter().any(|&(lo, hi)| cp >= lo && cp <= hi)
    }

    #[inline]
    pub fn glyph(&self, ch: char) -> Option<u32> {
        if !self.available {
            return None;
        }
        self.font
            .charmap()
            .map_unicode(ch)
            .map(|g| g.to_u32())
            .filter(|&g| g != 0)
    }

    /// Integer ascent and descent at `size`, rounded the way Blink rounds them.
    pub fn rounded_metrics(&self, size: f32) -> (f32, f32) {
        let scale = size / self.upem;
        (
            (self.ascent * scale).round(),
            (self.descent * scale).round(),
        )
    }
}

pub struct FontDb {
    pub faces: Vec<Face>,
    emoji: Option<FaceId>,
    /// The Chirp Latin face of each weight bucket (400, 500, 700, 800).
    primary: [FaceId; 4],
    /// [`select_with`](Self::select_with) for every code point under U+0100 at
    /// each weight bucket, with no emoji or text-presentation request: what
    /// almost every character of a post asks for, as one array index.
    latin: Box<[[FaceId; 256]; 4]>,
}

/// The process-wide font set. It is rebuilt when a host registers a lazy pack,
/// so face ids are assigned from the static [`FACES`] index and never shift:
/// an unavailable face keeps its slot (and a placeholder font) until its bytes
/// arrive.
static DB: RwLock<Option<Arc<FontDb>>> = RwLock::new(None);
/// Serializes rebuilds; a registration only marks the db dirty.
static REBUILD: Mutex<()> = Mutex::new(());
/// Set by [`register_font`], cleared by the next [`db`] call that rebuilds.
static DIRTY: AtomicBool = AtomicBool::new(true);
/// Bumped on every rebuild. [`SELECT_CACHE`] keys include it so a pack that
/// arrives after a render never leaves a stale face choice behind.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Runtime font bytes registered by the host, keyed by [`FaceData::file`].
/// The bytes are leaked: a registered face lives for the rest of the process,
/// exactly like an embedded one, which keeps `Face`'s `'static` borrows honest.
static RUNTIME: OnceLock<RwLock<HashMap<&'static str, &'static [u8]>>> = OnceLock::new();

fn runtime() -> &'static RwLock<HashMap<&'static str, &'static [u8]>> {
    RUNTIME.get_or_init(|| RwLock::new(HashMap::new()))
}

/// A snapshot of the font set. Derefs to [`FontDb`], so call sites keep writing
/// `db().select(…)` / `db().faces`. It owns an `Arc`, so it stays valid across a
/// later rebuild and never holds a lock.
pub struct FontDbRef(Arc<FontDb>);

impl Deref for FontDbRef {
    type Target = FontDb;

    fn deref(&self) -> &FontDb {
        &self.0
    }
}

/// The process-wide font set. Parsing is lazy inside each font, so a rebuild is
/// cheap; it happens only when a lazy pack has been registered.
pub fn db() -> FontDbRef {
    ensure_loaded();
    let db = DB
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .expect("the font db is built before a snapshot is handed out")
        .clone();
    FontDbRef(db)
}

/// Rebuilds the db once if a pack was registered since the last call.
fn ensure_loaded() {
    let initialized = DB.read().unwrap_or_else(|e| e.into_inner()).is_some();
    if initialized && !DIRTY.load(Ordering::Acquire) {
        return;
    }
    let _guard = REBUILD.lock().unwrap_or_else(|e| e.into_inner());
    // Clear the flag before building. A registration that lands while
    // `FontDb::load` runs sets it again, so the next `db()` rebuilds instead of
    // losing that pack. (Clearing it after the build would race exactly there.)
    let dirty = DIRTY.swap(false, Ordering::AcqRel);
    if initialized && !dirty {
        return;
    }
    let db = Arc::new(FontDb::load());
    *DB.write().unwrap_or_else(|e| e.into_inner()) = Some(db);
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Monotonic counter bumped whenever the font set is (re)loaded.
pub(crate) fn generation() -> u32 {
    GENERATION.load(Ordering::Relaxed) as u32
}

/// Registers one font file from a lazy pack. The host slices the pack blob and
/// calls this once per file. The next [`db`] call rebuilds the face table.
///
/// Core faces are embedded and need no registration; registering one is a
/// no-op, so a host can safely send a whole pack without special-casing it.
pub fn register_font(file: &str, data: Vec<u8>) -> Result<(), String> {
    let Some(spec) = FACES.iter().find(|face| face.file == file) else {
        return Err(format!("xeet: no font named `{file}` in this build"));
    };
    if spec.pack == "core" || face_bytes(spec.file).is_some() {
        // Core is embedded; a second registration of the same file would leak
        // another copy and rebuild for no reason.
        return Ok(());
    }
    store_runtime_font(spec.file, data)
}

/// Replaces a face's bytes at runtime, even when `file` is already embedded in
/// the binary (`core` included). Used by `XEET_FONT_DIR` overrides and hosts
/// that supply custom font files.
pub fn override_font(file: &str, data: Vec<u8>) -> Result<(), String> {
    let Some(spec) = FACES.iter().find(|face| face.file == file) else {
        return Err(format!("xeet: no font named `{file}` in this build"));
    };
    store_runtime_font(spec.file, data)
}

fn store_runtime_font(static_file: &'static str, data: Vec<u8>) -> Result<(), String> {
    if data.is_empty() {
        return Err(format!("xeet: `{static_file}` is empty"));
    }
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    runtime()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(static_file, leaked);
    DIRTY.store(true, Ordering::Release);
    Ok(())
}

/// The bytes for `file`: host-registered/overridden bytes first, then embedded.
pub fn face_bytes(file: &str) -> Option<&'static [u8]> {
    if let Some(&bytes) = runtime()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(file)
    {
        return Some(bytes);
    }
    if let Some(spec) = FACES.iter().find(|face| face.file == file) {
        if !spec.data.is_empty() {
            return Some(spec.data);
        }
    }
    None
}

/// True when any face in this build was omitted from the binary and must be
/// supplied at runtime via [`register_font`] / [`load_packs_for_json`].
/// Constant-folds to `false` in full-embed native builds.
#[inline]
pub fn has_lazy_packs() -> bool {
    FACES.iter().any(|face| face.data.is_empty())
}

/// True once every file in `pack` has bytes. `core` is always loaded.
pub fn pack_loaded(pack: &str) -> bool {
    if pack == "core" {
        return true;
    }
    FACES
        .iter()
        .filter(|face| face.pack == pack)
        .all(|face| face_bytes(face.file).is_some())
}

// ---------------------------------------------------------------------------
// Native pack loading
// ---------------------------------------------------------------------------
//
// The wasm host does this in JS (`packages/xeet/src/fonts.js`) because the
// browser owns the fetch. A native CLI reads the same files from disk, so the
// release binary can ship core-only and put the 39 MB of packs beside it.

/// The manifest `tools/ship/build-font-packs.mjs` writes beside the packs.
#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Deserialize)]
struct PackManifest {
    version: u32,
    packs: HashMap<String, PackEntry>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Deserialize)]
struct PackEntry {
    bytes: usize,
    #[serde(default)]
    sha256: String,
    files: Vec<PackFile>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Deserialize)]
struct PackFile {
    file: String,
    offset: usize,
    length: usize,
}

#[cfg(not(target_arch = "wasm32"))]
fn read_pack_manifest(dir: &std::path::Path) -> Result<PackManifest, String> {
    let path = dir.join("packs.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("xeet: cannot read {}: {e}", path.display()))?;
    let manifest: PackManifest = serde_json::from_str(&text)
        .map_err(|e| format!("xeet: {} is not a valid pack manifest: {e}", path.display()))?;
    if manifest.version != 1 {
        return Err(format!(
            "xeet: {} has version {}, expected 1",
            path.display(),
            manifest.version
        ));
    }
    Ok(manifest)
}

#[cfg(not(target_arch = "wasm32"))]
fn install_pack_inner(
    dir: &std::path::Path,
    name: &str,
    pack: &PackEntry,
    allow_override: bool,
) -> Result<(), String> {
    if !allow_override && pack_loaded(name) {
        return Ok(());
    }
    let path = dir.join(format!("{name}.bin"));
    let blob =
        std::fs::read(&path).map_err(|e| format!("xeet: cannot read {}: {e}", path.display()))?;
    if blob.len() != pack.bytes {
        return Err(format!(
            "xeet: {} is {} bytes, expected {}",
            path.display(),
            blob.len(),
            pack.bytes
        ));
    }
    if !pack.sha256.is_empty() {
        let actual = sha256_hex(&blob);
        if actual != pack.sha256 {
            return Err(format!(
                "xeet: {} failed its SHA-256 check (got {}, expected {})",
                path.display(),
                &actual[..12.min(actual.len())],
                &pack.sha256[..12.min(pack.sha256.len())]
            ));
        }
    }
    for file in &pack.files {
        let end = file
            .offset
            .checked_add(file.length)
            .ok_or_else(|| format!("xeet: {} has an overflowing range", path.display()))?;
        if end > blob.len() {
            return Err(format!(
                "xeet: {} has a bad entry for {}",
                path.display(),
                file.file
            ));
        }
        let slice = blob[file.offset..end].to_vec();
        if allow_override {
            override_font(&file.file, slice)?;
        } else {
            register_font(&file.file, slice)?;
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn install_pack(dir: &std::path::Path, name: &str, pack: &PackEntry) -> Result<(), String> {
    install_pack_inner(dir, name, pack, false)
}

/// Applies explicit font overrides from `dir` (`XEET_FONT_DIR`), replacing
/// embedded faces when present. Supports both a `packs.json` + `<pack>.bin`
/// bundle and standalone `<FaceFile>.ttf`/`.otf` files dropped directly into
/// `dir`.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_overrides_from_dir(dir: &std::path::Path) -> Result<usize, String> {
    if !dir.is_dir() {
        return Err(format!(
            "xeet: XEET_FONT_DIR `{}` is not a directory",
            dir.display()
        ));
    }
    let mut count = 0usize;
    if dir.join("packs.json").is_file() {
        let manifest = read_pack_manifest(dir)?;
        let mut names: Vec<&String> = manifest.packs.keys().collect();
        names.sort();
        for name in names {
            let pack = &manifest.packs[name];
            if dir.join(format!("{name}.bin")).is_file() {
                install_pack_inner(dir, name, pack, true)?;
                count += pack.files.len();
            }
        }
    }
    for spec in FACES {
        let candidate = dir.join(spec.file);
        if candidate.is_file() {
            let bytes = std::fs::read(&candidate)
                .map_err(|e| format!("xeet: cannot read {}: {e}", candidate.display()))?;
            override_font(spec.file, bytes)?;
            count += 1;
        }
    }
    if count == 0 {
        return Err(format!(
            "xeet: XEET_FONT_DIR `{}` contains neither a valid packs.json bundle nor any known font file",
            dir.display()
        ));
    }
    Ok(count)
}

/// Registers every pack `json` needs from `dir`. Returns the packs it loaded
/// (already-loaded packs are skipped).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_packs_for_json(dir: &std::path::Path, json: &str) -> Result<Vec<&'static str>, String> {
    let needed = missing_packs_for_json(json);
    if needed.is_empty() {
        return Ok(needed);
    }
    let manifest = read_pack_manifest(dir)?;
    for name in &needed {
        let pack = manifest.packs.get(*name).ok_or_else(|| {
            format!(
                "xeet: no pack `{name}` in {}",
                dir.join("packs.json").display()
            )
        })?;
        install_pack(dir, name, pack)?;
    }
    Ok(needed)
}

/// Registers every pack in `dir`, for commands that inspect the whole font set
/// (`xeet info`, `xeet fonts`).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_all_packs(dir: &std::path::Path) -> Result<usize, String> {
    let manifest = read_pack_manifest(dir)?;
    let names: Vec<String> = manifest.packs.keys().cloned().collect();
    let mut loaded = 0;
    for name in names {
        let pack = &manifest.packs[&name];
        if !pack_loaded(&name) {
            install_pack(dir, &name, pack)?;
            loaded += 1;
        }
    }
    Ok(loaded)
}

#[cfg(not(target_arch = "wasm32"))]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

impl FontDb {
    fn load() -> FontDb {
        // Parse what is available in a first pass, so an unavailable face can
        // borrow the primary Chirp font as its placeholder. `Font` is an Arc
        // handle, so cloning it is cheap.
        let mut loaded: Vec<(&'static FaceData, Option<Font>)> = Vec::with_capacity(FACES.len());
        for data in FACES {
            let font = face_bytes(data.file).and_then(|bytes| Font::new(bytes, 0));
            loaded.push((data, font));
        }
        let placeholder = loaded
            .iter()
            .find_map(|(data, font)| {
                (data.kind == FaceKind::Chirp && data.weight == 400 && data.subset == "latin")
                    .then(|| font.clone())
                    .flatten()
            })
            .or_else(|| loaded.iter().find_map(|(_, font)| font.clone()))
            .expect("xeet: no font could be parsed; the core pack must contain Chirp and Arial");

        let mut faces = Vec::with_capacity(FACES.len());
        for (index, (data, font)) in loaded.into_iter().enumerate() {
            let available = font.is_some();
            let font = font.unwrap_or_else(|| placeholder.clone());
            let m = font.metrics();
            let upem = m.units_per_em as f32;
            // Blink takes ascent and descent from hhea unless the font opts
            // into typographic metrics.
            let line = if m.use_typo_metrics {
                m.typo_line.or(m.hhea_line)
            } else {
                m.hhea_line.or(m.typo_line)
            };
            let (ascent, descent) = match line {
                Some(l) => (l.ascender.to_f32(), l.descender.to_f32().abs()),
                None => (upem * 0.8, upem * 0.2),
            };
            let (min_cp, max_cp) = bounds(data.ranges);
            faces.push(Face {
                id: index as FaceId,
                font,
                kind: data.kind,
                weight: data.weight,
                family: data.family,
                subset: data.subset,
                file: data.file,
                pack: data.pack,
                available,
                upem,
                ascent,
                descent,
                ranges: data.ranges,
                min_cp,
                max_cp,
            });
        }
        let emoji = faces
            .iter()
            .find(|f| f.available && f.kind == FaceKind::Emoji)
            .map(|f| f.id);
        let primary = [400u16, 500, 700, 800].map(|w| {
            faces
                .iter()
                .find(|f| {
                    f.available && f.kind == FaceKind::Chirp && f.weight == w && f.subset == "latin"
                })
                .or_else(|| faces.iter().find(|f| f.available))
                .expect("xeet: the core pack must contain a Chirp Latin face")
                .id
        });
        let mut db = FontDb {
            faces,
            emoji,
            primary,
            latin: Box::new([[0; 256]; 4]),
        };
        // The table is exactly the general algorithm's answer, computed once.
        for (bucket, weight) in [400u16, 500, 700, 800].into_iter().enumerate() {
            for cp in 0..256u32 {
                let ch = char::from_u32(cp).unwrap();
                db.latin[bucket][cp as usize] = db.select_uncached(ch, weight, false, false);
            }
        }
        db
    }

    #[inline]
    pub fn face(&self, id: FaceId) -> &Face {
        &self.faces[id as usize]
    }

    pub fn emoji_face(&self) -> Option<&Face> {
        self.emoji.map(|id| self.face(id))
    }

    /// The Chirp file Chrome would start with: the Latin subset at `weight`.
    pub fn primary(&self, weight: u16) -> &Face {
        self.face(self.primary[bucket(weight)])
    }

    /// Picks the face for one character at a CSS weight.
    ///
    /// `emoji` asks for color presentation. Such clusters always get the
    /// emoji face, even one newer than the bundled artwork: they are laid
    /// out with the platform emoji advance, and only painting needs a glyph.
    /// If no face has the character the primary face is returned and the
    /// caller draws its missing-glyph box.
    pub fn select(&self, ch: char, weight: u16, emoji: bool) -> FaceId {
        self.select_with(ch, weight, emoji, false)
    }

    /// [`select`](Self::select) for a cluster that asks for text presentation
    /// (U+FE0E, as in "▪︎"): the emoji font is then only a last resort, after
    /// every text face. Chrome draws "▪︎" with a text font, and "❤︎" with
    /// Zapf Dingbats, even though the emoji font is listed earlier in X's
    /// stack and has both.
    pub fn select_with(
        &self,
        ch: char,
        weight: u16,
        emoji: bool,
        text_presentation: bool,
    ) -> FaceId {
        // Latin text with no emoji request is the common case by a wide
        // margin, and its answer depends on the code point and the weight
        // only: one array index instead of a walk of X's font stack.
        let cp = ch as u32;
        if cp < 256 && !emoji && !text_presentation {
            return self.latin[bucket(weight)][cp as usize];
        }
        self.select_uncached(ch, weight, emoji, text_presentation)
    }

    /// The general selection: X's stack, in order, per character.
    ///
    /// Code points the Latin table does not cover — everything outside
    /// U+0000..U+00FF, plus emoji and text-presentation requests — come here,
    /// and a hit in [`SELECT_CACHE`] answers them without walking the stack.
    fn select_uncached(
        &self,
        ch: char,
        weight: u16,
        emoji: bool,
        text_presentation: bool,
    ) -> FaceId {
        let cp = ch as u32;
        if cp < 256 && !emoji && !text_presentation {
            // The Latin table already answered this one for every weight.
            return self.select_stack(ch, weight, emoji, text_presentation);
        }
        // The stack walk depends on the code point, the Chirp weight bucket
        // (which also decides the 700/400 fallback weight below) and the two
        // presentation flags, and on nothing else: the fonts never change
        // after loading, so the answer is worth keeping.
        // The generation is part of the key: a lazy pack registered after a
        // render changes the answer for the same code point, and the old
        // entry must miss rather than keep the pre-pack face.
        let key = (cp as u64)
            | ((bucket(weight) as u64) << 24)
            | ((emoji as u64) << 28)
            | ((text_presentation as u64) << 29)
            | ((GENERATION.load(Ordering::Relaxed) & 0xFFFF_FFFF) << 32);
        let slot = cache_slot(key);
        if let Some(id) = SELECT_CACHE.with(|c| {
            let slots = c.borrow();
            let (k, id) = slots[slot];
            (k == key).then_some(id)
        }) {
            return id;
        }
        let id = self.select_stack(ch, weight, emoji, text_presentation);
        SELECT_CACHE.with(|c| c.borrow_mut()[slot] = (key, id));
        id
    }

    /// X's stack walked per character: the answer [`select_uncached`] caches.
    fn select_stack(&self, ch: char, weight: u16, emoji: bool, text_presentation: bool) -> FaceId {
        let cp = ch as u32;
        // An explicit emoji sequence (VS16, a keycap, a skin tone, a ZWJ
        // sequence) wins over everything, Chirp included: "©️" is emoji while
        // "©" is not.
        if emoji {
            if let Some(e) = self.emoji {
                return e;
            }
        }
        let w = chirp_weight(weight);
        // Chirp comes first in X's stack and wins every character it has,
        // including the few that Unicode draws as emoji by default: Chrome
        // renders U+1F503 with Chirp, not with the emoji font.
        // Later @font-face rules win where ranges overlap.
        for f in self.faces.iter().rev() {
            if f.kind == FaceKind::Chirp && f.weight == w && f.in_range(cp) && f.glyph(ch).is_some()
            {
                return f.id;
            }
        }
        // A character Unicode draws as emoji by default gets the emoji face,
        // and with it the platform emoji advance, even if the bundled artwork
        // is older than the character: a new emoji is still one emoji wide on
        // the device. Chrome agrees whenever no text family in the stack has
        // the character, which the emoji face's unicode-range encodes.
        if !text_presentation && is_emoji_presentation(ch) {
            if let Some(e) = self.emoji {
                return e;
            }
        }
        // A space or invisible character that a Chirp file claims by its
        // unicode-range but lacks a glyph for stays in that file: the shaper
        // draws it with the space glyph or hides it. Chrome does this for
        // U+202F, U+2060 and the bidi controls rather than falling back.
        if is_space_or_ignorable(cp) {
            if let Some(f) = self
                .faces
                .iter()
                .rev()
                .find(|f| f.kind == FaceKind::Chirp && f.weight == w && f.in_range(cp))
            {
                return f.id;
            }
        }
        // Then the rest of X's stack, in the order the manifest lists it: the
        // stand-in for Arial, then the emoji font, then the stand-ins for the
        // system fallback. Chrome walks the stack per character and takes the
        // first family with a glyph, and each face is limited to the
        // unicode-range Chrome on Apple reaches it for, so this is one pass.
        // Arial before the emoji font is what makes "▪" a text character
        // (Arial has U+25AA) while "❤" stays emoji (Arial does not).
        let fw = if weight >= 600 { 700 } else { 400 };
        let mut regular_hit = None;
        for f in &self.faces {
            if f.kind == FaceKind::Chirp || !f.in_range(cp) || f.glyph(ch).is_none() {
                continue;
            }
            if f.kind == FaceKind::Emoji && text_presentation {
                continue;
            }
            if f.weight == fw {
                return f.id;
            }
            regular_hit.get_or_insert(f.id);
        }
        if let Some(id) = regular_hit {
            return id;
        }
        if let Some(e) = self.emoji {
            if self.face(e).glyph(ch).is_some() {
                return e;
            }
        }
        self.primary(weight).id
    }
}

/// Every lazy pack a piece of text needs, in a stable order.
pub fn packs_for_text(text: &str) -> Vec<&'static str> {
    let mut packs = Vec::new();
    packs_for_text_into(text, &mut packs);
    packs
}

/// Every lazy pack a request needs, in a stable order, including packs that
/// are already loaded. The request is the same JSON object the renderer takes;
/// every string in it is scanned, so a quoted post's text, a card title and an
/// author name all count.
pub fn packs_for_json(json: &str) -> Vec<&'static str> {
    let value: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let mut packs = Vec::new();
    collect_packs(&value, &mut packs);
    packs
}

/// [`packs_for_json`] minus the packs whose files are already registered. This
/// is what a host asks for before a render.
pub fn missing_packs_for_json(json: &str) -> Vec<&'static str> {
    if !has_lazy_packs() {
        return Vec::new();
    }
    let mut packs = packs_for_json(json);
    packs.retain(|pack| !pack_loaded(pack));
    packs
}

fn collect_packs(value: &serde_json::Value, out: &mut Vec<&'static str>) {
    match value {
        serde_json::Value::String(text) => packs_for_text_into(text, out),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_packs(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                packs_for_text_into(key, out);
                collect_packs(item, out);
            }
        }
        _ => {}
    }
}

fn packs_for_text_into(text: &str, out: &mut Vec<&'static str>) {
    for ch in text.chars() {
        packs_for_char(ch as u32, out);
    }
}

fn add_pack(out: &mut Vec<&'static str>, pack: &'static str) {
    if !out.contains(&pack) {
        out.push(pack);
    }
}

/// Coarse script routing for one code point.
///
/// The ranges over-approximate on purpose: one unnecessary 0.3 MB pack is
/// cheaper than a tofu render, while the two expensive packs (`emoji`, `cjk`)
/// are keyed to the ranges that actually need them. The selection itself still
/// decides per glyph; this only decides what to fetch.
fn packs_for_char(cp: u32, out: &mut Vec<&'static str>) {
    let ch = char::from_u32(cp).unwrap_or('\0');
    let mut matched = false;

    // Emoji: the manifest's explicit range plus the controls that turn an
    // otherwise-text character into color emoji (VS16, keycap, ZWJ, skin tone,
    // tag sequence).
    let in_emoji_range = FACES.iter().any(|face| {
        face.kind == FaceKind::Emoji && face.ranges.iter().any(|&(lo, hi)| cp >= lo && cp <= hi)
    });
    if in_emoji_range
        || is_emoji_presentation(ch)
        || matches!(
            cp,
            0xFE0F | 0x20E3 | 0x200D | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F
        )
    {
        add_pack(out, "emoji");
        matched = true;
    }

    // Korean Hangul: Jamo, Compatibility Jamo, Enclosed Hangul, Syllables,
    // and Halfwidth Jamo. Kept in its own lazy pack so Korean posts fetch ~1.8 MB
    // without pulling the full CJK ideograph pack.
    if matches!(
        cp,
        0x1100..=0x11FF
            | 0x3130..=0x318F
            | 0x3200..=0x321E
            | 0x3260..=0x327F
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7FF
            | 0xFFA0..=0xFFDC
    ) {
        add_pack(out, "hangul");
        matched = true;
    }

    // CJK and Japanese: kana, Bopomofo, Unified + Extension A..H ideographs,
    // compatibility forms, box drawing, and the fullwidth/halfwidth forms that
    // kaomoji, Chinese, and Japanese text use.
    if matches!(
        cp,
        0x2500..=0x259F
            | 0x2E80..=0x2FFF
            | 0x3000..=0x312F
            | 0x3190..=0x31FF
            | 0x3220..=0x325F
            | 0x3280..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF9F
            | 0xFFE0..=0xFFEF
            | 0x20000..=0x323AF
    ) {
        add_pack(out, "cjk");
        matched = true;
    }

    if matches!(cp, 0x0590..=0x05FF | 0xFB1D..=0xFB4F) {
        add_pack(out, "hebrew");
        matched = true;
    }

    if matches!(
        cp,
        0x0600..=0x06FF
            | 0x0750..=0x077F
            | 0x0870..=0x089F
            | 0x08A0..=0x08FF
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF
            | 0x1EE00..=0x1EEFF
    ) {
        add_pack(out, "arabic");
        matched = true;
    }

    // Math, arrows, dingbats and the mathematical alphanumerics ("𝗯𝗼𝗹𝗱")
    // that STIX and Noto cover.
    if matches!(
        cp,
        0x2190..=0x23FF
            | 0x25A0..=0x27BF
            | 0x27C0..=0x27EF
            | 0x2980..=0x2AFF
            | 0x2B00..=0x2BFF
            | 0x1D400..=0x1D7FF
    ) {
        add_pack(out, "math");
        matched = true;
    }

    // Noto Sans is the stand-in for the Latin/IPA/Greek/Cyrillic ranges Chirp
    // and Arial do not carry. Anything already routed to a script pack stays
    // there; an unsupported script (Thai, Devanagari, …) loads nothing.
    if !matched
        && matches!(
            cp,
            0x0250..=0x02FF
                | 0x0300..=0x036F
                | 0x0530..=0x058F
                | 0x10A0..=0x10FF
                | 0x1E00..=0x1EFF
                | 0x1F00..=0x1FFF
                | 0x2C60..=0x2C7F
                | 0xA720..=0xA7FF
                | 0xAB30..=0xAB6F
                | 0xFB00..=0xFB06
        )
    {
        add_pack(out, "latin");
    }
}

/// Index of a CSS weight's Chirp bucket in [`FontDb`]'s precomputed tables.
#[inline]
fn bucket(weight: u16) -> usize {
    match chirp_weight(weight) {
        400 => 0,
        500 => 1,
        700 => 2,
        _ => 3,
    }
}

thread_local! {
    /// The answers of [`FontDb::select_uncached`] for the code points the
    /// Latin table does not cover, keyed by code point, weight bucket and the
    /// two presentation flags.
    ///
    /// Direct-mapped and deliberately tiny. A face choice walks every face's
    /// unicode ranges, twice per cluster in the worst case, and the same
    /// characters repeat across a post; a collision only costs the walk that
    /// would have happened anyway.
    static SELECT_CACHE: RefCell<Box<[(u64, FaceId); 512]>> =
        RefCell::new(Box::new([(u64::MAX, 0); 512]));
}

/// Slot of `key` in [`SELECT_CACHE`]: a Fibonacci mix, so code points that
/// differ in their low bits (consecutive runs of one script) spread out.
#[inline]
fn cache_slot(key: u64) -> usize {
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 55) as usize
}

/// The lowest and highest code point any of `ranges` covers.
fn bounds(ranges: &[(u32, u32)]) -> (u32, u32) {
    let mut lo = u32::MAX;
    let mut hi = 0;
    for &(a, b) in ranges {
        lo = lo.min(a);
        hi = hi.max(b);
    }
    (lo, hi)
}

/// Unicode space separators (Zs) and the default-ignorable format characters
/// that show up in posts.
fn is_space_or_ignorable(cp: u32) -> bool {
    matches!(cp, 0x00A0 | 0x00AD | 0x1680 | 0x2000..=0x200F | 0x2028..=0x202F | 0x205F..=0x206F | 0x3000 | 0xFEFF)
}

/// Advance of one emoji at `size`, in CSS pixels.
///
/// Measured in Chrome on macOS: Apple Color Emoji advances 1.25 em, rounded
/// to a whole pixel (19 px at 15 px text, 21 px at 17 px). iOS and macOS are
/// where most posts are read, so this is the reference.
pub fn emoji_advance(size: f32) -> f32 {
    (size * 1.25).round()
}

/// CSS weight matching against the four Chirp weights (400, 500, 700, 800).
pub fn chirp_weight(weight: u16) -> u16 {
    match weight {
        0..=450 => 400,
        451..=599 => 500,
        600..=749 => 700,
        _ => 800,
    }
}

/// True if `ch` defaults to emoji presentation (Unicode `Emoji_Presentation`).
pub fn is_emoji_presentation(ch: char) -> bool {
    let c = ch as u32;
    if c < 0x231A {
        return false;
    }
    const RANGES: &[(u32, u32)] = &[
        (0x231A, 0x231B),
        (0x23E9, 0x23EC),
        (0x23F0, 0x23F0),
        (0x23F3, 0x23F3),
        (0x25FD, 0x25FE),
        (0x2614, 0x2615),
        (0x2648, 0x2653),
        (0x267F, 0x267F),
        (0x2693, 0x2693),
        (0x26A1, 0x26A1),
        (0x26AA, 0x26AB),
        (0x26BD, 0x26BE),
        (0x26C4, 0x26C5),
        (0x26CE, 0x26CE),
        (0x26D4, 0x26D4),
        (0x26EA, 0x26EA),
        (0x26F2, 0x26F3),
        (0x26F5, 0x26F5),
        (0x26FA, 0x26FA),
        (0x26FD, 0x26FD),
        (0x2705, 0x2705),
        (0x270A, 0x270B),
        (0x2728, 0x2728),
        (0x274C, 0x274C),
        (0x274E, 0x274E),
        (0x2753, 0x2755),
        (0x2757, 0x2757),
        (0x2795, 0x2797),
        (0x27B0, 0x27B0),
        (0x27BF, 0x27BF),
        (0x2B1B, 0x2B1C),
        (0x2B50, 0x2B50),
        (0x2B55, 0x2B55),
        (0x1F004, 0x1F004),
        (0x1F0CF, 0x1F0CF),
        (0x1F18E, 0x1F18E),
        (0x1F191, 0x1F19A),
        (0x1F1E6, 0x1F1FF),
        (0x1F201, 0x1F201),
        (0x1F21A, 0x1F21A),
        (0x1F22F, 0x1F22F),
        (0x1F232, 0x1F236),
        (0x1F238, 0x1F23A),
        (0x1F250, 0x1F251),
        (0x1F300, 0x1F320),
        (0x1F32D, 0x1F335),
        (0x1F337, 0x1F37C),
        (0x1F37E, 0x1F393),
        (0x1F3A0, 0x1F3CA),
        (0x1F3CF, 0x1F3D3),
        (0x1F3E0, 0x1F3F0),
        (0x1F3F4, 0x1F3F4),
        (0x1F3F8, 0x1F43E),
        (0x1F440, 0x1F440),
        (0x1F442, 0x1F4FC),
        (0x1F4FF, 0x1F53D),
        (0x1F54B, 0x1F54E),
        (0x1F550, 0x1F567),
        (0x1F57A, 0x1F57A),
        (0x1F595, 0x1F596),
        (0x1F5A4, 0x1F5A4),
        (0x1F5FB, 0x1F64F),
        (0x1F680, 0x1F6C5),
        (0x1F6CC, 0x1F6CC),
        (0x1F6D0, 0x1F6D2),
        (0x1F6D5, 0x1F6D7),
        (0x1F6DC, 0x1F6DF),
        (0x1F6EB, 0x1F6EC),
        (0x1F6F4, 0x1F6FC),
        (0x1F7E0, 0x1F7EB),
        (0x1F7F0, 0x1F7F0),
        (0x1F90C, 0x1F93A),
        (0x1F93C, 0x1F945),
        (0x1F947, 0x1F9FF),
        (0x1FA70, 0x1FA7C),
        (0x1FA80, 0x1FA89),
        (0x1FA8F, 0x1FAC6),
        (0x1FACE, 0x1FADC),
        (0x1FADF, 0x1FAE9),
        (0x1FAF0, 0x1FAF8),
    ];
    RANGES.iter().any(|&(lo, hi)| c >= lo && c <= hi)
}

/// True if a grapheme cluster *asks* for color emoji with an explicit
/// sequence: VS16, a keycap, a skin-tone modifier, an emoji tag sequence, or a
/// ZWJ sequence starting at an arrow. Chrome honors that request over the rest
/// of the stack, which is why "©️" is emoji while "©" is Chirp.
pub fn cluster_requests_emoji(cluster: &str) -> bool {
    let mut chars = cluster.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let mut has_vs16 = false;
    let mut has_vs15 = false;
    let mut has_keycap = false;
    let mut has_zwj = false;
    // A skin-tone modifier or an emoji tag sequence (subdivision flags)
    // makes the whole cluster an emoji, as in "✌🏿".
    let mut has_modifier_or_tag = false;
    for c in chars {
        match c {
            '\u{FE0F}' => has_vs16 = true,
            '\u{FE0E}' => has_vs15 = true,
            '\u{20E3}' => has_keycap = true,
            '\u{200D}' => has_zwj = true,
            '\u{1F3FB}'..='\u{1F3FF}' | '\u{E0020}'..='\u{E007F}' => has_modifier_or_tag = true,
            _ => {}
        }
    }
    if has_vs15 {
        return false;
    }
    has_vs16 || has_keycap || has_modifier_or_tag || (has_zwj && (first as u32) >= 0x2190)
}

/// True if a grapheme cluster should be drawn with the color emoji font:
/// either an explicit request (see [`cluster_requests_emoji`]) or a character
/// Unicode draws as emoji by default (Emoji_Presentation).
pub fn cluster_is_emoji(cluster: &str) -> bool {
    let Some(first) = cluster.chars().next() else {
        return false;
    };
    cluster_requests_emoji(cluster) || is_emoji_presentation(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Latin fast path must be the general algorithm's answer, for every
    /// code point it covers and every weight bucket: it is a cache, not a
    /// second set of rules.
    #[test]
    fn latin_table_matches_the_stack_walk() {
        let db = db();
        for weight in [
            100u16, 400, 450, 451, 500, 599, 600, 700, 749, 750, 800, 900,
        ] {
            for cp in 0..256u32 {
                let ch = char::from_u32(cp).unwrap();
                assert_eq!(
                    db.select_with(ch, weight, false, false),
                    db.select_uncached(ch, weight, false, false),
                    "U+{cp:04X} at weight {weight}"
                );
            }
        }
    }

    /// The selection cache must answer exactly what the stack walk does: it is
    /// a cache, not a second set of rules. Every kind of code point the Latin
    /// table does not cover, at every weight and presentation flag.
    #[test]
    fn selection_cache_matches_the_stack_walk() {
        let db = db();
        let cps = [
            0xE9u32, // Latin-1 supplement
            0x100,   // Latin extended
            0x131,   // dotless i
            0x3B1,   // Greek
            0x416,   // Cyrillic
            0x5D0,   // Hebrew
            0x627,   // Arabic
            0x915,   // Devanagari
            0x4E00,  // CJK
            0x1F600, // emoji, no text face has it
            0x1F503, // emoji Chirp draws
            0x2190,  // arrow
            0x25AA,  // text character Arial has
            0x2764,  // heart: emoji by default
            0x2605,  // star: neither bundled face claims it
            0xFF21,  // fullwidth A
            0x202F,  // space a Chirp file claims and has no glyph for
            0x2060,  // word joiner
            0x200D,  // joiner, as in a ZWJ sequence
            0xFE0F,  // VS16
        ];
        for cp in cps {
            let ch = char::from_u32(cp).unwrap();
            for weight in [100u16, 400, 500, 600, 700, 800, 900] {
                for emoji in [false, true] {
                    for text_presentation in [false, true] {
                        assert_eq!(
                            db.select_with(ch, weight, emoji, text_presentation),
                            db.select_stack(ch, weight, emoji, text_presentation),
                            "U+{cp:04X} at weight {weight}, emoji {emoji}, text {text_presentation}"
                        );
                    }
                }
            }
        }
    }

    /// Script routing is a fetch hint, not a font decision: one pack per
    /// family, and no pack for plain English.
    #[test]
    fn pack_routing_follows_script_ranges() {
        let packs = |text: &str| packs_for_text(text);
        assert!(packs("hello, world 123").is_empty());
        assert_eq!(packs("ship it 🚀"), vec!["emoji"]);
        assert_eq!(packs("日本語"), vec!["cjk"]);
        assert_eq!(packs("正式上线"), vec!["cjk"]);
        assert_eq!(packs("안녕하세요"), vec!["hangul"]);
        assert_eq!(packs("مرحبا"), vec!["arabic"]);
        assert_eq!(packs("שלום"), vec!["hebrew"]);
        assert_eq!(packs("𝗯𝗼𝗹𝗱"), vec!["math"]);
        assert_eq!(packs("ɛ"), vec!["latin"]);
        // "é" is Chirp's own Latin-1 range: no Noto Sans fallback.
        assert!(packs("café").is_empty());
        // A mixed post names every pack once, in scan order.
        let mixed = packs_for_text("🚀 日本語 한글 𝗯𝗼𝗹𝗱");
        assert_eq!(mixed, vec!["emoji", "cjk", "hangul", "math"]);
    }

    /// And the primary face table agrees with the search it replaced.
    #[test]
    fn primary_table_matches_the_search() {
        let db = db();
        for weight in [100u16, 400, 500, 700, 800, 900] {
            let w = chirp_weight(weight);
            let found = db
                .faces
                .iter()
                .find(|f| f.kind == FaceKind::Chirp && f.weight == w && f.subset == "latin")
                .expect("a Latin Chirp face per weight");
            assert_eq!(db.primary(weight).id, found.id, "weight {weight}");
        }
    }
}
