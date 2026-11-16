//! Embeds the vendored assets (fonts, icons) into the crate.
//!
//! The assets are fetched by `node tools/sync-upstream.mjs` into
//! `assets/vendor/` and are deliberately not committed. `XEET_VENDOR_DIR`
//! overrides the location, which CI uses to point at a restored snapshot.

use std::{env, fmt::Write as _, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest_dir.join("../..");
    let vendor = env::var("XEET_VENDOR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("assets/vendor"));
    println!("cargo:rerun-if-env-changed=XEET_VENDOR_DIR");

    // `XEET_FONT_PACKS=core` embeds only Chirp and Arial; every other face is
    // left empty and registered at runtime from a pack the host fetched. The
    // native CLI keeps the default (all faces embedded), the WebAssembly build
    // opts in so its module starts at ~6.4 MB instead of ~45 MB.
    let core_only = matches!(
        env::var("XEET_FONT_PACKS").as_deref(),
        Ok("core") | Ok("lazy")
    );
    println!("cargo:rerun-if-env-changed=XEET_FONT_PACKS");

    let fonts_manifest = vendor.join("fonts/manifest.json");
    let Ok(manifest_text) = fs::read_to_string(&fonts_manifest) else {
        panic!(
            "\n\nxeet: {} not found.\nFonts and icons are not committed. Fetch them first:\n\n    cd tools && pnpm install && cd .. && node tools/sync-upstream.mjs\n\n",
            fonts_manifest.display()
        );
    };
    println!("cargo:rerun-if-changed={}", fonts_manifest.display());
    let vendor = vendor.canonicalize().unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_text).expect("fonts/manifest.json is not valid JSON");

    let mut out = String::new();
    out.push_str("pub static FACES: &[FaceData] = &[\n");
    for face in manifest.as_array().expect("manifest must be an array") {
        let kind = match face["kind"].as_str().unwrap_or("chirp") {
            "emoji" => "Emoji",
            "fallback" => "Fallback",
            _ => "Chirp",
        };
        let file = vendor.join("fonts").join(face["file"].as_str().unwrap());
        println!("cargo:rerun-if-changed={}", file.display());
        let family = face["family"].as_str().unwrap();
        let pack = pack_for(family);
        // `data: &[]` is the lazy marker: `fonts::face_bytes` looks the file up
        // in the runtime pack registry instead. Core faces always embed.
        let data = if core_only && pack != "core" {
            "&[]".to_string()
        } else {
            format!("include_bytes!({:?})", file.display().to_string())
        };
        let mut ranges = String::new();
        for r in face["ranges"].as_array().unwrap() {
            write!(
                ranges,
                "({}, {}), ",
                r[0].as_u64().unwrap(),
                r[1].as_u64().unwrap()
            )
            .unwrap();
        }
        writeln!(
            out,
            "    FaceData {{ kind: FaceKind::{kind}, weight: {}, family: {:?}, subset: {:?}, file: {:?}, pack: {pack:?}, ranges: &[{ranges}], data: {data} }},",
            face["weight"].as_u64().unwrap(),
            family,
            face["subset"].as_str().unwrap(),
            face["file"].as_str().unwrap(),
        )
        .unwrap();
    }
    out.push_str("];\n");

    let icons = vendor.join("icons.json");
    println!("cargo:rerun-if-changed={}", icons.display());
    writeln!(
        out,
        "pub static ICONS_JSON: &str = include_str!({:?});",
        icons.display().to_string()
    )
    .unwrap();

    // X's default profile image: what x.com draws for an account with no avatar.
    // Embedded so every target (native, HTTP, WASM) draws the same thing without
    // I/O. Tolerated as absent — a tree whose vendor snapshot predates this file
    // still builds, and the renderer falls back to its placeholder instead.
    let default_avatar = vendor.join("default-avatar.png");
    println!("cargo:rerun-if-changed={}", default_avatar.display());
    if default_avatar.exists() {
        writeln!(
            out,
            "/// X's own default profile image (`assets/vendor/default-avatar.png`).\npub static DEFAULT_AVATAR_PNG: &[u8] = include_bytes!({:?});",
            default_avatar.display().to_string()
        )
        .unwrap();
    } else {
        out.push_str("/// The vendored default avatar is missing in this tree; see `default_avatar()`.\npub static DEFAULT_AVATAR_PNG: &[u8] = &[];\n");
    }

    // Which upstream snapshot this build embeds; surfaced in every report.
    let lock_path = root.join("assets/upstream.lock.json");
    println!("cargo:rerun-if-changed={}", lock_path.display());
    let lock: serde_json::Value = fs::read_to_string(&lock_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let app = lock["appVersion"].as_str().unwrap_or("unknown");
    let fetched = lock["fetchedAt"].as_str().unwrap_or("unknown");
    writeln!(out, "pub const UPSTREAM_APP_VERSION: &str = {app:?};").unwrap();
    writeln!(out, "pub const UPSTREAM_FETCHED_AT: &str = {fetched:?};").unwrap();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Design tokens, extracted from X's stylesheet by tools/extract-tokens.mjs.
    // Emitted separately from `assets_gen.rs` so the frame's constants are
    // independent of the font plumbing.
    let tokens_path = vendor.join("tokens.json");
    println!("cargo:rerun-if-changed={}", tokens_path.display());
    let tokens_text = fs::read_to_string(&tokens_path).unwrap_or_else(|_| {
        panic!(
            "\n\nxeet: {} not found.\nDesign tokens are generated from X's stylesheet and are not committed:\n\n    node tools/extract-tokens.mjs\n\n",
            tokens_path.display()
        )
    });
    let tokens: serde_json::Value =
        serde_json::from_str(&tokens_text).expect("tokens.json is not valid JSON");
    let mut t = String::new();
    let num = |path: &[&str]| -> f64 {
        let mut v = &tokens;
        for key in path {
            v = v
                .get(key)
                .unwrap_or_else(|| panic!("tokens.json is missing {}", path.join(".")));
        }
        v.as_f64()
            .unwrap_or_else(|| panic!("tokens.json: {} is not a number", path.join(".")))
    };
    // `f32` constants need a float literal, so `15` must be written `15.0`.
    let f = |path: &[&str]| -> String {
        let v = num(path);
        if v.fract() == 0.0 {
            format!("{v:.1}")
        } else {
            format!("{v}")
        }
    };
    let text = |path: &[&str]| -> String {
        let mut v = &tokens;
        for key in path {
            v = v
                .get(key)
                .unwrap_or_else(|| panic!("tokens.json is missing {}", path.join(".")));
        }
        v.as_str()
            .unwrap_or_else(|| panic!("tokens.json: {} is not a string", path.join(".")))
            .to_string()
    };
    writeln!(
        t,
        "// Generated by build.rs from assets/vendor/tokens.json. Do not edit."
    )
    .unwrap();
    writeln!(
        t,
        "pub const SOURCE_SHA256: &str = {:?};",
        text(&["source", "stylesheetSha256"])
    )
    .unwrap();
    writeln!(t, "pub const SPACING: f32 = {};", f(&["units", "spacing"])).unwrap();
    writeln!(
        t,
        "pub const ROOT_FONT_SIZE: f32 = {};",
        f(&["units", "rootFontSize"])
    )
    .unwrap();
    writeln!(
        t,
        "pub const RADIUS_MULTIPLIER: f32 = {};",
        f(&["units", "radiusMultiplier"])
    )
    .unwrap();
    writeln!(
        t,
        "pub const RADIUS_MULTIPLIER_SQUIRCLE: f32 = {};",
        f(&["units", "radiusMultiplierSquircle"])
    )
    .unwrap();
    for style in [
        "body",
        "lead",
        "subtext1",
        "subtext2",
        "headline1",
        "headline2",
        "title4",
    ] {
        let up = style.to_uppercase();
        writeln!(
            t,
            "pub const TYPE_{up}_SIZE: f32 = {};",
            f(&["type", style, "size"])
        )
        .unwrap();
        writeln!(
            t,
            "pub const TYPE_{up}_LINE_HEIGHT: f32 = {};",
            f(&["type", style, "lineHeight"])
        )
        .unwrap();
    }
    for weight in ["normal", "medium", "semibold", "bold", "extrabold"] {
        writeln!(
            t,
            "pub const WEIGHT_{}: u16 = {};",
            weight.to_uppercase(),
            num(&["weights", weight])
        )
        .unwrap();
    }
    let layout_keys = [
        ("PADDING_X", "paddingX"),
        ("PADDING_Y", "paddingY"),
        ("AVATAR_SIZE", "avatarSize"),
        ("AVATAR_GAP", "avatarGap"),
        ("GAP_TIGHT", "gapTight"),
        ("GAP_HALF", "gapHalf"),
        ("GAP_ROW", "gapRow"),
        ("GAP_BLOCK", "gapBlock"),
        ("MORE_BUTTON_SIZE", "moreButtonSize"),
        ("MORE_ICON_SIZE", "moreIconSize"),
        ("MORE_MARGIN_Y", "moreMarginY"),
        ("MORE_MARGIN_END_TIMELINE", "moreMarginEndTimeline"),
        ("MORE_MARGIN_END_DETAIL", "moreMarginEndDetail"),
        ("ACTION_ROW_HEIGHT", "actionRowHeight"),
        ("ACTION_ROW_BLEED", "actionRowBleed"),
        ("PILL_HEIGHT_TIMELINE", "pillHeightTimeline"),
        ("PILL_PADDING_X_TIMELINE", "pillPaddingXTimeline"),
        ("PILL_ICON_SIZE_TIMELINE", "pillIconSizeTimeline"),
        ("PILL_HEIGHT_DETAIL", "pillHeightDetail"),
        ("PILL_PADDING_X_DETAIL", "pillPaddingXDetail"),
        ("PILL_ICON_SIZE_DETAIL", "pillIconSizeDetail"),
        ("COUNT_PADDING_START", "countPaddingStart"),
        ("DOT_PADDING", "dotPadding"),
        ("BADGE_SIZE", "badgeSize"),
        (
            "BOOKMARK_HIDDEN_BELOW_ARTICLE_WIDTH",
            "bookmarkHiddenBelowArticleWidth",
        ),
    ];
    for (const_name, key) in layout_keys {
        writeln!(
            t,
            "pub const LAYOUT_{const_name}: f32 = {};",
            f(&["layout", key])
        )
        .unwrap();
    }
    // Radii: the effective value (with `--x-radius-m: 1.8` where Chrome
    // satisfies `@supports (corner-shape: squircle)`), the round fallback, and
    // the corner shape per class.
    for radius in ["xs", "sm", "md", "lg", "xl", "2xl", "full"] {
        let up = radius.to_uppercase();
        writeln!(t, "pub const RADIUS_{up}: f32 = {};", f(&["radii", radius])).unwrap();
        writeln!(
            t,
            "pub const RADIUS_{up}_ROUND: f32 = {};",
            f(&["radiiRound", radius])
        )
        .unwrap();
        let shape = text(&["cornerShape", radius]);
        let shape = if shape == "squircle" {
            "Squircle"
        } else {
            "Round"
        };
        writeln!(
            t,
            "pub const RADIUS_{up}_SHAPE: CornerShape = CornerShape::{shape};"
        )
        .unwrap();
    }
    for (theme, prefix) in [("light", "LIGHT"), ("dark", "DARK")] {
        for color in [
            "bgPrimary",
            "bgSecondary",
            "fgPrimary",
            "fgSecondary",
            "fgTertiary",
            "fgBrand",
            "borderNormal",
            "borderDivider",
        ] {
            let hex = text(&["colors", theme, color]);
            let (r, g, b, a) = parse_hex(&hex);
            writeln!(
                t,
                "pub const {prefix}_{}: RgbaParts = RgbaParts({r}, {g}, {b}, {a});",
                scream(color)
            )
            .unwrap();
        }
    }
    fs::write(out_dir.join("tokens_gen.rs"), t).unwrap();

    fs::write(out_dir.join("assets_gen.rs"), out).unwrap();
}

/// Which lazy pack a face belongs to. `core` is always embedded; every other
/// pack is a separate file the host registers before rendering text that needs
/// it. The groups are deliberately coarse: one fetch per script family, not one
/// per file.
fn pack_for(family: &str) -> &'static str {
    match family {
        "TwitterChirp" | "Arial" => "core",
        "Apple Color Emoji" => "emoji",
        "Hiragino Sans" | "Noto Sans SC" => "cjk",
        "Noto Sans KR" => "hangul",
        "Noto Sans Arabic" => "arabic",
        "Noto Sans Hebrew" => "hebrew",
        "Noto Sans" => "latin",
        // STIX Two Math, Noto Sans Math, Noto Sans Symbols, Noto Sans Symbols 2.
        _ => "math",
    }
}

/// `bgPrimary` to `BG_PRIMARY`, so generated constant names stay readable.
fn scream(camel: &str) -> String {
    let mut out = String::new();
    for (i, c) in camel.char_indices() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// `#rrggbb` or `#rrggbbaa` to bytes; the extractor only emits those two forms.
fn parse_hex(hex: &str) -> (u8, u8, u8, u8) {
    let s = hex.trim_start_matches('#');
    let byte =
        |i: usize| u8::from_str_radix(&s[i..i + 2], 16).expect("tokens.json has a bad color");
    let alpha = if s.len() >= 8 { byte(6) } else { 255 };
    (byte(0), byte(2), byte(4), alpha)
}
