# Link cards: resolving a URL the way X's crawler does

A post with a link gets a card because X's crawler fetched the page, followed
whatever redirects it took, and read its `twitter:*`/`og:*` tags. xeet's renderer
never fetches anything — link metadata is an input — so the card has to be
resolved beforehand and handed in as `post.links`. The crawler is
`crates/xeet-resolve` (`resolve_link`), reached from JS as `xeet.resolveLinks`
(`packages/xeet/src/resolve.js`); the Node host that supplies the network lives
in `tools/playground/lib/resolve-lib.mjs`. The playground calls it from
`/api/resolve`, and the web demo's one deployed function exposes it at
`POST /_playground/resolve-links` (`api/playground-resolve.mjs`).

```
POST /_playground/resolve-links {"urls": ["https://t.co/jO01BMGwXr"]}
→ {"links": {"https://t.co/jO01BMGwXr": {
     "meta": {"card":"summary_large_image","title":"Vercel Daemons is now in public beta - Vercel",
              "description":"…","domain":"vercel.com","image":{"width":2400,"height":1256},
              "display":"vercel.com/changelog/verc…"},
     "chain": [{"url":"https://t.co/jO01BMGwXr","status":301},
               {"url":"https://vercel.com/changelog/vercel-daemons-is-now-in-public-beta","status":200}],
     "finalUrl":"https://vercel.com/changelog/vercel-daemons-is-now-in-public-beta",
     "tags": {"twitter:card":"summary_large_image","og:title":"…","twitter:image":"…"},
     "notes": [], "cached": false, "ms": 675}}}
```

## The crawler, from X's own documentation

X's card docs went offline on 2026-11-02 (the old addresses redirect to
`docs.x.com`), so these are Wayback snapshots, read 2026-11-10:

