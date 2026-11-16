#!/usr/bin/env node
// Builds the lazy font packs the WebAssembly loader fetches on demand.
//
// The module itself embeds only `core` (Chirp + Arial). Everything else lives
// in one blob per script family:
//
//   emoji   Apple Color Emoji                         (~21 MB)
//   cjk     Hiragino Sans regular + bold              (~15 MB)
//   math    STIX Two Math, Noto Math/Symbols/Symbols 2 (~2.3 MB)
//   latin   Noto Sans regular + bold                   (~0.9 MB)
//   arabic  Noto Sans Arabic regular + bold            (~0.3 MB)
//   hebrew  Noto Sans Hebrew regular + bold            (~0.03 MB)
//
// Each pack is a plain concatenation of the font files plus `packs.json`,
// which carries the offset and SHA-256 of every file and the SHA-256 of the
// whole blob. The loader verifies the blob before registering its files; the
// web build serves the directory from a content-hashed URL with immutable
// caching, so a checksum mismatch means real corruption, not a stale deploy.
//
// The pack assignment must match `pack_for` in `crates/xeet-core/build.rs`.

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const MANIFEST = join(ROOT, "assets", "vendor", "fonts", "manifest.json");
const FONTS = join(ROOT, "assets", "vendor", "fonts");
const DEFAULT_OUT = join(ROOT, "packages", "xeet", "wasm", "fonts");

/** Same groups as `pack_for` in `crates/xeet-core/build.rs`. */
function packFor(family) {
  switch (family) {
    case "TwitterChirp":
    case "Arial":
      return "core";
    case "Apple Color Emoji":
      return "emoji";
    case "Hiragino Sans":
    case "Noto Sans SC":
      return "cjk";
    case "Noto Sans KR":
      return "hangul";
    case "Noto Sans Arabic":
      return "arabic";
    case "Noto Sans Hebrew":
      return "hebrew";
    case "Noto Sans":
      return "latin";
    default:
      return "math";
  }
}

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/**
 * Writes `packs.json` and one `<pack>.bin` per non-core pack.
 *
 * @param {string} [outDir] defaults to `packages/xeet/wasm/fonts`
 * @returns {{ packs: Record<string, { bytes: number, sha256: string, files: number }>, totalBytes: number }}
 */
export function buildFontPacks(outDir = DEFAULT_OUT) {
  if (!existsSync(MANIFEST)) {
    throw new Error(`xeet: ${MANIFEST} is missing; run node tools/sync-upstream.mjs first`);
  }
  const manifest = JSON.parse(readFileSync(MANIFEST, "utf8"));
  const groups = new Map();
  for (const face of manifest) {
    const pack = packFor(face.family);
    if (pack === "core") continue;
    if (!groups.has(pack)) groups.set(pack, []);
    groups.get(pack).push(face);
  }

  mkdirSync(outDir, { recursive: true });
  // A pack that disappears must not leave a stale blob behind: the loader
  // enumerates `packs.json`, but an old file would still be served and could
  // mask a packaging mistake.
  for (const name of readdirSync(outDir)) {
    if (name.endsWith(".bin")) rmSync(join(outDir, name), { force: true });
  }

  const packs = {};
  let totalBytes = 0;
  for (const [name, faces] of groups) {
    const chunks = [];
    const files = [];
    let offset = 0;
    for (const face of faces) {
      const bytes = readFileSync(join(FONTS, face.file));
      files.push({
        file: face.file,
        offset,
        length: bytes.length,
        sha256: sha256(bytes),
      });
      chunks.push(bytes);
      offset += bytes.length;
    }
    const blob = Buffer.concat(chunks, offset);
    writeFileSync(join(outDir, `${name}.bin`), blob);
    packs[name] = {
      bytes: blob.length,
      sha256: sha256(blob),
      files,
    };
    totalBytes += blob.length;
  }

  const document = {
    version: 1,
    generatedBy: "tools/ship/build-font-packs.mjs",
    packs,
  };
  writeFileSync(join(outDir, "packs.json"), `${JSON.stringify(document, null, 2)}\n`);

  const summary = Object.fromEntries(
    Object.entries(packs).map(([name, pack]) => [
      name,
      { bytes: pack.bytes, sha256: pack.sha256, files: pack.files.length },
    ]),
  );
  return { packs: summary, totalBytes };
}

const invokedDirectly =
  process.argv[1] && resolve(fileURLToPath(import.meta.url)) === resolve(process.argv[1]);
if (invokedDirectly) {
  const result = buildFontPacks(process.argv[2] ? resolve(process.argv[2]) : undefined);
  for (const [name, pack] of Object.entries(result.packs)) {
    console.log(
      `fonts: ${name.padEnd(6)} ${(pack.bytes / 1e6).toFixed(2).padStart(6)} MB  ${pack.sha256.slice(0, 12)}  (${pack.files} files)`,
    );
  }
  console.log(`fonts: ${(result.totalBytes / 1e6).toFixed(2)} MB in lazy packs`);
  const manifestSize = statSync(join(DEFAULT_OUT, "packs.json")).size;
  console.log(`fonts: packs.json ${(manifestSize / 1e3).toFixed(1)} kB`);
}
