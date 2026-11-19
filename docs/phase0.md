# Phase 0: Reddit API spike

`rd-spike` is a small command-line tool that proves rdtnative's Reddit access works with **your own
client ID**. It saves real responses as test data for `rd-core`. It runs on a PC and on the TV.

## 1. Bring your own API key

rdtnative comes without a Reddit API key: you need your own client ID (with no secret), plus the
redirect URI and user agent that go with it. There are ways to get a working API key; search Reddit
for them.

A client ID is not a secret, but keep your **refresh token** private. `rd-spike login` stores
it in `.rdt-spike.json`, which is git-ignored and readable only by you.

## 2. Run it on a PC

```bash
cargo build -p rd-spike
alias rds=target/debug/rd-spike          # or target\debug\rd-spike.exe on Windows

rds setup <your-client-id>                         # manual entry (defaults for the rest)
rds setup <id> --redirect-uri <uri> --user-agent "<ua>"   # your own redirect URI / user agent
rds setup --phone                                  # or: scan the QR code and fill the form on your phone
rds token                  # app-only token + 5 posts from /hot (no account needed)
rds fixtures               # saves front page, a subreddit, r/all, popular subs, search, a thread, morechildren
rds login                  # optional: open the URL, approve, paste the URL you land on
rds login --phone          # same, but paste the URL on your phone via a QR code
rds fixtures               # again, now with /best and your subscriptions
rds soak 30 10             # 30 min, one request every 10 s: logs the rate-limit window
```

Until GitHub Pages is on (see §4), the redirect after `login` goes to a page that doesn't exist yet.
That is fine: copy the URL from the browser's address bar and paste it anyway, because the code is in
the URL.

## 3. Run it on the TV

Cross-build. CI also uploads the binary as the `rd-spike-armv7` artifact.

```bash
tools/build-tv.sh -p rd-spike            # Linux/macOS (needs: rustup target add armv7-unknown-linux-gnueabi; pip install ziglang==0.13.0)
cargo build --release --target armv7-unknown-linux-gnueabi -p rd-spike   # Windows, same as ytnative
```

Then copy it over and run it the same way:

```bash
scp target/armv7-unknown-linux-gnueabi/release/rd-spike root@<tv>:/tmp/
ssh root@<tv> 'cd /tmp && ./rd-spike setup <id> && ./rd-spike token && ./rd-spike fixtures && ./rd-spike soak 10 10'
```

Without root, `ares-push` it into a dev-mode app folder and run it through the dev-mode SSH shell, the
same as ytnative's `docs/debugging.md`. The binary needs only glibc 2.17, `libc`, `libpthread` and
`libdl`, and was checked under ARM emulation.

## 4. Turn on the callback page

Go to repository **Settings › Pages › Source** and choose **GitHub Actions**. The `pages` workflow then
publishes `site/` on every push to `main` that touches it. `site/callback.html` is served at
`/rdtnative/callback`.

## What to report back

- the output of `rds token` and `rds fixtures` (no tokens are printed);
- the rate-limit lines from `soak`, from both the PC and the TV;
- `spike-out/` zipped, if you are happy to share it. It is public Reddit content, plus your
  subscription list if you signed in.

## Exit criteria

- [ ] app-only token works and `/hot` parses (PC + TV)
- [ ] sign-in with code flow works and the refresh token survives `rds whoami` the next day
- [ ] all `fixtures` steps parse
- [ ] rate-limit headers present; `soak` shows no unexpected 403/429

## Entering the settings on a phone

`setup --phone` and `login --phone` work like RustCouch's **Type on phone**. A one-time page opens on
the home network: it uses the machine's LAN address, a random port and a random 128-bit path. It
accepts one valid submission and then stops. It also stops after 10 minutes, or when the tool exits.
A wrong value re-shows the page with a message under that field. The page is plain HTTP, so use it
on a home network you trust. The TV app uses the same `rd-phone` crate and the same field
definitions (`rd_core::config::api_fields`) for its on-screen editor.
