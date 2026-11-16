// Profile preview for `/`: the profile photo and header image as x.com shows
// them on a profile page, without the "Edit profile" dialog's dark overlay and
// camera buttons in the way.
//
// Unlike the post previews this is HTML, not a WebAssembly PNG: the header is
// a handful of boxes around two images, and X builds it with a container query
// (`--profile-avatar-size: clamp(48px, 25cqw, 136px)`) that CSS can reproduce
// directly. The geometry, type and colors below were measured on x.com profile
// pages (web client 55591fe0, November 2026).
//
// Name, handle, badge, square avatar and the photo itself come from the post
// author above, so a new photo shows up in the posts and on the profile at once.

import icons from "../../assets/vendor/icons.json";
import { $, el } from "./util.js";

const WIDTHS = [
  { width: 600, label: "web · 600" },
  { width: 390, label: "web · 390" },
];

const BADGE_ICON = {
  blue: "icon-verified-premium-color",
  business: "icon-verified-business-color",
  government: "icon-verified-government-color",
};

const TABS = ["Posts", "Replies", "Reposts", "Media", "Articles"];

const MONTHS = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];

function icon(name, cls) {
  const def = icons[name];
  if (!def) return null;
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", def.viewBox);
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("fill", "currentColor");
  if (cls) svg.setAttribute("class", cls);
  svg.innerHTML = def.body;
  return svg;
}

/** X's profile-stats formatter: grouped below 10,000, then compact with one truncated decimal. */
export function formatStat(value) {
  const n = Math.max(0, Math.floor(Number(value) || 0));
  if (n < 10_000) return n.toLocaleString("en-US");
  const units = [
    [1e9, "B"],
    [1e6, "M"],
    [1e3, "K"],
  ];
  for (const [size, suffix] of units) {
    if (n >= size) {
      const whole = Math.floor((n / size) * 10) / 10;
      return `${whole % 1 === 0 ? whole.toFixed(0) : whole.toFixed(1)}${suffix}`;
    }
  }
  return String(n);
}

