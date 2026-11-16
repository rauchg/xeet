import { ASSET_URL_MAP } from "./asset-urls.js";
import { resolveInput, resolveLinks } from "./api.js";
import { createComposer, ensureAuthorAndAvatars } from "./composer.js";
import { HARD } from "./hard.js";
import { createProfilePreview } from "./profile.js";
import { highlightAll } from "./highlight.js";
import { cleanPost, isStatusUrl, urlsInText } from "./post.js";
import { $, bytesToBase64, debounce, el, errorMessage, formatMs, pngSize } from "./util.js";
import { onLoadStage, onWasmSized, preloadWorker, renderSuiteInWorker, wasmReady } from "./wasm.js";

let booted = false;
export function bootPlayground() {
  if (booted || typeof document === 'undefined') return;
  booted = true;
  // One post → four PNGs, painted together, in the visitor's system theme.
  //
  // Anything the post needs (an x.com link, a card photo) is loaded first; then
  // every layout is rendered in WebAssembly and the grid is swapped in one go, so
  // the times shown are paint times and nothing else.


  // The resolver endpoint checks BotID on every request; this makes the browser
  // attach the headers it checks. It has to run before the first fetch.
  // BotID initialized via <BotIdClient> in app/layout.jsx.

  const BASE_LAYOUTS = [
    { surface: "web-timeline", width: 390 },
    { surface: "web-detail", width: 390 },
    { surface: "web-timeline", width: 600 },
    // iPhone Duo closed outer display: 466 pt screen minus the 84 pt right
    // system strip = 382 pt at @3x (1,146 px wide), rendered with X's native iOS
    // timeline surface (47.5pt avatar, 15.5/20.67pt type, 9-line clamp).
    { surface: "ios-timeline", width: 382, dpr: 3, label: "duo · 382", slug: "duo-382", comment: " // iPhone Duo (iOS)" },
  ];
  const DPR = 2;
  const SURFACE_LABEL = {
    "web-timeline": "timeline",
    "web-detail": "detail",
    "ios-timeline": "ios",
    "ios-detail": "ios detail",
  };

  const input = $("#input");
  const grid = $("#grid");
  const gridShell = $("#gridShell");
  const status = $("#status");
  const loadBar = $("#loadBar");
  const loadFill = $("#loadFill");
  let hasPaintedOnce = false;

  const PACK_NAMES = {
    emoji: "Apple Color Emoji",
    cjk: "Hiragino CJK",
    math: "math & symbols",
    latin: "Latin",
    arabic: "Arabic",
    hebrew: "Hebrew",
  };

  function describePacks(packs = []) {
    const names = packs.map((p) => PACK_NAMES[p] ?? p);
    if (!names.length) return "font pack";
    if (names.length === 1) return `${names[0]} font pack`;
    if (names.length === 2) return `${names[0]} + ${names[1]} font packs`;
    return `${names[0]} + ${names.length - 1} font packs`;
  }

  function mbPair(loaded, total) {
    if (!total) return `${(loaded / 1e6).toFixed(1)} MB`;
    return `${(loaded / 1e6).toFixed(1)} / ${(total / 1e6).toFixed(1)} MB`;
  }

  function setProgressBar(pct) {
    if (!loadBar || !loadFill) return;
    if (pct === null) {
      delete loadBar.dataset.active;
      return;
    }
    loadBar.dataset.active = "";
    loadFill.style.width = `${Math.max(2, Math.min(100, Math.round(pct)))}%`;
  }

  function syncGridFade() {
    if (!gridShell) return;
    const maxScroll = grid.scrollWidth - grid.clientWidth;
    if (grid.scrollLeft > 2) gridShell.dataset.fadeLeft = "";
    else delete gridShell.dataset.fadeLeft;
    if (maxScroll - grid.scrollLeft > 2) gridShell.dataset.fadeRight = "";
    else delete gridShell.dataset.fadeRight;
  }
  grid.addEventListener("scroll", syncGridFade, { passive: true });
  if (typeof ResizeObserver !== "undefined") new ResizeObserver(syncGridFade).observe(grid);
  const scheme = matchMedia("(prefers-color-scheme: dark)");
  const root = document.documentElement;
  /** A saved choice wins; otherwise the system decides. */
  const theme = () => root.dataset.theme || (scheme.matches ? "dark" : "light");
  const toggle = $("#themeToggle");

  function syncToggle() {
    const next = theme() === "dark" ? "light" : "dark";
    toggle.textContent = next;
    toggle.setAttribute("aria-label", `Switch to ${next} mode`);
  }

  toggle.addEventListener("click", () => {
    const next = theme() === "dark" ? "light" : "dark";
    // Picking what the system already says clears the override.
    if (next === (scheme.matches ? "dark" : "light")) {
      delete root.dataset.theme;
      try { localStorage.removeItem("xeet.theme"); } catch {}
    } else {
      root.dataset.theme = next;
      try { localStorage.setItem("xeet.theme", next); } catch {}
    }
    syncToggle();
    refresh();
  });
  syncToggle();

  const cache = new Map();
  let index = 0;
  let seq = 0;
  let urls = [];

  // ---------------------------------------------------------------- cycling

  const cycle = $(".cycle");

  const MAX_INPUT_HEIGHT = 360;
  const MIN_INPUT_HEIGHT = 51;

  function minInputHeight() {
    return parseFloat(getComputedStyle(input).minHeight) || MIN_INPUT_HEIGHT;
  }

  /** Auto-sizes `#input` to its text content (1 line for URLs, growing up to 360 px). */
  function fitInput() {
    const isSingleUrl = /^https?:\/\/\S+$/.test(input.value.trim());
    if (isSingleUrl) input.dataset.url = "1";
    else delete input.dataset.url;
    const floor = minInputHeight();
    input.style.height = "0px";
    const needed = isSingleUrl ? floor : input.scrollHeight + 2;
    const clamped = Math.min(MAX_INPUT_HEIGHT, Math.max(floor, needed));
    input.style.height = `${clamped}px`;
    input.style.overflowY = needed > MAX_INPUT_HEIGHT ? "auto" : "hidden";
  }
  let lastInputWidth = 0;
  new ResizeObserver((entries) => {
    const w = entries[0]?.contentRect.width ?? 0;
    if (!w) return;
    if (lastInputWidth === 0) {
      lastInputWidth = w;
      return;
    }
    if (w !== lastInputWidth) {
      lastInputWidth = w;
      fitInput();
    }
  }).observe(input);

  function show(i, { focusCarousel = false } = {}) {
    index = (i + HARD.length) % HARD.length;
    const item = HARD[index];
    composer.resetForExample();
    $("#count").textContent = `${index + 1} / ${HARD.length}`;
    $("#title").textContent = item.title;
    $("#why").textContent = item.why;
    input.value = item.url;
    fitInput();
    input.scrollTop = 0;
    grid.scrollLeft = 0;
    syncGridFade();
    composer.syncToolbar(item.post);
    if (focusCarousel) {
      const pinTop = window.innerWidth <= 640 ? 8 : 20;
      if (Math.abs(cycle.getBoundingClientRect().top - pinTop) > 4) {
        const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
        cycle.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "start" });
      }
    }
    refresh();
  }

  $("#prev").addEventListener("click", () => show(index - 1, { focusCarousel: true }));
  $("#next").addEventListener("click", () => show(index + 1, { focusCarousel: true }));
  document.addEventListener("keydown", (event) => {
    const tag = event.target?.tagName;
    if (tag === "TEXTAREA" || tag === "INPUT" || tag === "SELECT" || event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.key === "ArrowLeft") show(index - 1, { focusCarousel: true });
    if (event.key === "ArrowRight") show(index + 1, { focusCarousel: true });
  });
  scheme.addEventListener("change", () => {
    syncToggle();
    if (!root.dataset.theme) refresh();
  });

  const schedule = debounce(() => refresh(), 120);
  input.addEventListener("input", () => {
    fitInput();
    composer.markUserTyped();
    schedule();
  });

  // ------------------------------------------------------------- resolution

  async function cached(key, load) {
    if (cache.has(key)) return cache.get(key);
    try {
      const value = await load();
      cache.set(key, value);
      return value;
    } catch {
      cache.set(key, null);
      return null;
    }
  }

  /** An image served with the site, as the base64 the renderer takes. */
  function asset(path) {
    return cached(`asset:${path}`, async () => {
      const res = await fetch(ASSET_URL_MAP[path] ?? path);
      if (!res.ok) throw new Error(`${path}: ${res.status}`);
      return bytesToBase64(new Uint8Array(await res.arrayBuffer()));
    });
  }

  /** Profile pictures served with the site, by handle. */
  const AVATARS = {
    NASAGoddard: "./avatars/NASAGoddard.jpg",
    pizzahut: "./avatars/pizzahut.jpg",
    MerriamWebster: "./avatars/MerriamWebster.png",
    cursor_ai: "./avatars/cursor_ai.jpg",
    rauchg: "./avatars/rauchg.jpg",
    mihar_22: "./avatars/mihar_22.jpg",
    bscholl: "./avatars/bscholl.jpg",
    kyunbit: "./avatars/kyunbit.jpg",
    anabology: "./avatars/anabology.jpg",
    vineettiruvadi: "./avatars/vineettiruvadi.jpg",
    zackdfilms1: "./avatars/zackdfilms1.jpg",
    adamfuhrer: "./avatars/adamfuhrer.jpg",
    justinbieber: "./avatars/justinbieber.jpg",
    ciguleva: "./avatars/ciguleva.jpg",
    espn: "./avatars/espn.jpg",
    NFL: "./avatars/NFL.jpg",
    karpathy: "./avatars/karpathy.jpg",
    mattpocockuk: "./avatars/mattpocockuk.jpg",
    jack: "./avatars/jack.jpg",
  };

  async function lookupStaticAvatar(handle) {
    const exact = AVATARS[handle] ?? Object.entries(AVATARS).find(([k]) => k.toLowerCase() === String(handle).toLowerCase())?.[1];
    return exact ? asset(exact) : null;
  }

  const composer = createComposer({
    input,
    getExample: () => HARD[index],
    resolveCached: (key) => {
      const url = key.startsWith("input:") ? key.slice(6) : key;
      return cached(`input:${url}`, () => resolveInput(url, { media: true, dpr: 3 }));
    },
    lookupStaticAvatar,
    onChange: () => refresh(),
  });
  const profile = createProfilePreview();

  /** `/resolve-links` returns `{meta, image}` per URL; the renderer wants LinkMeta. */
  function linkMeta(entry) {
    if (!entry || typeof entry !== "object" || entry.error) return null;
    const meta = { ...(entry.meta ?? entry) };
    if (meta.image && entry.image?.data && !meta.image.data) meta.image = { ...meta.image, data: entry.image.data };
    return meta;
  }

  /** Builds the example's base Post before composer overrides are layered on top. */
  async function buildExampleBase(text) {
    const item = HARD[index];
    const post = await ensureAuthorAndAvatars({ ...item.post, text, links: { ...item.post.links } }, lookupStaticAvatar);
    if (item.assets?.affiliateBadge && post.author?.affiliate && !post.author.affiliate.badge) {
      const badge = await asset(item.assets.affiliateBadge);
      if (badge) post.author = { ...post.author, affiliate: { ...post.author.affiliate, badge } };
    }
    if (item.assets?.quoteAffiliateBadge && post.quote?.author?.affiliate && !post.quote.author.affiliate.badge) {
      const badge = await asset(item.assets.quoteAffiliateBadge);
      if (badge) {
        post.quote = {
          ...post.quote,
          author: { ...post.quote.author, affiliate: { ...post.quote.author.affiliate, badge } },
        };
      }
    }
    if (item.assets?.sourceAvatar && post.media?.[0]?.sourceAuthor && !post.media[0].sourceAuthor.avatar) {
      const srcAva = await asset(item.assets.sourceAvatar);
      if (srcAva) {
        post.media = post.media.map((m, i) =>
          i === 0 ? { ...m, sourceAuthor: { ...m.sourceAuthor, avatar: srcAva } } : m,
        );
      }
    }
    for (const [url, path] of Object.entries(item.assets?.cards ?? {})) {
      const data = await asset(path);
      if (data && post.links[url]?.image) post.links[url] = { ...post.links[url], image: { ...post.links[url].image, data } };
    }
    if (item.assets?.media?.length && post.media?.length) {
      post.media = await Promise.all(
        post.media.map(async (m, i) => {
          const path = item.assets.media[i];
          const data = path ? await asset(path) : null;
          return data ? { ...m, data } : m;
        }),
      );
    }
    if (item.assets?.quoteMedia?.length && post.quote?.media?.length) {
      const media = await Promise.all(
        post.quote.media.map(async (m, i) => {
          const path = item.assets.quoteMedia[i];
          const data = path ? await asset(path) : null;
          return data ? { ...m, data } : m;
        }),
      );
      post.quote = { ...post.quote, media };
    }
    return post;
  }

  /** Resolved external link metadata (`url -> meta | null`) + in-flight deduping,
   *  fetched in the background so typing a URL never blocks painting the draft. */
  const linkMetaCache = new Map();
  const linkMetaPending = new Set();

  function fetchMissingLinksInBackground(urls) {
    const todo = urls.filter((u) => {
      if (linkMetaCache.has(u) || linkMetaPending.has(u)) return false;
      try {
        const parsed = new URL(u);
        return parsed.hostname.includes(".");
      } catch {
        return false;
      }
    }).slice(0, 4);
    if (!todo.length) return;
    for (const u of todo) linkMetaPending.add(u);
    resolveLinks(todo, { media: true })
      .then((res) => {
        let anyUseful = false;
        for (const u of todo) {
          const meta = linkMeta(res?.links?.[u]);
          linkMetaCache.set(u, meta);
          if (meta && (meta.card || meta.display)) anyUseful = true;
        }
        if (anyUseful && todo.some((u) => input.value.includes(u))) refresh();
      })
      .catch(() => {
        for (const u of todo) linkMetaCache.set(u, null);
      })
      .finally(() => {
        for (const u of todo) linkMetaPending.delete(u);
      });
  }

  /** Everything this draft needs, loaded before any paint. */
  async function buildPost(text) {
    const post = await composer.buildComposedPost(text, buildExampleBase);
    const extUrls = urlsInText(post.text).filter((url) => !isStatusUrl(url));
    if (extUrls.length) {
      const links = { ...(post.links ?? {}) };
      const missing = [];
      for (const url of extUrls) {
        if (links[url]) continue;
        if (linkMetaCache.has(url)) {
          const cachedMeta = linkMetaCache.get(url);
          if (cachedMeta) links[url] = cachedMeta;
        } else {
          missing.push(url);
        }
      }
      if (Object.keys(links).length) post.links = links;
      if (missing.length) fetchMissingLinksInBackground(missing);
    }
    return cleanPost(post);
  }

  // ------------------------------------------------------------------ paint

  /** Space under each image for its caption, matching `figcaption` in style.css. */
  const CAPTION = 40;
  /** `.grid` vertical padding in style.css (`8px` top + `10px` bottom halo clearance). */
  const FRAME = 18;

  function figure(layout, mode, result, next) {
    const dpr = layout.dpr ?? DPR;
    const size = pngSize(result.png);
    const kind = SURFACE_LABEL[layout.surface];
    const caption = layout.label ?? `${kind} · ${layout.width}${layout.expanded ? " · expanded" : ""}`;
    const slug = layout.slug ?? `${kind}-${layout.width}${layout.expanded ? "-expanded" : ""}`;
    const itemId = composer.isCustomDraft() ? "draft" : HARD[index].id;
    const name = `xeet-${itemId}-${slug}-${mode}@${dpr}x.png`;
    const href = URL.createObjectURL(new Blob([result.png], { type: "image/png" }));
    next.push(href);
    const rawW = size ? size.width / dpr : layout.width;
    const rawH = size ? size.height / dpr : 0;
    // On narrow phone viewports, fit wide cards inside the screen width cleanly
    // while keeping desktop cards at exact 1:1 CSS pixels.
    const maxMobileW = window.innerWidth <= 640 ? Math.max(260, window.innerWidth - 28) : Infinity;
    const scale = rawW > maxMobileW ? maxMobileW / rawW : 1;
    const width = Math.round(rawW * scale);
    const height = Math.round(rawH * scale);
    const img = el("img", { src: href, alt: `${caption} layout, ${layout.width} px`, width, height, decoding: "async" });
    const node = el("figure", {}, [
      img,
      el("figcaption", {}, [
        el("span", { class: "cap-surface", text: caption }),
        el("span", { class: "cap-metrics", text: `${formatMs(result.paint)}${size ? ` · ${size.width}×${size.height}` : ""}` }),
        el("a", { class: "cap-dl", href, download: name, text: "download" }),
      ]),
    ]);
    return { node, img, height };
  }

  async function refresh() {
    const token = ++seq;
    const text = input.value;
    if (!text.trim() && !composer.hasActiveAttachment()) {
      status.textContent = "type something";
      composer.syncToolbar(null, null);
      return;
    }

    // The post's data is the only thing this thread waits on: the renderer was
    // started on its worker when this module loaded. The status says which one is
    // being waited on: the one-time module download, or the network for this post.
    let waitingOnPost = true;
    let stopStage = () => {};
    const coldBoot = !hasPaintedOnce && !wasmReady();
    const applyStage = (ev) => {
      if (token !== seq || !ev) return;
      if (ev.stage === "wasm-download") {
        const pct = ev.fraction == null ? null : Math.min(99, Math.floor(ev.fraction * 100));
        const detail = ev.totalBytes ? ` — ${mbPair(ev.loadedBytes, ev.totalBytes)}${pct != null ? ` (${pct}%)` : ""}` : "";
        status.textContent = `Downloading WebAssembly core${detail}`;
        setProgressBar(ev.fraction == null ? 10 : ev.fraction * 82);
      } else if (ev.stage === "wasm-compile") {
        status.textContent = "Compiling WebAssembly core…";
        setProgressBar(88);
      } else if (ev.stage === "font-download") {
        const pct = ev.fraction == null ? null : Math.min(99, Math.floor(ev.fraction * 100));
        const label = describePacks(ev.packs);
        status.textContent = `Downloading ${label} — ${mbPair(ev.loadedBytes, ev.totalBytes)}${pct != null ? ` (${pct}%)` : ""}`;
        setProgressBar(8 + (ev.fraction ?? 0.2) * 82);
      } else if (ev.stage === "font-register") {
        status.textContent = `Verifying & registering ${describePacks(ev.packs)}…`;
        setProgressBar(94);
      } else if (ev.stage === "paint") {
        if (!hasPaintedOnce || loadBar?.hasAttribute("data-active")) {
          status.textContent = `Painting ${ev.count ?? 4} layouts in WebAssembly…`;
          setProgressBar(98);
        }
      }
    };

    if (coldBoot) {
      setProgressBar(3);
      stopStage = onLoadStage(applyStage);
    }
    const pending = setTimeout(() => {
      if (token !== seq) return;
      if (waitingOnPost) {
        status.textContent = "Fetching post metadata & media from X…";
        setProgressBar(20);
      } else if (!coldBoot) {
        stopStage = onLoadStage(applyStage);
      }
    }, 140);

    let post;
    let suite;
    const mode = theme();
    const activeVariants = composer.getVariants();
    const autoExpand = composer.getAutoExpand();
    const fontScale = composer.getFontScale();
    try {
      post = await buildPost(text).finally(() => (waitingOnPost = false));
      if (token !== seq) return;
      profile.update(post.author, mode);
      suite = await renderSuiteInWorker(post, mode, activeVariants, DPR, { autoExpand, fontScale });
    } catch (error) {
      if (token === seq) {
        setProgressBar(null);
        status.textContent = errorMessage(error);
      }
      return;
    } finally {
      clearTimeout(pending);
      stopStage();
    }
    // A superseded render comes back as `{ cancelled: true }`: the input moved on,
    // and `token !== seq` says whether this call is the one that gave it up.
    if (suite?.cancelled || token !== seq) return;
    const results = suite.results;

    // Build every figure off screen and wait until all of them have decoded, so
    // the row changes once: new images in, and one transition to the height of
    // the tallest. Nothing pops in piecemeal and nothing below jumps twice.
    const next = [];
    const figures = results.map((result) => figure(result.layout, mode, result, next));
    await Promise.all(figures.map(({ img }) => img.decode().catch(() => {})));
    if (token !== seq) {
      for (const url of next) URL.revokeObjectURL(url);
      return;
    }

    const tallest = Math.max(...figures.map((f) => f.height));
    grid.style.height = `${Math.ceil(tallest + CAPTION + FRAME)}px`;
    settleHash();
    grid.replaceChildren(...figures.map((f) => f.node));
    grid.classList.add("ready");
    hasPaintedOnce = true;
    setProgressBar(null);
    syncGridFade();
    for (const url of urls) URL.revokeObjectURL(url);
    urls = next;

    // Render time only: each number is the `render()` call alone, after every
    // byte the post needs has arrived. Fetching, decoding and layout of the page
    // are not in it.
    const paints = results.map((result) => result.paint);
    const lo = Math.min(...paints);
    const hi = Math.max(...paints);
    const range = formatMs(lo) === formatMs(hi) ? formatMs(lo) : `${formatMs(lo).replace(" ms", "")}–${formatMs(hi)}`;
    const noun = results.length === 1 ? "PNG" : "PNGs";
    status.textContent =
      window.innerWidth <= 640
        ? `${results.length} ${noun} · ${range} in browser`
        : `${results.length} ${noun}, ${range} each to render, in your browser`;
    composer.syncToolbar(post, results[0]?.report ?? null);
    shown = { post, mode, expandable: suite.expandable, layouts: results.map((r) => r.layout), fontScale };
    if (!codePane.hidden) showCode();
  }

  // ------------------------------------------------------------- view code

  const codePane = $("#codePane");
  const viewCode = $("#viewCode");
  let shown = null;

  /** Long base64 strings (avatars, images) become a short, still-valid label. */
  function shortenBytes(value) {
    if (typeof value === "string" && value.length > 120 && /^[A-Za-z0-9+/=]+$/.test(value)) {
      return `<${(value.length * 0.75 / 1024).toFixed(1)} KB of base64>`;
    }
    if (Array.isArray(value)) return value.map(shortenBytes);
    if (value && typeof value === "object") {
      return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, shortenBytes(v)]));
    }
    return value;
  }

  /** JSON with identifier keys unquoted, so it reads as a JavaScript literal. */
  const literal = (value) => JSON.stringify(value, null, 2).replace(/^(\s*)"([A-Za-z_$][\w$]*)":/gm, "$1$2:");

  function showCode() {
    if (!shown) return;
    const { post, mode, layouts, fontScale } = shown;
    const callLine = (l) => {
      const scale = l.fontScale ?? fontScale ?? 1;
      const scalePart = scale !== 1 ? `, fontScale: ${Number(scale.toFixed(2))}` : "";
      const expPart = l.expanded ? ", expanded: true" : "";
      return `xeet.render(post, { surface: "${l.surface}", width: ${l.width}, theme: "${mode}", dpr: ${l.dpr ?? DPR}${scalePart}${expPart} });${l.comment ?? ""}`;
    };
    const calls = (layouts ?? BASE_LAYOUTS).map((l) => callLine(l));
    const code = [
      `import { loadXeet, type Post } from "@xeet/core/node";`,
      ``,
      `const xeet = await loadXeet();`,
      ``,
      `// The post exactly as the previews received it.`,
      `const post: Post = ${literal(shortenBytes(post))};`,
      ``,
      `// One call per preview; each returns { png, report }.`,
      ...calls,
    ].join("\n");
    const out = $("#codeOut");
    out.textContent = code;
    highlightAll(codePane);
  }

  viewCode.addEventListener("click", () => {
    const open = codePane.hidden;
    codePane.hidden = !open;
    viewCode.setAttribute("aria-expanded", String(open));
    viewCode.textContent = open ? "hide code" : "view code";
    if (open) {
      showCode();
      const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
      codePane.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "start" });
    }
  });

  // ------------------------------------------------------------------ tabs

  const tabs = [...document.querySelectorAll('[role="tab"]')];
  function selectTab(tab) {
    for (const t of tabs) {
      const on = t === tab;
      t.setAttribute("aria-selected", String(on));
      t.tabIndex = on ? 0 : -1;
      $(`#${t.getAttribute("aria-controls")}`).hidden = !on;
    }
  }
  for (const tab of tabs) {
    tab.addEventListener("click", () => selectTab(tab));
    tab.addEventListener("keydown", (event) => {
      const step = { ArrowRight: 1, ArrowLeft: -1 }[event.key];
      if (!step) return;
      event.preventDefault();
      const next = tabs[(tabs.indexOf(tab) + step + tabs.length) % tabs.length];
      selectTab(next);
      next.focus();
    });
  }

  // ------------------------------------------------------------- permalinks
  //
  // Every heading and API entry has an id (#render, #options-width,
  // #rust-render-a-post). A link into a tab that is not showing opens that tab
  // first, so the target is there to scroll to.

  function revealHash() {
    const id = decodeURIComponent(location.hash.slice(1));
    const target = id && document.getElementById(id);
    if (!target) return;
    const panel = target.closest('[role="tabpanel"]');
    if (panel?.hidden) selectTab(tabs.find((t) => t.getAttribute("aria-controls") === panel.id));
    target.scrollIntoView({ block: "start" });
  }

  // The preview row starts at a placeholder height and settles to the real one
  // after the first render, which moves everything below it. If the visitor
  // arrived on a permalink and has not scrolled since, land on it again once.
  let userMoved = false;
  for (const type of ["wheel", "touchmove", "keydown", "mousedown"]) {
    addEventListener(type, () => (userMoved = true), { once: true, passive: true });
  }
  let hashSettled = false;
  function settleHash() {
    if (hashSettled) return;
    hashSettled = true;
    if (!location.hash) return;
    let done = false;
    const again = () => {
      if (done) return;
      done = true;
      if (!userMoved) revealHash();
    };
    grid.addEventListener("transitionend", again, { once: true });
    setTimeout(again, 700);
  }

  addEventListener("hashchange", () => revealHash());
  // On load: open a hidden tab if the link points inside it, then scroll there.
  revealHash();

  // ------------------------------------------------------------------- boot

  function reportWasmSize(info) {
    const el = $("#wasmSize");
    if (!el) return;
    const entry = performance.getEntriesByType("resource").find((e) => e.name.endsWith("xeet_wasm_bg.wasm"));
    const encoded = info?.encodedBytes || entry?.encodedBodySize || 0;
    const decoded = info?.decodedBytes || entry?.decodedBodySize || 0;
    if (!decoded) return;
    const mb = (n) => `${(n / 1e6).toFixed(1)} MB`;
    const compressed = encoded && encoded < decoded;
    el.textContent = compressed ? `${mb(encoded)} to download, compressed` : `${mb(decoded)}`;
  }

  // The renderer downloads and compiles on a worker thread from the moment this
  // module runs: the first keystroke should never be the thing that starts a
  // core module. Nothing here waits for it.
  preloadWorker();
  onWasmSized(reportWasmSize);
  highlightAll();
  show(0);

}
