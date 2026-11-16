#!/usr/bin/env node
// Fetches the upstream assets xeet is verified against and pins them.
//
//   node tools/sync-upstream.mjs            fetch, convert, write the lockfile
//   node tools/sync-upstream.mjs --check    one request; exit 3 if upstream moved
//
// Everything downloaded is X's property. It goes to assets/vendor/, which is
// git-ignored. Only assets/upstream.lock.json (URLs and hashes) is committed.
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { writeTokens } from "./extract-tokens.mjs";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const VENDOR = join(ROOT, "assets", "vendor");
const LOCK = join(ROOT, "assets", "upstream.lock.json");
// A stable public post. Its page carries the stylesheet link and the icon sprite.
const PROBE_URL = "https://x.com/jack/status/20";
const UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

const NOTO = "https://github.com/notofonts/notofonts.github.io/raw/main/fonts";

// The ranges below are derived from the fonts on a Mac, not chosen: X's stack
// is TwitterChirp, -apple-system, BlinkMacSystemFont, Segoe UI, Roboto,
// Helvetica, Arial, Apple Color Emoji, …, sans-serif, and Chrome walks it per
// character. A family can therefore only win the characters no earlier family
// in the stack covers. `node tools/oracle/font-ranges.mjs` re-derives both
// ranges and prints them in this form; the measurements behind them are in
// tools/oracle/fonts-used.mjs and PLAN.md §14.
//
// Arial: its own cmap minus what SF and Helvetica cover, i.e. exactly the
// characters Chrome on Apple reaches Arial for. Measured: on the sample, every
// character Chrome gives Arial gets the same advance (0.00 px error) from the
// bundled file, so this is an exact match, not an approximation. Arial is
// Monotype's: like Chirp it is fetched at build time and never committed. This
// is the Windows 5.21 build; it has the same coverage as Apple's 5.01 for
// these characters and lays out identically in Chrome (87 of 87 probe strings,
// regular and bold, including the "الله" ligature, Urdu and vowelled text).
const ARIAL_RANGE =
  "U+0184-0185,U+0187-0188,U+0193,U+0195,U+019C,U+01A2-01A5,U+01AA-01AD,U+01B2,U+01BA," +
  "U+01BC-01BF,U+01F6,U+0221-0225,U+0234-0236,U+023F-0240,U+03D0-03DB,U+03DE-03F1," +
  "U+0460-0461,U+046E-046F,U+047A-0486,U+0488-0489,U+0502-050F,U+0591-05C7,U+05D0-05EA," +
  "U+05F0-05F4,U+0600-0603,U+060B-0615,U+061B,U+061E-061F,U+0621-063A,U+0640-065E," +
  "U+0660-06FF,U+0750-076D,U+1D02-1D03,U+1D10,U+1D15,U+1D23-1D25,U+1D2C-1D42,U+1D5C,U+1D60," +
  "U+1D69,U+1D6F,U+1D74,U+1D76-1D79,U+1D7C-1DBA,U+1DBC-1DBE,U+1DC2-1DC3,U+1DFE-1DFF," +
  "U+20AF-20B0,U+2113,U+212E,U+2195,U+21A8,U+2215,U+221F,U+2229,U+2302,U+2310,U+2320-2321," +
  "U+251C,U+2524,U+252C,U+2534,U+253C,U+2550-256C,U+2580,U+2584,U+258C,U+2590-2593," +
  "U+25AA-25AC,U+25BA,U+25C4,U+25CC,U+25D8-25D9,U+263C,U+2640,U+2642,U+266B,U+2C74,U+2C77," +
  "U+FB1D-FB36,U+FB38-FB3C,U+FB3E,U+FB40-FB41,U+FB43-FB44,U+FB46-FBB1,U+FBD3-FBE9," +
  "U+FBFC-FBFF,U+FC5E-FC62,U+FD3E-FD3F,U+FDF2,U+FDFC,U+FE20-FE23,U+FE80-FEFC";

