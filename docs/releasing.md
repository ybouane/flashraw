# Releasing

A checklist for a release of all packages ([packages.md](packages.md)). The first release, `0.1.0`, was published on 2026-10-25
(crates.io, PyPI and npm by hand); since then a published GitHub release publishes to the registries
(`release.yml`, first used for npm 0.2.0). Everything before that is run by hand, after the local CI ([testing.md](testing.md), `tools/ci/local.sh --full` on Linux and `tools/ci/mac.sh --full`).

## Before tagging

1. Version: set `version` in the workspace `Cargo.toml` (`[workspace.package]` and the `version` of every
   `[workspace.dependencies]` path entry), and `version` in `npm/package.json`. The Python version is read from
   the Rust crate.
2. `CHANGELOG.md`: move "unreleased" to the version and date; keep user-visible items.
3. Run the full local CI on Linux and macOS: `tools/ci/local.sh --full` and `tools/ci/mac.sh --full`. Includes
   fmt, clippy, tests, the corpus regression suite, fuzz smoke runs, MSRV, WASM, npm and Python.
4. Licence copies: `tools/ci/check-licenses.sh` (byte-identical `LICENSE-MIT` / `LICENSE-APACHE` in every
   package, SPDX `MIT OR Apache-2.0` in every manifest). Also run `cargo deny check` (`deny.toml`) and
   `cargo audit` if installed.
5. Packaging: `cargo package --workspace --allow-dirty` (omit `--allow-dirty` on a clean checkout) packages and
   verifies every crate against a local overlay of its siblings; `cargo publish --workspace --dry-run` does the
   same for the seven publishable crates without uploading. `cargo package -p <crate> --list` shows a crate's files.
6. Docs: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p flashraw -p flashraw-core -p flashraw-codecs -p flashraw-formats -p flashraw-camera-db -p flashraw-gpu`.
7. Check the install commands and links in `README.md` and `docs/packages.md`; update the status column.

## Publish

Publishing is done by `.github/workflows/release.yml`, the one workflow that starts without a manual dispatch:
it runs when a GitHub release is published. It checks that the tag is `v<version>` and matches `Cargo.toml` and
`npm/package.json`, builds and tests the wheels and the sdist (`python.yml`) and the npm package (`wasm.yml`)
from the tag, then publishes to crates.io (in dependency order: `flashraw-camera-db`, `flashraw-core`,
`flashraw-codecs`, `flashraw-formats`, `flashraw-gpu`, `flashraw`, `flashraw-cli`), PyPI and npm. No tokens are
stored: each registry has a trusted publisher for the repository `ybouane/flashraw`, the workflow `release.yml`
and the environment `release`. Versions already on a registry are skipped, so a run can be repeated.

1. **Tag**: `git tag -a v0.3.0 -m "FlashRaw 0.3.0" && git push origin main v0.3.0`.
2. **GitHub release**: create it from the tag with the CHANGELOG entry as notes
   (`gh release create v0.3.0 --title "FlashRaw 0.3.0" --notes-file notes.md --verify-tag`) and attach the C library
   archives if built. Publishing the release starts `release.yml`; follow it with `gh run watch`.
3. **Repeat or one registry only**: `gh workflow run release.yml -f tag=v0.3.0 -f crates=false -f pypi=false`
   (here npm only). A published version cannot be replaced, only yanked (crates.io), deleted (PyPI) or deprecated (npm).
4. **C library**: for each platform `cargo build --release -p flashraw-capi --features gpu,jpegxl` (or the CMake
   install); archive the shared and static library, `flashraw.h`, `flashraw.hpp`, `LICENSE-MIT`, `LICENSE-APACHE`.
5. Deploy the demo (`demo/`) if it should advertise the new version.

By hand, if the workflow cannot run: `cargo publish --workspace` (needs `cargo login`); `cd npm && npm run build &&
npm test && npm publish`; the wheels and sdist of a manual `python.yml` run, uploaded with `twine upload`.

## After

* Update the status in `docs/packages.md` and the install notes; add badges (crates.io, docs.rs, npm, PyPI).
* Open the next `CHANGELOG.md` section.
