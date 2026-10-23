# Releasing

A checklist for a release of all packages ([packages.md](packages.md)). Nothing has been released yet;
the first release is `0.1.0`. There are no automatic GitHub workflows: everything below is run by hand, after
the local CI ([testing.md](testing.md), `tools/ci/local.sh --full` on Linux and `tools/ci/mac.sh --full`).

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

1. **crates.io**, in dependency order: `flashraw-camera-db`, `flashraw-core`, `flashraw-codecs`,
   `flashraw-formats`, `flashraw-gpu`, `flashraw`, `flashraw-cli`.
   `cargo publish --workspace` publishes them in that order and waits for the index between crates
   (needs `cargo login`); or publish one by one with `cargo publish -p <crate>`. A published version cannot be
   replaced, only yanked.
2. **npm**: `cd npm && npm run build` (needs the wasm toolchain, see [wasm.md](wasm.md)), `npm test`,
   `npm pack --dry-run` (the `prepack` hook refuses an incomplete `dist/`; check the file list includes
   the licence files), then `npm publish`.
3. **PyPI**: build the wheels and the sdist with maturin (the platforms of `.github/workflows/python.yml`,
   run on manual dispatch, or locally: `cd crates/flashraw-python && maturin build --release` per platform,
   `maturin sdist`), install each wheel into a clean environment and run `pytest crates/flashraw-python/tests`,
   check the licence files are in the wheel (`unzip -l`) and sdist, then `maturin upload` / `twine upload`.
4. **C library**: for each platform `cargo build --release -p flashraw-capi --features gpu,jpegxl` (or the CMake
   install); archive the shared and static library, `flashraw.h`, `flashraw.hpp`, `LICENSE-MIT`, `LICENSE-APACHE`.
5. **Tag and GitHub release**: `git tag -a v0.1.0 -m "FlashRaw 0.1.0" && git push origin v0.1.0` (pushing is done by hand, by the
   maintainer), create the release from the CHANGELOG entry and attach the C library archives (and prebuilt
   `flashraw` binaries, if built).
6. Deploy the demo (`demo/`) if it should advertise the new version.

## After

* Replace "unreleased" in `docs/packages.md` and the install notes; add badges (crates.io, docs.rs, npm, PyPI).
* Open the next `CHANGELOG.md` section.