// Twemoji: Apple Color Emoji's cmap minus the three text families ahead of it
// in X's stack (minus the variation selectors and the keycap combiner, which
// are handled with their cluster). These are the characters Chrome on Apple
// draws as emoji without a VS16: "❤", "✔", "☀". Characters SF covers instead,
// such as "★", "✈" and "☺", are text on macOS and stay text here.
const EMOJI_RANGE =
  "U+200C-200D,U+20E3,U+2139,U+21A9-21AA,U+231A-231B,U+2328,U+23E9-23F3,U+23F8-23FA," +
  "U+25FB-25FE,U+2600-2604,U+260E,U+2614-2615,U+2618,U+261D,U+2620,U+2622-2623,U+2626," +
  "U+262A,U+262E-262F,U+2638,U+2648-2653,U+265F,U+2668,U+267B,U+267E-267F,U+2692-2697," +
  "U+2699,U+269B-269C,U+26A1,U+26A7,U+26AA-26AB,U+26B0-26B1,U+26BD-26BE,U+26C4-26C5,U+26C8," +
  "U+26CE-26CF,U+26D1,U+26D3-26D4,U+26E9-26EA,U+26F0-26F5,U+26F7-26FA,U+26FD,U+2702,U+2705," +
  "U+2709-270D,U+270F,U+2712,U+2714,U+2716,U+271D,U+2721,U+2728,U+2733-2734,U+2744,U+2747," +
  "U+274C,U+274E,U+2753-2755,U+2757,U+2763-2764,U+2795-2797,U+27A1,U+27B0,U+27BF," +
  "U+2B05-2B07,U+2B1B-2B1C,U+2B50,U+2B55,U+3030,U+303D,U+3297,U+3299,U+FE0F,U+1F004," +
  "U+1F0CF,U+1F18E,U+1F191-1F19A,U+1F1E6-1F1FF,U+1F201-1F202,U+1F21A,U+1F22F,U+1F232-1F23A," +
  "U+1F250-1F251,U+1F300-1F321,U+1F324-1F393,U+1F396-1F397,U+1F399-1F39B,U+1F39E-1F3F0," +
  "U+1F3F3-1F3F5,U+1F3F7-1F4FD,U+1F4FF-1F53D,U+1F549-1F54E,U+1F550-1F567,U+1F56F-1F570," +
  "U+1F573-1F57A,U+1F587,U+1F58A-1F58D,U+1F590,U+1F595-1F596,U+1F5A4-1F5A5,U+1F5A8," +
  "U+1F5B1-1F5B2,U+1F5BC,U+1F5C2-1F5C4,U+1F5D1-1F5D3,U+1F5DC-1F5DE,U+1F5E1,U+1F5E3,U+1F5E8," +
  "U+1F5EF,U+1F5F3,U+1F5FA-1F64F,U+1F680-1F6C5,U+1F6CB-1F6D2,U+1F6D5-1F6D8,U+1F6DC-1F6E5," +
  "U+1F6E9,U+1F6EB-1F6EC,U+1F6F0,U+1F6F3-1F6FC,U+1F7E0-1F7EB,U+1F7F0,U+1F90C-1F93A," +
  "U+1F93C-1F945,U+1F947-1F9FF,U+1FA70-1FA7C,U+1FA80-1FA8A,U+1FA8E-1FAC6,U+1FAC8," +
  "U+1FACD-1FADC,U+1FADF-1FAEA,U+1FAEF-1FAF8,U+E0061-E007A,U+E007F";

// Noto Sans Symbols 2 draws its dingbats heavier than the Apple fonts it stands
// in for, and a few symbol ranges show it: macOS shows "✓" as a hairline (SF),
// Symbols 2 has a brush stroke. For the ranges below STIX Two Math has the same
// character in a lighter weight (and, except for v and the ballot box with a
// check, a nearer advance), so Symbols 2 gives them up and the walk continues
// to STIX. Measured on this Mac with Chrome 141 under X's real stack
// (`node tools/oracle/fonts-used.mjs "✓ 57"` shows the SF pick); advances are
// at 15 px:
//
//   ✓                     U+2713        SF 12.77 hairline  | Symbols 2 12.44 brush   | STIX 11.60 hairline
//   ☑ ☒ ballot boxes      U+2611-2612   SF 13.38 hairline  | Symbols 2 12.33 brush   | STIX 11.45 / 13.98 hairline
//   ⇦⇧⇨⇩⇪ block arrows    U+21E6-21EA   SF 14.25-15.0 thin | Symbols 2 9.93/12.23 bold | STIX 10.80/14.22 thin
//   ♔♕♖♗♘♙♚♛♜♝♞ chess     U+2654-265E   Menlo 9.03 hairline | Symbols 2 15.00 bold    | STIX 8.15-11.19 hairline
//
// Only ✓ buys shape with a slightly worse advance; the rest improve both. Two
// cases stay heavy because no bundled face has a light one: ✗ U+2717 and
// ✘ U+2718 (Symbols 2 only), and so does ✔︎ U+2714 with VS15. ♟ U+265F is not
// here either: Apple draws it as emoji, so the emoji face reaches it first.
export const SYMBOL_STAND_IN_EXCEPTIONS = "U+21E6-21EA,U+2611-2612,U+2654-265E,U+2713";
const SYMBOLS2_RANGE = complement("U+0-10FFFF", SYMBOL_STAND_IN_EXCEPTIONS);

