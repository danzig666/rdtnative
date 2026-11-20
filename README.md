# rdtnative

> [!IMPORTANT]
> **Bring your own API key.** rdtnative comes without a Reddit API key: it only works with a
> client ID you supply yourself (entered in Settings › Reddit API, with the remote or from your
> phone). With an app address (like `myapp://`) as redirect URI, signing in takes one copy and
> paste on a computer, once per account; the TV keeps the sign-in (see [Signing in](#signing-in)).

A native Reddit client for LG webOS TVs (webOS 4+), built for the Magic Remote. It is a Rust app
with no browser or Android runtime, built the same way as
[ytnative](https://github.com/danzig666/ytnative): the UI core is vendored from
[plx-native](https://github.com/GLinnik21/plx-native) and draws with OpenGL ES.

The API key (client ID), redirect URI and user agent are typed with the remote, or entered on a
phone by scanning a QR code (a one-time page on the home network).

> Not affiliated with Reddit or LG. Use at your own risk.

**Status:** a full client, tested in the simulator against live Reddit; TV testing next.

| | |
|---|---|
| ![Two panes: list and preview](docs/screenshots/feed-two-panes.jpg) | ![A post and its comments](docs/screenshots/post-comments.jpg) |
| ![Dense list](docs/screenshots/feed-dense.jpg) | ![Post menu (hold OK)](docs/screenshots/post-menu.jpg) |
| ![Subreddits](docs/screenshots/subreddits.jpg) | ![Display settings](docs/screenshots/settings-display.jpg) |

These are screenshots from the Windows simulator, which runs the same UI code as the TV build.

### Features

- **Feeds:** Home, Popular, All, any subreddit, favourites (one or all together), custom feeds,
  users, saved posts, search. Sorts (Best, Hot, New, Top, Rising, Controversial) with time ranges,
  remembered per subreddit. Endless paging; each feed keeps its place, and the start page can be
  the last feed shown.
- **Subreddits and users:** a subreddit's info page (members, rules, sidebar, join) and user
  profiles (karma, cake day, posts and comments).
- **Six layouts:** Two panes (a list with a live preview of the selected post and its top
  comments; the default), Cards (2-6 per row), List, Large, Compact and Dense (one line per post).
- **Posts:** pictures, galleries with captions, v.redd.it videos and GIFs (played by the TV),
  Imgur images, albums and GIFs, YouTube (opens in ytnative or LG's YouTube app), links (QR code),
  crossposts, and Reddit markdown: headings, quotes, lists, code, tables, spoilers, superscript,
  links.
- **Comments:** threaded with coloured depth rails, collapse with OK, "load more" in place,
  "continue this thread", OP / moderator / pinned marks, flairs; Left goes to the parent, Right to
  the next thread. Links to a comment open on it. Comments new since the last visit are marked
  (CH▲ / CH▼ jump between them), and Live mode keeps match and game threads updating.
- **Full screen:** pictures with zoom and pan, galleries with thumbnails, the video player with
  speed, quality, loop, replay and the remote's media keys.
- **Lean back:** a slideshow or a video channel of any feed, and an idle screen of pictures from a
  subreddit; the TV stays awake while they play.
- **Signed in:** vote, save posts and comments, hide (with undo), join and leave subreddits,
  reply (drafts are kept), inbox with new-message alerts, your saved and submitted posts. Several
  accounts with a switcher. With rdtnative's page as redirect URI, sign in on the TV itself or
  from a phone or computer; with an app address (like `myapp://`), one copy and paste on a
  computer, once per account (see [Signing in](#signing-in)).
- **Typing:** the on-screen keyboard, a phone (scan a QR code and type or dictate there), a USB
  keyboard, or LG voice search. Any post or comment can be sent to a phone as a QR code.
- **Display:** text size (70-175 %, for any TV size and distance) with a separate size for post and
  comment text, five fonts, five themes (including OLED black), eight accent colours, picture
  quality, title lines, clock.
- **Content:** NSFW and spoilers blurred, hidden or shown; read posts dimmed or hidden; hidden
  subreddits and words; pinned posts hidden; scores, flairs and sites on or off.
- **Remote:** pointer hover and click, pointer edge scrolling (speed setting), wheel step,
  smooth scrolling, hold OK for menus, colour buttons (green/red vote, yellow saves, blue
  comments) with a key hints line, BACK opens the side menu (twice leaves).
- **Setup:** Reddit API settings (client ID, any redirect URI scheme, user agent) and an Imgur
  client ID, typed with the remote or entered on a phone through a QR code.

## Signing in

Browsing works without an account once the API settings are in. Signing in adds voting, saving,
replies, your home feed and the inbox. How it goes depends on the redirect URI that comes with
your client ID:

| Redirect URI | Signing in |
|---|---|
| rdtnative's page, `https://danzig666.github.io/rdtnative/callback` | Nothing to copy: Reddit's answer comes back to the TV by itself, from the TV's browser, a phone or a computer. |
| An app address such as `myapp://…` (usual for a client ID you can't change) | **One copy and paste, on a computer.** Browsers can't open such an address and never hand it to a web page, so nothing can catch it by itself. This can't be avoided; you do it once per account, and the TV keeps the sign-in. |
| Another web address | One copy and paste, on a phone or computer: copy the address the browser ends up on and paste it on the sign-in page. If you can change the redirect URI to rdtnative's page, nothing needs copying. |

1. **Settings › Reddit API:** enter your client ID, with the remote or with "Enter on phone (QR
   code)". Enter the redirect URI that comes with your client ID exactly as you got it (like
   `myapp://callback`); leave it empty only if it is rdtnative's page. Press "Save and connect".
2. **Sign in** (side menu, or Settings › Sign in to Reddit). The TV shows the short address of its
   sign-in page (type it into a browser on the same Wi-Fi; a QR code where a phone can help) and
   the steps for your kind of redirect URI. The sign-in page works for 10 minutes; after that the
   TV shows a new address.
   - **rdtnative's page:** choose "Sign in on this TV" (Reddit opens in the TV's browser; log in
     with the remote and press Allow), or scan the QR code with a phone (or type the address on a
     computer) on the same Wi-Fi as the TV, press "Approve on Reddit", log in and press Allow. The
     TV signs in by itself.
   - **An app address:** on a computer on the same Wi-Fi, type the sign-in page's address into
     Chrome, Edge or Firefox and press "Approve on Reddit". Reddit opens in a new tab; log in if
     asked, but don't press Allow yet.
     - **Chrome or Edge:** in the Reddit tab, press Ctrl+Shift+J (Mac: Cmd+Option+J) first; a
       panel opens. Then press Allow. The page doesn't change, but a red line "Failed to launch
       '…' because the scheme does not have a registered handler" appears in the panel: select
       the whole line and copy it. No such line? Press Allow again.
     - **Firefox:** press Allow. Firefox shows an error page: copy all of its address bar.

     Switch back to the sign-in page's tab and paste it: it goes straight to the TV. Phones can't
     show this address. You do this once per account; the TV keeps the sign-in.
   - **Another web address:** after Allow, copy the address from the address bar (it has code= in
     it) and paste it on the sign-in page.
3. **More accounts:** Settings › Accounts › Add account. Switch between them there or from the
   side menu. Reddit approves whoever is signed in on reddit.com in that browser, so sign out
   there first or open the sign-in page in a private window.

## Install

- **Homebrew Channel:** Settings › Add repository:
  `https://github.com/danzig666/rdtnative/releases/latest/download/repo.json`, then install
  rdtnative from it. New releases show up there as updates.
- **By hand:** download the `.ipk` from [Releases](https://github.com/danzig666/rdtnative/releases)
  and run `ares-install --device <tv> io.github.danzig666.rdtnative_<version>_arm.ipk`.

Then enter your API key (client ID) in Settings › Reddit API (with the remote, or scan the QR
code and paste it on your phone). Releasing is described in [docs/release.md](docs/release.md).

## Building

Requirements: Rust stable, `rustup target add armv7-unknown-linux-gnueabi`, and
`python -m pip install ziglang==0.13.0`.

```bash
cargo test                       # rd-core, rd-phone, rd-spike, xtask
cargo xtask build                # cross-build for the TV (+ DT_NEEDED check)
cargo xtask ipk                  # target/ipk/io.github.danzig666.rdtnative_<ver>_arm.ipk
cargo xtask install              # install on a rooted TV over SSH (see docs/debugging.md)
```

Or install the IPK with LG's CLI: `ares-install --device <tv> target/ipk/*.ipk`.

### Simulator

The app runs in a 1920x1080 window on a PC: Windows (run `tools/fetch-sim-deps.ps1` once), or
Linux with SDL2, SDL2_ttf and libGL (`apt install libsdl2-dev libsdl2-ttf-dev libgl-dev`).

```bash
cargo xtask sim --detach         # build + start it
cargo xtask sim-key down ok      # press remote keys
cargo xtask sim-shot settings    # target/shots/settings.png
```

On a headless Linux machine, run it under `Xvfb :99` with `DISPLAY=:99`.

### Layout

- `crates/rd-core`: the Reddit API (OAuth, session, rate limits, posts, comment trees). Pure Rust.
- `crates/rd-phone`: the one-time "enter on phone" LAN form and QR code.
- `crates/rd-spike`: Phase 0 CLI for trying the API on a PC or the TV ([docs/phase0.md](docs/phase0.md)).
- `crates/app`: the TV app (plx UI core, SDL2, GLES2).
- `vendor/plx`, `xtask`, `ci`, `tools`: from ytnative.

## Credits and license

- [ytnative](https://github.com/danzig666/ytnative): the app shell, build system and TV integration.
- [plx-native](https://github.com/GLinnik21/plx-native) by Gleb Linnik and contributors: the
  vendored UI core (GPL-3.0).
- [ReSync](https://github.com/danzig666/ReSync): the feature reference (no code is copied).
- Fonts: Inter (OFL) and the other bundled fonts with their licences in `crates/app/pkg`.

rdtnative is licensed under the GNU GPL v3.0 (see [LICENSE](LICENSE)).
