//! Font handles for painting.
//!
//! Shaping (`fonts::Face`) parses each file through fontations' newer model
//! API; painting wants the classic `skrifa` outline, color and palette APIs,
//! which both read the same bytes. A face's bytes live for the process — either
//! embedded in the module or leaked by the runtime registry — so these
//! `FontRef`s (`Copy`, 24 bytes) are parsed once per font generation and shared.

use crate::fonts::{face_bytes, generation, FaceId, FACES};
use skrifa::FontRef;
use std::sync::{OnceLock, RwLock};

type Slot = Option<(u32, FontRef<'static>)>;

fn all() -> &'static RwLock<Vec<Slot>> {
    static FONTS: OnceLock<RwLock<Vec<Slot>>> = OnceLock::new();
    FONTS.get_or_init(|| RwLock::new(vec![None; FACES.len()]))
}

/// Emoji advance in CSS pixels, the same rule layout uses.
pub(crate) use crate::fonts::emoji_advance;

pub(crate) fn font(id: FaceId) -> Option<FontRef<'static>> {
    let gen = generation();
    {
        let slots = all().read().unwrap_or_else(|e| e.into_inner());
        if let Some(Some((slot_gen, font_ref))) = slots.get(id as usize) {
            if *slot_gen == gen {
                return Some(font_ref.clone());
            }
        }
    }
    let spec = FACES.get(id as usize)?;
    let data = face_bytes(spec.file)?;
    let font_ref = FontRef::new(data).ok()?;
    let mut slots = all().write().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = slots.get_mut(id as usize) {
        *slot = Some((gen, font_ref.clone()));
    }
    Some(font_ref)
}
