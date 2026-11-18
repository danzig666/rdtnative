# rdtnative

A native Reddit client for LG webOS TVs (webOS 4+), built for the Magic Remote. It is a Rust app
with no browser or Android runtime, built the same way as
[ytnative](https://github.com/danzig666/ytnative). Each user signs in with **their own Reddit
client ID**.

Status: Phase 0 (API spike). See [PLAN.md](PLAN.md) and [docs/phase0.md](docs/phase0.md).

```bash
cargo test                      # rd-core unit tests
cargo run -p rd-spike -- help   # the Phase 0 CLI
```

Licensed GPL-3.0-or-later. Not affiliated with Reddit or LG.
