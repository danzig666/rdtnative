# Phase 0: Reddit API spike

`rd-spike` is a small command-line tool that proves rdtnative's Reddit access works with **your own
client ID**. It saves real responses as test data for `rd-core`. It runs on a PC and on the TV.

## 1. Create your Reddit app (once)

1. Go to <https://www.reddit.com/prefs/apps> and choose **create another app…**
2. Set the type to **installed app**. A "web app" or "script" app needs a secret, which rdtnative never has.
3. Set the redirect URI to `https://danzig666.github.io/rdtnative/callback`.
4. Copy the client ID: the short string under the app name.

A client ID for an installed app is not a secret, but keep your **refresh token** private. `rd-spike login` stores
it in `.rdt-spike.json`, which is git-ignored and readable only by you.

## 2. Run it on a PC

```bash
cargo build -p rd-spike
alias rds=target/debug/rd-spike          # or target\debug\rd-spike.exe on Windows

rds setup <your-client-id>
rds token                  # app-only token + 5 posts from /hot (no account needed)
rds fixtures               # saves front page, a subreddit, r/all, popular subs, search, a thread, morechildren
rds login                  # optional: open the URL, approve, paste the URL you land on
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
