# Releasing rdtnative

Releases are built by GitHub Actions (`.github/workflows/release.yml`), not on a PC.

1. Set the new version in `crates/app/pkg/appinfo.json` (and `version` in the workspace
   `Cargo.toml`), and write the notes in `docs/releases/v<version>.md`.
2. Commit, push, and tag: `git tag v<version> && git push origin v<version>`.
3. The workflow checks that the tag matches `appinfo.json`, runs the tests, cross-builds the IPK
   (with the DT_NEEDED check), and publishes the release with:
   - `io.github.danzig666.rdtnative_<version>_arm.ipk`
   - `io.github.danzig666.rdtnative.manifest.json` (webOS Homebrew Channel package manifest)
   - `repo.json` (a one-app Homebrew Channel repository)
   - `rdtnative-<version>.debug` (symbols for `cargo xtask crash`)

## Homebrew Channel

`repo.json` is the same shape as `repo.webosbrew.org/api/apps.json`. Each release carries a new
one pointing at its own manifest and IPK, so the newest release's copy is always current:

    https://github.com/danzig666/rdtnative/releases/latest/download/repo.json

Users add that URL under Homebrew Channel › Settings › Add repository. The package manifest alone
is at `releases/latest/download/io.github.danzig666.rdtnative.manifest.json`.

`cargo xtask release-manifest` writes both files into `target/ipk/` for a local IPK too.