/** macOS system Apple Color Emoji TTC; extracted to a single 64 ppem strike in assets/vendor/fonts/. */
const APPLE_EMOJI_TTC = "/System/Library/Fonts/Apple Color Emoji.ttc";
const APPLE_EMOJI_PPEM = 64;
const APPLE_EMOJI_SPEC = {
  kind: "emoji",
  family: "Apple Color Emoji",
  weight: 400,
  subset: "emoji",
  file: "AppleColorEmoji.ttf",
  url: `system://${APPLE_EMOJI_TTC}#ppem=${APPLE_EMOJI_PPEM}`,
  unicodeRange: EMOJI_RANGE,
};

// Order is X's stack order, and it is what the engine walks: Arial first, then
// Apple Color Emoji (matching X's CSS stack on iOS/macOS), then the stand-ins
// for the system fallback (Apple Symbols, Menlo, Hiragino, STIX, …).
export const OPEN_FONTS = [
  { kind: "fallback", family: "Arial", weight: 400, subset: "text", file: "Arial.ttf",
    url: "https://raw.githubusercontent.com/wolfsonliu/web_typography/master/fonts/arial.ttf", unicodeRange: ARIAL_RANGE },
  { kind: "fallback", family: "Arial", weight: 700, subset: "text", file: "Arial-Bold.ttf",
    url: "https://raw.githubusercontent.com/wolfsonliu/web_typography/master/fonts/arialbd.ttf", unicodeRange: ARIAL_RANGE },
  APPLE_EMOJI_SPEC,
  // Noto (SIL OFL 1.1): text and symbol fallback, in lookup order.
  { kind: "fallback", family: "Noto Sans", weight: 400, subset: "text", file: "NotoSans-Regular.ttf",
    url: `${NOTO}/NotoSans/unhinted/ttf/NotoSans-Regular.ttf` },
  { kind: "fallback", family: "Noto Sans", weight: 700, subset: "text", file: "NotoSans-Bold.ttf",
    url: `${NOTO}/NotoSans/unhinted/ttf/NotoSans-Bold.ttf` },
  { kind: "fallback", family: "Noto Sans Arabic", weight: 400, subset: "arabic", file: "NotoSansArabic-Regular.ttf",
    url: `${NOTO}/NotoSansArabic/unhinted/ttf/NotoSansArabic-Regular.ttf` },
  { kind: "fallback", family: "Noto Sans Arabic", weight: 700, subset: "arabic", file: "NotoSansArabic-Bold.ttf",
    url: `${NOTO}/NotoSansArabic/unhinted/ttf/NotoSansArabic-Bold.ttf` },
  { kind: "fallback", family: "Noto Sans Hebrew", weight: 400, subset: "hebrew", file: "NotoSansHebrew-Regular.ttf",
    url: `${NOTO}/NotoSansHebrew/unhinted/ttf/NotoSansHebrew-Regular.ttf` },
  { kind: "fallback", family: "Noto Sans Hebrew", weight: 700, subset: "hebrew", file: "NotoSansHebrew-Bold.ttf",
    url: `${NOTO}/NotoSansHebrew/unhinted/ttf/NotoSansHebrew-Bold.ttf` },
  { kind: "fallback", family: "Noto Sans Symbols", weight: 400, subset: "symbols", file: "NotoSansSymbols-Regular.ttf",
    url: `${NOTO}/NotoSansSymbols/unhinted/ttf/NotoSansSymbols-Regular.ttf` },
  // Noto Sans Symbols 2 is the stand-in for the symbol fonts Apple ships that
  // cannot be bundled, but its dingbats are drawn heavier than Apple's: a
  // brush-stroke check where macOS shows a hairline. For the few ranges where
  // STIX Two Math has the same character in a nearer weight, this face steps
  // aside so the walk continues to STIX (see SYMBOL_STAND_IN_EXCEPTIONS).
  { kind: "fallback", family: "Noto Sans Symbols 2", weight: 400, subset: "symbols2", file: "NotoSansSymbols2-Regular.ttf",
    url: `${NOTO}/NotoSansSymbols2/unhinted/ttf/NotoSansSymbols2-Regular.ttf`,
    unicodeRange: SYMBOLS2_RANGE },
  // Styled letters (U+1D400 Mathematical Alphanumerics, the "𝗯𝗼𝗹𝗱" of
  // marketing posts): Apple devices draw them with STIX Two Math, which is
  // OFL, so these match iOS and macOS exactly. Noto Sans Math stays as the
  // Android-style fallback for anything STIX lacks.
  { kind: "fallback", family: "STIX Two Math", weight: 400, subset: "math", file: "STIXTwoMath-Regular.otf",
    url: "https://mirrors.ctan.org/fonts/stix2-otf/STIXTwoMath-Regular.otf" },
  { kind: "fallback", family: "Noto Sans Math", weight: 400, subset: "math", file: "NotoSansMath-Regular.ttf",
    url: `${NOTO}/NotoSansMath/unhinted/ttf/NotoSansMath-Regular.ttf` },
  // Hiragino Sans (macOS system CJK / box-drawing / kaomoji fallback, face 0):
  // covers Box Drawing (U+2500–U+259F: ┻, ┳, ╯), Halfwidth Katakana (ﾉ), Kana,
  // and CJK ideographs with the exact glyphs and advances Chrome on macOS uses.
  { kind: "fallback", family: "Hiragino Sans", weight: 400, subset: "cjk", file: "HiraginoSans-Regular.otf",
    url: "system:///System/Library/Fonts/ヒラギノ角ゴシック W3.ttc#face=0" },
  { kind: "fallback", family: "Hiragino Sans", weight: 700, subset: "cjk", file: "HiraginoSans-Bold.otf",
    url: "system:///System/Library/Fonts/ヒラギノ角ゴシック W6.ttc#face=0" },
  // Noto Sans CJK SC & KR (SIL OFL 1.1): delta-subsetted to the Simplified /
  // Traditional Chinese + Extension A ideographs Hiragino lacks, plus Korean
  // Hangul in its own lazy pack (`hangul`). Placing SC after Hiragino preserves
  // 100% of Chrome-macOS Japanese/kaomoji parity while covering GB18030/BMP/Ext-A.
  { kind: "fallback", family: "Noto Sans SC", weight: 400, subset: "cjk", file: "NotoSansSC-Regular.otf",
    url: "https://github.com/googlefonts/noto-cjk/raw/main/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf#delta=hiragino",
    unicodeRange: "U+2E80-33FF,U+3400-4DBF,U+4E00-9FFF,U+F900-FAFF,U+FE30-FE4F,U+FF00-FFEF,U+20000-2FA1F" },
  { kind: "fallback", family: "Noto Sans SC", weight: 700, subset: "cjk", file: "NotoSansSC-Bold.otf",
    url: "https://github.com/googlefonts/noto-cjk/raw/main/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Bold.otf#delta=hiragino",
    unicodeRange: "U+2E80-33FF,U+3400-4DBF,U+4E00-9FFF,U+F900-FAFF,U+FE30-FE4F,U+FF00-FFEF,U+20000-2FA1F" },
  { kind: "fallback", family: "Noto Sans KR", weight: 400, subset: "hangul", file: "NotoSansKR-Regular.otf",
    url: "https://github.com/googlefonts/noto-cjk/raw/main/Sans/OTF/Korean/NotoSansCJKkr-Regular.otf#subset=hangul",
    unicodeRange: "U+1100-11FF,U+3130-318F,U+3200-321E,U+3260-327F,U+A960-A97F,U+AC00-D7FF,U+FFA0-FFDC" },
  { kind: "fallback", family: "Noto Sans KR", weight: 700, subset: "hangul", file: "NotoSansKR-Bold.otf",
    url: "https://github.com/googlefonts/noto-cjk/raw/main/Sans/OTF/Korean/NotoSansCJKkr-Bold.otf#subset=hangul",
    unicodeRange: "U+1100-11FF,U+3130-318F,U+3200-321E,U+3260-327F,U+A960-A97F,U+AC00-D7FF,U+FFA0-FFDC" },
];

