# Release checklist

The repository is public and carries the `herdr-plugin` topic, so the Herdr marketplace can list it.

## Every release

- [ ] `version` agrees in `Cargo.toml`, `herdr-plugin.toml` and `Cargo.lock`.
- [ ] Formatting, strict all-target Clippy, locked tests and release builds pass on native macOS, Linux and Windows runners.
- [ ] Walk through `docs/manual-test.md`, including the client-witnessed checks.
- [ ] Tag `v<version>` on `main`, push the tag, and create the GitHub release with notes in user terms.
- [ ] The `Release binaries` workflow passes: it attaches five binaries (including Windows x64) and `SHA256SUMS`, checks every asset is present, and exercises actual published-binary installation on macOS and Windows.
- [ ] When deliberately updating a checkout installation, fast-forward the clean fork `main`, run `sh scripts/install.sh` on macOS/Linux or `scripts/install.ps1` on Windows, then `herdr-projects doctor` and `doctor --fix`. Do not replace binaries or restart tickers in unrelated running sessions merely to publish a release.

## Done

- [x] Public repository with the `herdr-plugin` topic, description and homepage set.
- [x] No "private repository" wording in `README.md` or `docs/getting-started.md`.
- [x] `v0.2.0`: the Herdr-native redesign.

## Open

- [ ] Verify `herdr plugin install ubranch/herdr-projects` from a clean machine: it clones, downloads the prebuilt binary, and registers the actions and popups.
- [ ] `docs/herdr-notes.md` and `docs/manual-test.md` name the author's machines and home paths. Generalise them.