| Claim | Source |
|---|---|
| User agent is `Twitterbot` with a version, e.g. `Twitterbot/1.0` | [Troubleshooting Cards](https://web.archive.org/web/20251231142717/https://developer.x.com/en/docs/x-for-websites/cards/guides/troubleshooting-cards) |
| Twitterbot implements Google's `robots.txt` specification; a blocked page gets no card, a blocked image a card without a picture | same page |
| A page response over 2 MB is too large for the crawler | same page |
| Images: JPG/PNG/WEBP/GIF, under 5 MB, SVG unsupported, first frame of a GIF, minimum 144×144 px | [Cards markup](https://web.archive.org/web/20260201012312/https://developer.x.com/en/docs/x-for-websites/cards/overview/markup) + the troubleshooting page |
| `twitter:title`←`og:title`, `twitter:description`←`og:description`, `twitter:image`←`og:image`, `twitter:image:alt`←`og:image:alt`; `twitter:site` has no OG equivalent; X reads `property` as well as `name` | Cards markup |
| "If an og:type, og:title and og:description exist in the markup but twitter:card is absent, then a summary card may be rendered." | Cards markup |
| Title max 70 characters, description max 200, image alt max 420 | Cards markup |
| Card tag data is re-indexed "roughly every seven days" | Troubleshooting Cards |

### What was verified live (2026-11-10, logged out, ≥1 s between requests)

- **The UA is what produces a card.** `https://t.co/jO01BMGwXr` with
  `Twitterbot/1.0` → `301 Location: https://vercel.com/changelog/vercel-daemons-is-now-in-public-beta`.
  The same URL with a browser UA → `200`, 351 bytes,
  `<noscript><META http-equiv="refresh" content="0;URL=…">`. Hence: send the
  Twitterbot UA, and follow meta refresh as well as 3xx.
- **Chains are long.** `https://t.co/V7GErf6DS1` reaches its carded page only via
  `t.co → nyti.ms → trib.al → nytimes.com?smid=tw-nytimes… → nytimes.com/…`
  (five 301s). X carded that post, so it follows at least that far.
- **Pages are bigger than the documented limit and still work.**
  `www.pcgamer.com/games/the-top-100-pc-games-2026/` is 2.59 MB; its tags are in
  the first kilobyte. Reading a 1 MB prefix resolves it — which is why the cap is
  on bytes read, not on `content-length`.
- **The OG fallback is real.** `www.bellingcat.com/follow-bellingcat-on-social-media/`
  sets `twitter:card=summary_large_image` and *no* `twitter:*` text or image; the
  captured post's card carries the page's `og:title` and `og:description`.
  `github.blog` and `theverge.com` pages set both, and X's card agrees with
  `twitter:*`.
- **`twitter:image` beats `og:image`.** `nytimes.com` sets both with different
  crops (`…-videoSixteen…` vs `…-facebookJumbo…`); X's card image is a 1.91 crop.
- **Self links have no card.** The ESPN post `2120010545872949671` has no card and
  its one link (`t.co/E8ZVi7PQis`) redirects to `twitter.com/espn/status/2120010545872949671/video/1`.
  That page's own tags say `summary_large_image` — a naive scraper would invent a
  card for X's own post. So: a chain that lands on `x.com`/`twitter.com` stops,
  and the result has no card.
- **Display is the unshortened form.** Every URL entity in the corpus is written
  as `t.co/…` and displayed as its expanded form (`bit.ly/4esrsTj`,
  `polygon.com/most-ambitious…`). `text-ProductHunt.html` is the decisive case:
  its `expanded_url` is *still* `https://t.co/CzX7ZkCvUz` while `display_url` is
  `producthunt.com/products/produ…` — X unshortened past the payload's own
  expansion before printing it. The display rule itself (host + 15 characters of
  the tail + `…`) is the one in `crates/xeet-text/src/display.rs`, derived from
  122 payload pairs.

## What the resolver does

1. **URLs** come from xeet's own entity extraction (the `entities` of an
   `explain` report), at most 4 per post; a URL is keyed in `post.links` exactly
   as it is written in the text, which is what `rules` looks up.
2. **Follow**: `fetch(redirect: "manual")`, 301/302/303/307/308 up to 10 hops,
   plus `<meta http-equiv="refresh">`; the chain is recorded as
   `[{url, status}]`. An X destination ends the walk.
3. **Card**: `twitter:card` (last tag wins, as X's docs describe); if absent and
   `og:type`+`og:title`+`og:description` are present, a `summary` card (the
   documented, hedged rule — the response says it was inferred). `player` without
   `twitter:player`, `app` without `twitter:app:name:*`, and unknown card types
   get no card. Fields fall back to OG as the table above; values are decoded and
   capped at X's documented lengths. `domain` is the **final** host verbatim
   (`www.nytimes.com` keeps its `www`, matching X's card payloads). Relative image
   URLs resolve against the final URL.
4. **Image**: fetched (following the same rules) to learn its dimensions — X needs
   them to reserve the box — and cached. Dropped, with a note, when it is not
   JPG/PNG/WEBP/GIF, over 5 MB, unreadable, or smaller than 144×144. With
   `media: true` the bytes are inlined as base64 in `post.links[url].image.data`.
   What X shows for a large or player card is not that file. X re-hosts
   the picture on its image service and the client asks for a copy fitted in
   a square box, the smallest of `name=small` (680), `900x900`, `medium`
   (1200) and `large` (2048) that is wider than the picture's slot in device
   pixels, the rule photos follow; Chrome scales the copy. A signed-in dpr 2
   session asked for `small` at card boxes of 284–336 css px, `900x900` at
   340–416 and `medium` at 476 and 564; nothing changes at the 512 px
   viewport breakpoint. Nobody but X can fetch that copy for an unpublished
   link, so the card layout marks the picture (`Item::Image.service`), the
   painter picks the box from the slot and the dpr (`paint::service_box`) and
   reproduces the resize (`paint::service_copy`, Lanczos-3) before it samples.
   A picture that already fits its box is drawn as given, so a phone at dpr 3
   with a 1200 px original draws what it always did. Against a 2× capture of
   a mobile-web cell whose card picture was 2400 px wide, scaling the original
   straight down left 2,599 mismatched pixels in the card; through the 680
   copy, 68 (measured with the fork's renderer, which draws that cell). The
   `summary` thumbnail's variant is not measured, so it draws the file as
   given.
5. **Display**: `xeet-text`'s form of the final URL, unless the post already came
   from a capture (`post.links[url].display` from the payload wins: a bit.ly link
   displays as `bit.ly/…` because that is what X printed, even though the final
   page is elsewhere).
6. **Cache**: `assets/vendor/playground/links/<sha256(url)>.json`, an hour;
   `refresh: true` (or `?refresh=1`) skips it. Failures that will not change
   (404, blocked, robots, private address) are cached for 10 minutes so typing
   does not hammer a dead link. Image bytes — card images and the post's own
   media — share one directory, `assets/vendor/playground/media/`, which is
   capped and evicted least-recently-used first (`XEET_MEDIA_CACHE_BYTES`,
   500 MB by default). See `docs/MEDIA.md`.

## Safety

The resolver runs server-side on user input, so every hop is checked *before* it
is fetched:

- `http:`/`https:` and default ports only;
- the hostname is resolved and **every** address must be public: loopback,
  RFC 1918, CGNAT (100.64/10), link-local (`169.254.0.0/16`, which is the cloud
  metadata endpoint), multicast, reserved, documentation, unique-local
  (`fc00::/7`), link-local v6 (`fe80::/10`), and IPv4-mapped/NAT64/6to4 forms of
  any of those are refused. Re-resolved on every hop, so a rebinding answer does
  not survive a redirect (the residual TOCTOU window needs a pinned dispatcher;
  it is not closed here);
- 5 s per URL, 10 hops, 1 MB of HTML, 5 MB per image, `content-type` checked
  before parsing;
- no cookies, no credentials, no redirects followed by `fetch` itself;
- one request a second per host, chained per host; a request aborted while
  waiting does not consume a slot;
- `robots.txt` per origin, cached an hour: the `Twitterbot` group (else `*`),
  longest match wins, `Allow` wins ties, `*` and `$` supported, a 401/403
  `robots.txt` means "stay out" (Google's spec, which X's docs point at).

Two env seams exist **for tests only**; `api/playground-resolve.mjs` reads them
when a test sets them: `XEET_LINKS_ALLOW_PRIVATE` (hosts exempt from the
address/port checks) and `XEET_LINKS_HOST_MAP` (`host=ip:port`). The offline
smoke test needs both because xeet's entity extraction does not treat an IP
literal as a URL, so the fixture has to be reached under a name with a real TLD.

## What is not verified

- **No live page was found** that has OG tags, no `twitter:card`, and a card on
  X. The summary fallback rests on the archived documentation's own wording
  ("may be rendered"); every other rule above has a live sample.
- **The hop limit** (10) is undocumented for Twitterbot; it is chosen to cover the
  five-hop chains the corpus produces with room to spare.
- **`robots.txt` for images**: the troubleshooting page says a blocked image
  yields a card without a picture; we apply the same check to the image request,
  which may be stricter than X in a corner case.
- **Timeouts**: X documents none for card fetches. Five seconds is a guess bounded
  by the playground's budget.
- **DNS pinning**: see the TOCTOU note above.