/** Bio text with @mentions, #hashtags and links in the brand color, like x.com. */
function bioNodes(text) {
  const out = [];
  const re =
    /(https?:\/\/\S+|\b[\w-]+\.(?:com|org|net|dev|io|ai|app|co)(?:\/\S*)?|[@#][\p{L}\p{N}_]+)/gu;
  let last = 0;
  for (const m of text.matchAll(re)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const token = m[0];
    const shown = token.replace(/^https?:\/\//, "").replace(/\/$/, "");
    out.push(el("span", { class: "xp-brand", text: shown }));
    last = m.index + token.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

/** How X's centered cover crop treats an image: what fraction is cut, and from where. */
function cropNote(size, ratio) {
  if (!size) return "";
  const actual = size.width / size.height;
  if (Math.abs(actual - ratio) / ratio < 0.01) return "";
  if (actual > ratio)
    return `${Math.round((1 - ratio / actual) * 100)}% cut from the sides`;
  return `${Math.round((1 - actual / ratio) * 100)}% cut from top and bottom`;
}

function imageSize(src) {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () =>
      resolve({ width: img.naturalWidth, height: img.naturalHeight });
    img.onerror = () => resolve(null);
    img.src = src;
  });
}

export function createProfilePreview() {
  const grid = $("#profileGrid");
  if (!grid) return { update() {} };
  const shell = $("#profileShell");
  const note = $("#profileNote");

  const state = {
    author: null,
    mode: "dark",
    banner: null, // { url, width, height }
    avatarSize: null,
  };
  let avatarKey = "";

  const field = (id) => $(id)?.value ?? "";

  function syncFade() {
    if (!shell) return;
    const maxScroll = grid.scrollWidth - grid.clientWidth;
    if (grid.scrollLeft > 2) shell.dataset.fadeLeft = "";
    else delete shell.dataset.fadeLeft;
    if (maxScroll - grid.scrollLeft > 2) shell.dataset.fadeRight = "";
    else delete shell.dataset.fadeRight;
  }
  grid.addEventListener("scroll", syncFade, { passive: true });
  if (typeof ResizeObserver !== "undefined")
    new ResizeObserver(syncFade).observe(grid);

  function avatarSrc() {
    const data = state.author?.avatar;
    if (!data) return null;
    // Post avatars are raw base64; the browser sniffs the real image type.
    return data.startsWith("data:") ||
      data.startsWith("blob:") ||
      data.startsWith("http")
      ? data
      : `data:image/png;base64,${data}`;
  }

  function card(width) {
    const author = state.author ?? {};
    const name = author.name || "Vercel";
    const handle = author.handle || "vercel";
    const badge = BADGE_ICON[author.verified];
    const avatar = avatarSrc();
    const posts = field("#prPosts");
    const following = field("#prFollowing");
    const followers = field("#prFollowers");
    const bio = field("#prBio").trim();
    const location = field("#prLocation").trim();
    const website = field("#prWebsite")
      .trim()
      .replace(/^https?:\/\//, "")
      .replace(/\/$/, "");
    const now = new Date();

    const nameLine = (cls, badgeCls) =>
      el("span", { class: cls }, [
        el("span", { class: "xp-name-text", text: name }),
        badge ? el("span", { class: badgeCls }, [icon(badge)]) : null,
      ]);

    const metaItem = (iconName, text, brand) =>
      el("span", { class: "xp-meta-item" }, [
        icon(iconName, "xp-meta-icon"),
        el("span", { class: brand ? "xp-brand" : "", text }),
      ]);

    const stat = (value, label) =>
      el("span", { class: "xp-stat" }, [
        el("b", { text: formatStat(value) }),
        " ",
        el("span", { text: label }),
      ]);

    return el(
      "div",
      { class: "xp", dataset: { mode: state.mode }, style: `width:${width}px` },
      [
        el("div", { class: "xp-top" }, [
          el("span", { class: "xp-icon-btn" }, [icon("icon-arrow-left")]),
          el("span", { class: "xp-top-title" }, [
            nameLine("xp-top-name", "xp-top-badge"),
            el("span", {
              class: "xp-top-sub",
              text: `${formatStat(posts)} posts`,
            }),
          ]),
          el("span", { class: "xp-icon-btn xp-icon-sm" }, [
            icon("icon-search"),
          ]),
          el("span", { class: "xp-icon-btn" }, [icon("icon-more-dots-hor")]),
        ]),
        el("div", { class: "xp-banner" }, [
          state.banner ? el("img", { src: state.banner.url, alt: "" }) : null,
        ]),
        el("div", { class: "xp-head" }, [
          el("div", { class: "xp-id" }, [
            el("div", { class: "xp-avatar-row" }, [
              el(
                "div",
                {
                  class: `xp-avatar${author.squareAvatar ? " xp-square" : ""}`,
                },
                [avatar ? el("img", { src: avatar, alt: "" }) : null],
              ),
              el("span", { class: "xp-edit", text: "Edit profile" }),
            ]),
            el("div", { class: "xp-names" }, [
              nameLine("xp-name", "xp-name-badge"),
              el("span", { class: "xp-handle", text: `@${handle}` }),
            ]),
          ]),
          bio ? el("div", { class: "xp-bio" }, bioNodes(bio)) : null,
          el("div", { class: "xp-meta" }, [
            location ? metaItem("icon-location-pin", location) : null,
            website ? metaItem("icon-link", website, true) : null,
            metaItem(
              "icon-calendar",
              `Joined ${MONTHS[now.getMonth()]} ${now.getFullYear()}`,
            ),
          ]),
          el("div", { class: "xp-stats" }, [
            stat(following, "Following"),
            stat(followers, "Followers"),
          ]),
        ]),
        el(
          "div",
          { class: "xp-tabs" },
          TABS.map((t, i) =>
            el("span", { class: i === 0 ? "xp-tab xp-tab-on" : "xp-tab" }, [
              el("span", { text: t }),
            ]),
          ),
        ),
      ],
    );
  }

  function render() {
    if (!state.author) return;
    const maxMobileW =
      window.innerWidth <= 640
        ? Math.max(260, window.innerWidth - 28)
        : Infinity;
    const figures = WIDTHS.map(({ width, label }) => {
      const avatarPx = Math.min(136, Math.max(48, (width - 32) * 0.25));
      const node = card(width);
      if (width > maxMobileW) node.style.zoom = String(maxMobileW / width);
      return el("figure", { class: "xp-figure" }, [
        node,
        el("figcaption", {}, [
          el("span", { class: "cap-surface", text: label }),
          el("span", {
            class: "cap-metrics",
            text: `photo ${+avatarPx.toFixed(1)} px · header ${width}×${Math.round(width / 3)}`,
          }),
        ]),
      ]);
    });
    grid.replaceChildren(...figures);
    syncFade();
    syncNote();
  }

  function syncNote() {
    if (!note) return;
    const parts = [];
    if (state.banner) {
      const crop = cropNote(state.banner, 3);
      parts.push(
        `Header ${state.banner.width}×${state.banner.height}${crop ? `: ${crop} (X wants 3:1, 1500×500)` : ", exactly 3:1"}.`,
      );
      if (state.banner.width < 1500)
        parts.push("X recommends at least 1500 px wide.");
    } else {
      parts.push("No header image yet: X shows a flat gray band.");
    }
    if (state.avatarSize) {
      const crop = cropNote(state.avatarSize, 1);
      parts.push(
        `Photo ${state.avatarSize.width}×${state.avatarSize.height}${crop ? `: ${crop}` : ""}.`,
      );
    }
    note.textContent = parts.join(" ");
  }

  const rerender = () => render();
  for (const id of [
    "#prBio",
    "#prLocation",
    "#prWebsite",
    "#prPosts",
    "#prFollowing",
    "#prFollowers",
  ]) {
    $(id)?.addEventListener("input", rerender);
  }

  $("#prBanner")?.addEventListener("change", async (ev) => {
    const file = ev.target.files?.[0];
    if (!file) return;
    const url = URL.createObjectURL(file);
    const size = await imageSize(url);
    if (!size) {
      URL.revokeObjectURL(url);
      if (note)
        note.textContent =
          "That header file isn't an image this browser can read.";
      return;
    }
    if (state.banner) URL.revokeObjectURL(state.banner.url);
    state.banner = { url, ...size };
    render();
  });

  // The profile photo is the post author's avatar: hand the file to the
  // author pane's own input so the posts above pick it up too.
  $("#prAvatar")?.addEventListener("change", (ev) => {
    const target = $("#aAvatarFile");
    const files = ev.target.files;
    if (!target || !files?.length) return;
    const dt = new DataTransfer();
    dt.items.add(files[0]);
    target.files = dt.files;
    target.dispatchEvent(new Event("change", { bubbles: true }));
  });

  $("#prClear")?.addEventListener("click", () => {
    if (state.banner) URL.revokeObjectURL(state.banner.url);
    state.banner = null;
    for (const id of ["#prBanner", "#prAvatar"]) {
      const input = $(id);
      if (input) input.value = "";
    }
    render();
  });

  return {
    /** Called with each freshly built post, so the profile follows the author pane. */
    async update(author, mode) {
      state.author = author ?? null;
      state.mode = mode;
      const key = author?.avatar
        ? `${author.avatar.length}:${author.avatar.slice(-32)}`
        : "";
      if (key !== avatarKey) {
        avatarKey = key;
        state.avatarSize = null;
        const src = avatarSrc();
        if (src) {
          const size = await imageSize(src);
          if (avatarKey === key) state.avatarSize = size;
        }
      }
      render();
    },
  };
}
