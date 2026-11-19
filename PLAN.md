# rdtnative: a native Reddit client for LG webOS

Goal: a fast, fluid Reddit client for LG TVs, driven by the **Magic Remote** (pointer, wheel, OK)
and by the arrow keys. It is built the same way as
[ytnative](https://github.com/danzig666/ytnative): a native Rust app with no browser and no Android
runtime. It draws with GLES2 through the vendored plx-native UI core and plays video through the
TV's own decoder. [ReSync](https://github.com/danzig666/ReSync) is the **feature and behaviour
reference**, in the same way SmartTube was for ytnative. Its Java code is not ported.

Licence: GPL-3.0, because the plx crates are GPL-3.0. ReSync is a reconstruction of a closed app,
so we copy **no code and no assets** from it, only knowledge: endpoints, link rules and UX.

## Status

- **Decisions:** bring your own client ID (§2); NSFW **supported**, behind an age gate and a setting,
  blurred by default; **minimum webOS 4**, the same as ytnative. On webOS 4 the video goes full
  screen instead of sitting beside the comments.
- **API settings entry** (client ID, redirect URI, user agent): typed on the TV or entered on a
  phone via a QR code, the same as RustCouch. Done in `rd-core::config` + `rd-phone`; the TV screen
  itself comes with the app shell (Phase 1/3).
- **Phase 0:** done in code. `rd-core`, `rd-spike` and the callback page are built; the spike is
  still to be run with a real client ID on a PC and on the TV (see [docs/phase0.md](docs/phase0.md)).
- **Phase 1 (app shell):** done. `crates/app` is a slim app on ytnative's TV-proven platform
  modules (boot, input, Wayland surface, dev remote, crash tracer) and the vendored plx UI core.
  It has these screens:
  - Home: a welcome screen until an API is set up, then the front page (Hot) as a list.
  - Settings › Reddit API: three fields, an on-screen keyboard with live checks, and Enter on phone.

  Build tooling:
  - `cargo xtask build`/`ipk` produce the TV package on Linux or Windows.
  - CI builds the IPK.
  - A **Linux simulator** (`--features sim`, Xvfb-friendly) joins ytnative's Windows one, so the UI
    can be checked and screenshotted without a TV.
  - Not yet run on a real TV.
- **Phases 2-7:** done in code and tried in the simulator against live Reddit: media resolver,
  markdown, feeds in six layouts (two panes by default), post and comments, viewer, video player
  (engine A: the TV's media service), YouTube hand-off to ytnative, sign-in, actions, inbox,
  subreddits, search, settings (display, content, feeds, remote), filters and favourites. Still to
  check on the TV: video playback (DASH/HLS from v.redd.it), sign-in with a real redirect, and
  the frame rate with many pictures. Not done: animated GIF/WebP pictures (GIF posts play as
  video), webOS 4 video (needs ytnative's ACB engine B), multiple accounts.
---

## 1. Feasibility in short

| Area | Verdict | Why |
|---|---|---|
| UI, focus, Magic Remote, packaging, CI, Homebrew | **High: reuse** | ytnative already solved these: plx focus engine, pointer hover/click, wheel, edge glide, OSK, sidebar, settings, i18n, IPK, symcheck, crash tracer, Windows simulator |
| Video (v.redd.it) | **High: reuse** | Reddit serves `DASHPlaylist.mpd` and HLS. ytnative's engine A (LG media service + DASH MPD) plays these with almost no new work |
| Images and galleries | **Medium** | `images.rs` and plx imgcache handle JPEG and PNG. Animated GIF and WebP, and a pan/zoom viewer, are new |
| Post and comment text | **Medium, the largest piece of new UI** | Reddit markdown (bold, italic, links, quotes, lists, code, tables, spoilers, superscript) needs a rich-text layout widget. plx has shaping and wrapping but no inline styling runs |
| Deep comment trees | **Medium** | Collapse, "load more", and jumping between parent and siblings need a custom list. ytnative's comments screen is a starting point |
| Reddit access (auth and API) | **Resolved: bring your own client ID** | See §2. Official OAuth API; each user enters their own client ID through a QR setup page on their phone |

**Overall:** the TV client itself is very feasible. About 60–70% of the non-Reddit code (the app
shell, UI core, input, player, images, settings, build) carries over from ytnative. Access goes through the official API, with each user's own client ID (§2).

## 2. Reddit access: bring your own client ID (decided)

**Decision:** every user registers their own Reddit app and enters its **client ID** in rdtnative.
The project ships **no key**. The owner has an approved key for development. Each user's 100
requests per minute limit is their own, and Reddit's terms apply to each user's own key.

The scraping route that ReSync uses (a WebView-minted `/svc/shreddit/token`, TLS fingerprint
dodging and CDP login past reCAPTCHA) stays out of scope.

### What the user does once (on a phone or PC)
1. At reddit.com/prefs/apps, create an app of type **"installed app"**, with the redirect URI
   `https://danzig666.github.io/rdtnative/callback`. If Reddit requires approval for a new app,
   the user must have that approval too.
2. Copy the client ID (about 14–22 characters). An installed app has no secret.

### Getting the ID onto the TV and signing in (no typing on the remote)
```
TV                                   phone (same Wi-Fi)                 reddit.com / GitHub Pages
──                                   ──────────────────                 ─────────────────────────
start a LAN setup server
  http://<tv-ip>:8091/setup?n=<nonce>
show QR of that URL  ───────────────▶ opens the setup page served by the TV
                                      paste client ID, tap "Sign in"
                     ◀── POST id ──── (or "Browse without account")
TV stores the ID; page redirects ───▶ reddit.com/api/v1/authorize?client_id=…&response_type=code
                                        &duration=permanent&scope=…&redirect_uri=<pages callback>
                                        &state=<nonce>.<tv-ip>:8091
                                      user approves ─────────────────▶ 302 to the callback page
                                      static callback page does a top-level navigation (not a fetch,
                                      so no mixed-content block) to http://<tv-ip>:8091/cb?code=…&state=…
TV checks the nonce, exchanges the code ──▶ POST https://www.reddit.com/api/v1/access_token
  (Basic auth "<client_id>:" with an empty secret) → access token (1 h) + refresh token (permanent)
page shows "Done, look at your TV"
```
- **Fallback with no phone:** type the client ID with the on-screen keyboard. For sign-in, the TV
  shows the authorize URL as a QR code and the user enters the code shown on the callback page.
- **Browsing without an account:** the client ID alone gets an app-only token
  (`grant_type=https://oauth.reddit.com/grants/installed_client`, `device_id` stored per install).
  Listings, comments and search then work with no sign-in.
- The LAN server reuses the small HTTP server ytnative runs for DIAL (`:8090`). It only runs while
  the setup screen is open, accepts one nonce, and closes after it succeeds.
- **Several accounts:** each account stores `{client_id, refresh_token, username}`. Usually one
  client ID is shared by the household's accounts.
- **Required User-Agent:** `webos:io.github.danzig666.rdtnative:v<ver> (by /u/<username>)`.
  Before sign-in, the `by` part is left out.
- **Rate limiting:** read `X-Ratelimit-Remaining` and `X-Ratelimit-Reset`, and use a token bucket
  in `rd-core`. Prefetching backs off first, then images, so the user's own actions always get through.
- **Errors to show clearly:** an invalid client ID (401 on the token request), a wrong app type
  (the app must be "installed", not "web"), a redirect URI mismatch, and a revoked refresh token.
  Each gets a plain-language screen that points back to the setup QR code.

### Entering the settings on the TV (implemented in `rd-core::config` + `rd-phone`)
**Settings › Reddit API** shows three fields: **OAuth client ID**, **Redirect URI** and
**User agent**. The last two are optional and show their defaults as placeholders. Each field opens
on OK:
- **On the TV:** the on-screen keyboard (ytnative's OSK), or a USB keyboard.
- **On a phone:** an **Enter on phone** button shows a QR code. It works the same way as RustCouch's
  "Type on phone" (danzig666/webos-rustdesk-client): a one-time page on the TV's LAN address, with a
  random port, a random 128-bit path and one valid submission, open for 10 minutes. It has all three
  fields pre-filled, plus a link to reddit.com/prefs/apps so the client ID can be copied and pasted
  on the phone. Wrong values come back with a message under each field. The TV shows "Opened on your
  phone…" and fills in the fields when the form is sent.

Labels, help text, limits and validation are defined once (`rd_core::config::api_fields`,
`ApiConfig::from_fields`), so the TV editor and the phone page always agree. A custom redirect URI
works, but then the sign-in code is not handed to the TV automatically: the user pastes it on the
phone page or types it. A custom user agent is sent unchanged.

### Design rule (unchanged)
All of Reddit sits behind a `Session` trait in `rd-core` (`get`, `post`, `token`, `identity`), with
two implementations: `AppOnly` (the client ID) and `User` (the client ID and refresh token).

**Phase 0 (spike, no longer a gate on approval):** with the owner's client ID, build a Rust CLI
that runs the app-only token flow and the code flow, fetches `/best`, `/r/x/hot`, `/comments/id`
and `/api/morechildren`, saves the JSON as test fixtures and checks the rate-limit headers. Then
run the same binary on the TV.

## 3. Architecture (mirrors ytnative)

```
rdtnative/
├── crates/rd-core/        pure Rust, unit-testable, no TV deps (like yt-core)
│   ├── session.rs         Session trait; AppOnly + User (client ID / refresh token); refresh; rate limiter
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
| 0 API spike | Rust CLI: app-only token + code flow with the owner's client ID; save fixtures; check rate-limit headers; run on the TV | Both token flows work and the fixtures are saved |
| 1 Skeleton | New repo from ytnative: xtask, CI, vendor/plx, app shell, sim, IPK, Homebrew manifest; YouTube code removed | Empty app launches on the TV and in the simulator; CI green |
| 2 rd-core read path | Session, models, listings, comments tree + morechildren, media resolver, markdown parser; unit tests on saved JSON fixtures | `cargo test -p rd-core` covers 30+ real-world posts and threads |
| 3 Feed MVP | First-run client-ID setup (LAN page via QR + OSK fallback) with an app-only token; sidebar (Home/Popular/All/Subs/Search/Settings), cards and list feeds, sorts, paging, image thumbnails | Smooth wheel and arrow browsing on the C3 at 60 fps |
| 4 Post + comments | Rich-text widget, split view, tree navigation, collapse, load more | A 2000-comment thread navigates without hitches |
| 5 Media | Image viewer (zoom/pan), galleries, GIF/WebP animation, v.redd.it via engine A, imgur/redgifs/streamable, YouTube hand-off to ytnative, QR for external links | Every ReSync link type opens or degrades cleanly |
| 6 Signed-in | QR code-flow sign-in (§2), token refresh, subscriptions, vote/save/hide/subscribe, inbox (read), reply (OSK + phone), multi-account | Actions round-trip; token refresh survives a day |
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

## 7. Decided

- NSFW: supported. Settings has show, blur (the default) or hide, and the first use asks for an age
  confirmation. Media resolvers include the NSFW hosts that ReSync handles (redgifs).
- Minimum webOS: 4 (ytnative's floor).
