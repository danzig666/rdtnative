# rdtnative: a native Reddit client for LG webOS

Goal: a fast, fluid Reddit client for LG TVs, driven by the **Magic Remote** (pointer, wheel, OK)
and by the arrow keys. It is built the same way as
[ytnative](https://github.com/danzig666/ytnative): a native Rust app with no browser and no Android
runtime. It draws with GLES2 through the vendored plx-native UI core and plays video through the
TV's own decoder. [ReSync](https://github.com/danzig666/ReSync) is the **feature and behaviour
reference**, in the same way SmartTube was for ytnative. Its Java code is not ported.

Licence: GPL-3.0, because the plx crates are GPL-3.0. ReSync is a reconstruction of a closed app,
so we copy **no code and no assets** from it, only knowledge: endpoints, link rules and UX.

---

## 1. Feasibility in short

| Area | Verdict | Why |
|---|---|---|
| UI, focus, Magic Remote, packaging, CI, Homebrew | **High: reuse** | ytnative already solved these: plx focus engine, pointer hover/click, wheel, edge glide, OSK, sidebar, settings, i18n, IPK, symcheck, crash tracer, Windows simulator |
| Video (v.redd.it) | **High: reuse** | Reddit serves `DASHPlaylist.mpd` and HLS. ytnative's engine A (LG media service + DASH MPD) plays these with almost no new work |
| Images and galleries | **Medium** | `images.rs` and plx imgcache handle JPEG and PNG. Animated GIF and WebP, and a pan/zoom viewer, are new |
| Post and comment text | **Medium, the largest piece of new UI** | Reddit markdown (bold, italic, links, quotes, lists, code, tables, spoilers, superscript) needs a rich-text layout widget. plx has shaping and wrapping but no inline styling runs |
| Deep comment trees | **Medium** | Collapse, "load more", and jumping between parent and siblings need a custom list. ytnative's comments screen is a starting point |
| **Reddit access (auth and API)** | **The real risk** | See §2. This decides whether the project is viable, not the TV side |

**Overall:** the TV client itself is very feasible. About 60–70% of the non-Reddit code (the app
shell, UI core, input, player, images, settings, build) carries over from ytnative. Access to
Reddit's data is the open question and must be settled first (Phase 0).

## 2. The access problem (decide this first)

ReSync's README says it "works without the official Reddit API". Its source shows how
(`auth/WebSessionManager.java`, `auth/NativeLoginFragment.java`, `auth/WebViewCdpDriver.java`):

1. A hidden Android **WebView** holds a reddit.com browser session.
2. It mints a bearer JWT from `www.reddit.com/svc/shreddit/token` by running `fetch()` *inside*
   the WebView. A plain HTTP client gets a 403 because of Fastly **TLS fingerprinting**.
3. Login drives reddit.com's real login page over **Chrome DevTools Protocol**, so that the input
   events count as trusted and pass **reCAPTCHA Enterprise** and the bot challenge.

That does not carry over to ytnative's architecture: there is no browser engine in the app.
Pretending to be Reddit's web client is also explicitly against Reddit's terms, and it is
fragile. Any change to the fingerprint or challenge breaks every install at once, the same way
ytnative's `VISIONOS` stream window "can close at any time", but with more exposure. I don't
recommend building the TV app on TLS impersonation or captcha workarounds.

Options, best first:

| # | Path | Pros | Cons |
|---|---|---|---|
| A | **Official OAuth API with an approved client ID** (`oauth.reddit.com`). Sign in on a phone through a QR code: an authorize URL whose redirect goes to a tiny static page (GitHub Pages) that shows the code or forwards it to the TV over LAN | Clean and stable, with documented endpoints and 100 QPM per client. The same flow works for signed-out browsing with an `installed_client` token | Since Nov 2025, new OAuth clients need Reddit's approval under the [Responsible Builder Policy](https://r.datuan.dev/r/redditdev/comments/1oug31u/introducing_the_responsible_builder_policy_new/nocca6t). It is unclear whether a personal or hobby client gets approved |
| A′ | Path A with **each user's own client ID**: they register their own app and enter the ID through the QR or phone page | Spreads the rate limit and the approval burden | Same approval problem per user, and setup is awkward for non-technical users |
| B | **Anonymous public JSON** (`www.reddit.com/r/x/hot.json`, `/comments/id.json`) | No account and no key; read-only browsing | Heavily rate limited and often blocked for non-browser clients now. Must be measured from the TV's IP. Treat it as a fallback, not a foundation |
| C | ReSync's approach (browser-session token) | Works today on Android | Needs a browser-grade TLS stack plus captcha circumvention. Against Reddit's terms and the most fragile option. Not recommended |

**Design rule:** all of Reddit sits behind a `Session` trait in `rd-core` (`get(path)`,
`post(path, form)`, `token()`, `identity()`), so the UI never knows which path supplies the
token. Swapping A, A′ and B is then a configuration matter.

**Phase 0 is a go/no-go gate:**
1. Apply for API access (path A) immediately; it is the long pole.
2. Spike in Rust on a PC and then on the TV: measure anonymous JSON (B) for success rate,
   429s and blocks over a day of normal browsing.
3. If approval arrives, build on A. If not and B holds up, ship read-only on B. If neither works,
   stop before writing UI.

## 3. Architecture (mirrors ytnative)

```
rdtnative/
├── crates/rd-core/        pure Rust, unit-testable, no TV deps (like yt-core)
│   ├── session.rs         Session trait; oauth (A/A′), anon_json (B); token refresh; rate limiter (X-Ratelimit-*)
│   ├── model.rs           Listing<T>, Post, Comment, MoreChildren, Subreddit, Account, Message, Flair, Award
│   ├── listing.rs         paging (after/before), sorts + time ranges, best/home, r/all, popular, multis
│   ├── comments.rs        tree build, flatten-for-display with depth, /api/morechildren, collapse state
│   ├── media.rs           resolve a post's media: i.redd.it, preview/variants (pick by TV size), gallery
│   │                      media_metadata, v.redd.it (DASH/HLS/fallback mp4), imgur, redgifs, gfycat-dead,
│   │                      streamable, YouTube (hand off to ytnative via launch link), external links (QR)
│   ├── markdown.rs        Reddit markdown → styled runs/blocks (spoilers, superscript, tables, quotes)
│   ├── actions.rs         vote, save, hide, subscribe, reply, mark read (signed in only)
│   ├── search.rs          posts / subreddits / users, typeahead (subreddit autocomplete)
│   └── store.rs           accounts, subscriptions cache, visited/read posts, per-sub sort/view
├── crates/app/            the TV app (copy ytnative's crate, gut the YouTube screens)
│   ├── host.rs, main.rs, sdl.rs, wl.rs, ls2.rs, input.rs, edge.rs, images.rs, player.rs,
│   │   vplane.rs, settings.rs, i18n.rs, crashtrace.rs, relaunch.rs, device.rs, sim/   ← reuse
│   ├── richtext.rs        NEW: markdown blocks → laid-out styled glyph runs + link hit-boxes
│   ├── anim.rs            NEW: GIF/WebP frame decoder + texture ring (bounded memory)
│   └── screens/           feed, post, comments, viewer, subreddit, user, search, inbox, settings, signin
├── vendor/plx/            vendored UI core (same commit and local patches as ytnative, then diverge)
└── xtask/                 build / ipk / install / sim / symcheck / crash (copy from ytnative)
```

App ID: `io.github.danzig666.rdtnative`. It uses the same zig cross-build, stub-linked CI and
Homebrew Channel repo layout as ytnative.

## 4. Magic Remote UX: the "fluid" part

ytnative already has pointer hover and click, wheel, edge glide, and focus that follows the pointer.
On top of that, Reddit needs the following:

**Feed (home, subreddit, user)**
- Two views, settable per subreddit like ReSync's per-sub view mode. *Cards* is a poster grid
  with the plx `poster_grid` and title, sub and score under each tile. *List* is a compact row with
  a thumbnail, title, meta line and score, for text-heavy subreddits.
- Wheel scrolls smoothly by pixels, not rows, with inertia. Arrow keys move focus. Paging
  prefetches when focus is within one screen of the end, so there is no visible "loading" stop.
- Hovering a card for about 600 ms expands it in place: a larger preview and the first lines of
  the selftext. A GIF starts animating, muted. There is no inline video autoplay, because the TV
  has one hardware video plane.
- Hover actions on the focused card, reachable with the pointer or with ↑ from the card: upvote,
  downvote, save, open comments, open link (QR code).
- Colour buttons default to red = downvote, green = upvote, yellow = save, blue = comments, and
  can be remapped with ytnative's mechanism.

**Post and comments (split view)**
- The left 40% shows the post (media or selftext, scrollable). The right 60% shows the comment
  tree. This is ytnative's video-shrinks-beside-comments layout in reverse.
- Comment navigation: ↑/↓ moves to the next or previous comment at any depth. ←/→ goes to the
  parent or the next sibling. Long OK collapses or expands a thread. Channel ▲/▼ jumps between
  top-level comments. The wheel scrolls freely and the pointer focuses whatever is under it.
- A depth-coloured left rail like Sync's: clicking the rail with the pointer collapses that thread.
- "Load more (N)" rows fetch in place with no jump. Scroll position is kept when collapsing.

**Media viewer (full screen)**
- Images: OK toggles fit and 1:1. In zoom, dragging with the pointer (hold OK) pans and the wheel
  zooms around the pointer. ←/→ moves through gallery items, with a page counter and captions.
- Video: ytnative's player HUD, trimmed down (seek, pause, captions, audio on/off).
  ↓ shows the comments over the video.
- BACK always returns to exactly where you were: same scroll offset and same focused item.

**Text entry** (search and reply): ytnative's on-screen keyboard and search history, with
subreddit typeahead. Replying is done better on a phone, so offer "reply on phone" via QR with a
deep link as well as on-TV typing.

**Performance budget:** 60 fps scrolling on the C3. Image decode runs off the main thread and
picks the smallest `preview` variant that is at least the tile size, through a bounded texture
cache (plx imgcache). Comment rich-text layout is cached per comment and width.

## 5. Phases

| Phase | Content | Exit test |
|---|---|---|
| **0 Access spike (gate)** | API access request; Rust spike for A and B; rate-limit measurement from the TV | A token works on `/best` and `/comments`, or B holds up for a day. Otherwise stop |
| 1 Skeleton | New repo from ytnative: xtask, CI, vendor/plx, app shell, sim, IPK, Homebrew manifest; YouTube code removed | Empty app launches on the TV and in the simulator; CI green |
| 2 rd-core read path | Session, models, listings, comments tree + morechildren, media resolver, markdown parser; unit tests on saved JSON fixtures | `cargo test -p rd-core` covers 30+ real-world posts and threads |
| 3 Feed MVP | Sidebar (Home/Popular/All/Subs/Search/Settings), cards and list feeds, sorts, paging, image thumbnails | Smooth wheel and arrow browsing on the C3 at 60 fps |
| 4 Post + comments | Rich-text widget, split view, tree navigation, collapse, load more | A 2000-comment thread navigates without hitches |
| 5 Media | Image viewer (zoom/pan), galleries, GIF/WebP animation, v.redd.it via engine A, imgur/redgifs/streamable, YouTube hand-off to ytnative, QR for external links | Every ReSync link type opens or degrades cleanly |
| 6 Signed-in | QR sign-in, subscriptions, vote/save/hide/subscribe, inbox (read), reply (OSK + phone), multi-account | Actions round-trip; token refresh survives a day |
| 7 Polish | Settings (theme, OLED black, card size, NSFW blur/hide, filters by sub/keyword/domain, read-post dimming, per-sub sort/view), i18n, crash tracer, backup | ytnative-level settings parity for what applies |

Not planned: chat, which ReSync reverse-engineers; mod tools; posting new submissions, which a phone
does better; inline feed video autoplay.

## 6. What to take from each repo

**From ytnative (code):** the entire `xtask`, `.github/workflows`, `vendor/plx` with patches,
`crates/app/{host,main,sdl,wl,ls2,input,edge,images,player,vplane,settings,store,i18n,crashtrace,relaunch,device,sim}`,
the screens `browse`, `search`, `menu`, `settings`, `signin` and `comments` as templates,
`yt-core/{mpd,memory,fsutil,paths,backup}`, and `docs/debugging.md`.

**From ReSync (knowledge only):** the endpoint list (`net/oauth/*`: every request type Sync used),
`util/links/LinkTypeHelper` and `LinkHandler` (link classification rules), the media resolvers
(`net/thirdparty/*`: redgifs, imgur, streamable, v.redd.it), the comment navigation and collapse
behaviour (`CommentNavigation`, `CommentsStateHelper`), filters (`FilterHelper`), per-sub
sort and view helpers, and the settings inventory (`res/xml` preference screens), as the feature
matrix.

## 7. Open questions for the owner

1. Is it acceptable to ship read-only first (path B) if API approval is slow?
2. Should NSFW content be supported at all? It affects the media resolvers and needs an age gate in settings.
3. What is the minimum webOS? ytnative supports 4+; webOS 4 has no hardware video plane resize,
   so the split view would show video full screen there.