/** X's default profile image: what x.com draws for an account with no avatar. */
const DEFAULT_AVATAR_URL =
  "https://abs.twimg.com/sticky/default_profile_images/default_profile_400x400.png";

const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

async function get(url, as = "buffer") {
  const res = await fetch(url, { headers: { "user-agent": UA } });
  if (!res.ok) throw new Error(`${res.status} for ${url}`);
  return as === "text" ? res.text() : Buffer.from(await res.arrayBuffer());
}

/** Reads the identifiers that change when X ships: app version and hashed asset URLs. */
function fingerprintShell(html) {
  const appVersion = html.match(/data-app-version="([^"]+)"/)?.[1] ?? null;
  const stylesheet =
    html.match(/href="(https:\/\/abs\.twimg\.com\/[^"]+\/styles-[^"]+\.css)"/)?.[1] ?? null;
  return { appVersion, stylesheet };
}

function parseFontFaces(css) {
  const faces = [];
  for (const m of css.matchAll(/@font-face\{([^}]*)\}/g)) {
    const body = m[1];
    const family = body.match(/font-family:([^;]+)/)?.[1]?.trim().replace(/["']/g, "");
    const url = body.match(/url\(([^)]+)\)/)?.[1]?.replace(/["']/g, "");
    const weight = Number(body.match(/font-weight:(\d+)/)?.[1] ?? 400);
    const unicodeRange = body.match(/unicode-range:([^;]+)/)?.[1]?.trim() ?? "U+0-10FFFF";
    if (!family || !url) continue;
    const file = url.split("/").pop(); // Chirp-Regular.c88864db.latin.woff2
    const [name, , subset] = file.replace(/\.woff2$/, "").split(".");
    faces.push({ family, weight, subset, name, url, unicodeRange });
  }
  return faces;
}

/** "U+??,U+131,U+152-153" -> [[0,255],[0x131,0x131],[0x152,0x153]] */
export function parseUnicodeRange(spec) {
  return spec.split(",").map((part) => {
    const s = part.trim().replace(/^U\+/i, "");
    if (s.includes("?")) {
      return [parseInt(s.replace(/\?/g, "0"), 16), parseInt(s.replace(/\?/g, "F"), 16)];
    }
    const [a, b] = s.split("-");
    return [parseInt(a, 16), parseInt(b ?? a, 16)];
  });
}

/**
 * `whole` minus `cut`, both unicode-range strings: the range a face keeps when
 * it steps aside for a character another face draws better.
 *
 *   complement("U+0-10FFFF", "U+2713") -> "U+0000-2712,U+2714-10FFFF"
 */
export function complement(whole, cut) {
  const [[start, end]] = parseUnicodeRange(whole);
  const out = [];
  let next = start;
  for (const [lo, hi] of parseUnicodeRange(cut).sort((a, b) => a[0] - b[0])) {
    if (lo > next) out.push([next, Math.min(lo - 1, end)]);
    next = Math.max(next, hi + 1);
  }
  if (next <= end) out.push([next, end]);
  const hex = (c) => c.toString(16).toUpperCase().padStart(4, "0");
  return out.map(([a, b]) => (a === b ? `U+${hex(a)}` : `U+${hex(a)}-${hex(b)}`)).join(",");
}

function sfntChecksum(buf) {
  let sum = 0;
  const end = buf.length & ~3;
  for (let i = 0; i < end; i += 4) sum = (sum + buf.readUInt32BE(i)) >>> 0;
  if (end < buf.length) {
    const tail = Buffer.alloc(4);
    buf.copy(tail, 0, end);
    sum = (sum + tail.readUInt32BE(0)) >>> 0;
  }
  return sum;
}

/**
 * Extracts face 0 (`Apple Color Emoji`) from macOS's `Apple Color Emoji.ttc` as
 * a standalone `.ttf` keeping only the `targetPpem` (64 px) `sbix` strike (~19 MB
 * instead of 192 MB).
 */
export function extractAppleColorEmoji(ttc, targetPpem = APPLE_EMOJI_PPEM) {
  if (ttc.toString("latin1", 0, 4) !== "ttcf") {
    throw new Error("Apple Color Emoji.ttc does not start with ttcf");
  }
  const faceOffset = ttc.readUInt32BE(12);
  const sfntVersion = ttc.readUInt32BE(faceOffset);
  const numTables = ttc.readUInt16BE(faceOffset + 4);
  const entries = [];
  for (let i = 0; i < numTables; i++) {
    const rec = faceOffset + 12 + i * 16;
    const tag = ttc.toString("latin1", rec, rec + 4);
    const offset = ttc.readUInt32BE(rec + 8);
    const length = ttc.readUInt32BE(rec + 12);
    entries.push({ tag, data: Buffer.from(ttc.subarray(offset, offset + length)) });
  }
  const sbixEntry = entries.find((e) => e.tag === "sbix");
  if (!sbixEntry) throw new Error("Apple Color Emoji face 0 has no sbix table");
  const sbix = sbixEntry.data;
  const version = sbix.readUInt16BE(0);
  const flags = sbix.readUInt16BE(2);
  const numStrikes = sbix.readUInt32BE(4);
  const strikeOffsets = [];
  for (let i = 0; i < numStrikes; i++) strikeOffsets.push(sbix.readUInt32BE(8 + i * 4));
  let chosen = -1;
  for (let i = 0; i < numStrikes; i++) {
    if (sbix.readUInt16BE(strikeOffsets[i]) === targetPpem) {
      chosen = i;
      break;
    }
  }
  if (chosen < 0) throw new Error(`Apple Color Emoji has no ${targetPpem} ppem strike`);
  const sStart = strikeOffsets[chosen];
  const sEnd = chosen + 1 < numStrikes ? strikeOffsets[chosen + 1] : sbix.length;
  const strikeBytes = sbix.subarray(sStart, sEnd);
  const newSbix = Buffer.alloc(12 + strikeBytes.length);
  newSbix.writeUInt16BE(version, 0);
  newSbix.writeUInt16BE(flags, 2);
  newSbix.writeUInt32BE(1, 4);
  newSbix.writeUInt32BE(12, 8);
  strikeBytes.copy(newSbix, 12);
  sbixEntry.data = newSbix;

  entries.sort((a, b) => (a.tag < b.tag ? -1 : a.tag > b.tag ? 1 : 0));
  const headEntry = entries.find((e) => e.tag === "head");
  if (headEntry && headEntry.data.length >= 12) headEntry.data.writeUInt32BE(0, 8);

  const dirSize = 12 + entries.length * 16;
  let totalSize = dirSize;
  for (const e of entries) totalSize += (e.data.length + 3) & ~3;

  const out = Buffer.alloc(totalSize);
  const maxPow2 = 1 << Math.floor(Math.log2(entries.length));
  const searchRange = maxPow2 * 16;
  const entrySelector = Math.floor(Math.log2(entries.length));
  const rangeShift = entries.length * 16 - searchRange;
  out.writeUInt32BE(sfntVersion, 0);
  out.writeUInt16BE(entries.length, 4);
  out.writeUInt16BE(searchRange, 6);
  out.writeUInt16BE(entrySelector, 8);
  out.writeUInt16BE(rangeShift, 10);

  let cursor = dirSize;
  let headOffsetInOut = -1;
  entries.forEach((e, i) => {
    const rec = 12 + i * 16;
    out.write(e.tag, rec, 4, "latin1");
    out.writeUInt32BE(sfntChecksum(e.data), rec + 4);
    out.writeUInt32BE(cursor, rec + 8);
    out.writeUInt32BE(e.data.length, rec + 12);
    if (e.tag === "head") headOffsetInOut = cursor;
    e.data.copy(out, cursor);
    cursor += (e.data.length + 3) & ~3;
  });
  if (headOffsetInOut >= 0) {
    const adj = (0xb1b0afba - sfntChecksum(out)) >>> 0;
    out.writeUInt32BE(adj, headOffsetInOut + 8);
  }
  return out;
}

/** Extracts a single face (`faceIndex`, default 0) from a `.ttc` collection as a standalone SFNT `.ttf`/`.otf`. */
export function extractTtcFace(ttc, faceIndex = 0) {
  if (ttc.toString("latin1", 0, 4) !== "ttcf") {
    throw new Error("TTC does not start with ttcf");
  }
  const numFonts = ttc.readUInt32BE(8);
  if (faceIndex < 0 || faceIndex >= numFonts) {
    throw new Error(`TTC face ${faceIndex} out of range (0..${numFonts})`);
  }
  const faceOffset = ttc.readUInt32BE(12 + faceIndex * 4);
  const sfntVersion = ttc.readUInt32BE(faceOffset);
  const numTables = ttc.readUInt16BE(faceOffset + 4);
  const entries = [];
  for (let i = 0; i < numTables; i++) {
    const rec = faceOffset + 12 + i * 16;
    const tag = ttc.toString("latin1", rec, rec + 4);
    const offset = ttc.readUInt32BE(rec + 8);
    const length = ttc.readUInt32BE(rec + 12);
    entries.push({ tag, data: Buffer.from(ttc.subarray(offset, offset + length)) });
  }
  entries.sort((a, b) => (a.tag < b.tag ? -1 : a.tag > b.tag ? 1 : 0));
  const headEntry = entries.find((e) => e.tag === "head");
  if (headEntry && headEntry.data.length >= 12) headEntry.data.writeUInt32BE(0, 8);

  const dirSize = 12 + entries.length * 16;
  let totalSize = dirSize;
  for (const e of entries) totalSize += (e.data.length + 3) & ~3;

  const out = Buffer.alloc(totalSize);
  const maxPow2 = 1 << Math.floor(Math.log2(entries.length));
  const searchRange = maxPow2 * 16;
  const entrySelector = Math.floor(Math.log2(entries.length));
  const rangeShift = entries.length * 16 - searchRange;
  out.writeUInt32BE(sfntVersion, 0);
  out.writeUInt16BE(entries.length, 4);
  out.writeUInt16BE(searchRange, 6);
  out.writeUInt16BE(entrySelector, 8);
  out.writeUInt16BE(rangeShift, 10);

  let cursor = dirSize;
  let headOffsetInOut = -1;
  entries.forEach((e, i) => {
    const rec = 12 + i * 16;
    out.write(e.tag, rec, 4, "latin1");
    out.writeUInt32BE(sfntChecksum(e.data), rec + 4);
    out.writeUInt32BE(cursor, rec + 8);
    out.writeUInt32BE(e.data.length, rec + 12);
    if (e.tag === "head") headOffsetInOut = cursor;
    e.data.copy(out, cursor);
    cursor += (e.data.length + 3) & ~3;
  });
  if (headOffsetInOut >= 0) {
    const adj = (0xb1b0afba - sfntChecksum(out)) >>> 0;
    out.writeUInt32BE(adj, headOffsetInOut + 8);
  }
  return out;
}

function resolveSystemFontPath(rawPath) {
  if (existsSync(rawPath)) return rawPath;
  const nfd = rawPath.normalize("NFD");
  if (existsSync(nfd)) return nfd;
  const nfc = rawPath.normalize("NFC");
  if (existsSync(nfc)) return nfc;
  return null;
}

function parseIcons(html) {
  const icons = {};
  for (const m of html.matchAll(/<symbol id="(icon-[a-z0-9-]+)"([^>]*)>([\s\S]*?)<\/symbol>/g)) {
    icons[m[1]] = { viewBox: m[2].match(/viewBox="([^"]+)"/)?.[1] ?? "0 0 24 24", body: m[3] };
  }
  // Icons that are inlined at their point of use rather than in the sprite.
  for (const m of html.matchAll(/<svg[^>]*data-icon="(icon-[a-z0-9-]+)"[^>]*>([\s\S]*?)<\/svg>/g)) {
    icons[m[1]] ??= { viewBox: m[0].match(/viewBox="([^"]+)"/)?.[1] ?? "0 0 24 24", body: m[2] };
  }
  return icons;
}

async function main() {
  const check = process.argv.includes("--check");
  const html = await get(PROBE_URL, "text");
  const shell = fingerprintShell(html);
  if (!shell.stylesheet) throw new Error("could not find the stylesheet link; upstream markup changed");

  const prev = existsSync(LOCK) ? JSON.parse(await readFile(LOCK, "utf8")) : null;
  if (check) {
    const moved = !prev || prev.stylesheet.url !== shell.stylesheet;
    console.log(
      JSON.stringify({ appVersion: shell.appVersion, stylesheet: shell.stylesheet, moved }, null, 2),
    );
    process.exit(moved ? 3 : 0);
  }

  await mkdir(join(VENDOR, "fonts"), { recursive: true });
  await mkdir(join(VENDOR, "upstream"), { recursive: true });

  const css = await get(shell.stylesheet);
  await writeFile(join(VENDOR, "upstream", "styles.css"), css);
  await writeFile(join(VENDOR, "upstream", "probe.html"), html);

  // Design tokens, extracted from the stylesheet we just wrote. This throws
  // (rather than silently drifting) if a declaration the frame depends on
  // disappeared, so a sync that breaks the frame fails here.
  const tokens = await writeTokens({ vendor: VENDOR });

  const { decompress } = (await import("wawoff2")).default;
  const faces = parseFontFaces(css.toString("utf8"));
  const fonts = [];
  for (const face of faces) {
    const woff2 = await get(face.url);
    const ttf = Buffer.from(await decompress(woff2));
    const file = `${face.name}.${face.subset}.ttf`;
    await writeFile(join(VENDOR, "fonts", file), ttf);
    await writeFile(join(VENDOR, "fonts", file.replace(/\.ttf$/, ".woff2")), woff2);
    fonts.push({
      kind: "chirp",
      family: face.family,
      weight: face.weight,
      subset: face.subset,
      file,
      url: face.url,
      sha256: sha256(woff2),
      unicodeRange: face.unicodeRange,
      ranges: parseUnicodeRange(face.unicodeRange),
    });
  }

  // Openly licensed fonts for what Chirp does not cover. On the real site
  // these characters fall back to whatever the device has (SF, Roboto, Segoe,
  // and the system emoji font), so no single file is "right"; these are the
  // stand-ins, and the report flags text that needed them.
  for (const open of OPEN_FONTS) {
    let data;
    if (open.url.startsWith("system://")) {
      const vendored = join(VENDOR, "fonts", open.file);
      const [rawSysPath, frag = ""] = open.url.slice("system://".length).split("#");
      const sysPath = resolveSystemFontPath(decodeURIComponent(rawSysPath));
      if (sysPath) {
        const raw = await readFile(sysPath);
        if (frag.startsWith("ppem=")) {
          data = extractAppleColorEmoji(raw, Number(frag.slice(5)) || APPLE_EMOJI_PPEM);
        } else {
          const faceIdx = frag.startsWith("face=") ? Number(frag.slice(5)) || 0 : 0;
          data = extractTtcFace(raw, faceIdx);
        }
        await writeFile(vendored, data);
      } else if (existsSync(vendored)) {
        data = await readFile(vendored);
      } else {
        throw new Error(`missing ${rawSysPath} and ${vendored}; extract on macOS or restore assets/vendor/`);
      }
    } else if (open.url.includes("#delta=") || open.url.includes("#subset=")) {
      const vendored = join(VENDOR, "fonts", open.file);
      if (existsSync(vendored)) {
        data = await readFile(vendored);
      } else {
        throw new Error(`missing pre-subsetted ${vendored}; rebuild with hb-subset`);
      }
    } else {
      data = await get(open.url);
      await writeFile(join(VENDOR, "fonts", open.file), data);
    }
    const unicodeRange = open.unicodeRange ?? "U+0-10FFFF";
    fonts.push({ ...open, sha256: sha256(data), unicodeRange, ranges: parseUnicodeRange(unicodeRange) });
  }

  // Icons: the sprite on the probe page, plus any already-captured corpus pages.
  const icons = parseIcons(html);
  const corpusDir = join(VENDOR, "corpus");
  if (existsSync(corpusDir)) {
    const { readdir } = await import("node:fs/promises");
    for (const f of await readdir(corpusDir)) {
      if (!f.endsWith(".html")) continue;
      Object.assign(parseIcons(await readFile(join(corpusDir, f), "utf8")), icons);
      for (const [k, v] of Object.entries(parseIcons(await readFile(join(corpusDir, f), "utf8")))) {
        icons[k] ??= v;
      }
    }
  }
  await writeFile(join(VENDOR, "icons.json"), JSON.stringify(icons, null, 1));

  // X's own default profile image. x.com always draws an image in the avatar
  // slot — an account without one gets this — so the renderer falls back to it
  // instead of a placeholder. It is X's asset: vendored, pinned, not committed.
  const defaultAvatar = await get(DEFAULT_AVATAR_URL);
  await writeFile(join(VENDOR, "default-avatar.png"), defaultAvatar);

  // The manifest the Rust build reads. Order is CSS declaration order, which
  // decides precedence when unicode ranges overlap (later wins).
  await writeFile(join(VENDOR, "fonts", "manifest.json"), JSON.stringify(fonts, null, 1));

  const lock = {
    comment:
      "Pins the upstream snapshot xeet was built and verified against. Assets themselves are not committed.",
    fetchedAt: new Date().toISOString(),
    appVersion: shell.appVersion,
    stylesheet: { url: shell.stylesheet, sha256: sha256(css), bytes: css.length },
    fonts: fonts.map(({ ranges, ...f }) => f),
    icons: { count: Object.keys(icons).length, sha256: sha256(JSON.stringify(icons)) },
    defaultAvatar: {
      url: DEFAULT_AVATAR_URL,
      sha256: sha256(defaultAvatar),
      bytes: defaultAvatar.length,
    },
  };
  await writeFile(LOCK, JSON.stringify(lock, null, 2) + "\n");
  console.log(
    `synced app ${shell.appVersion?.slice(0, 8)}: stylesheet ${css.length}b, ${fonts.length} font files, ${Object.keys(icons).length} icons`,
  );
  if (prev && prev.stylesheet.sha256 !== lock.stylesheet.sha256) {
    console.log("stylesheet changed since the last lock: re-run the oracle and the parity suite");
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main().catch((err) => {
    console.error(err.message);
    process.exit(1);
  });
}
