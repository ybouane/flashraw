# Contributing to FlashRaw

Thanks for helping. This page covers building, testing, how to add a camera or a format, code style and
how contributions are licensed. The per-format decoder contract is in [docs/contributing.md](docs/contributing.md).

## Reporting bugs and unsupported cameras

Use the issue templates on GitHub: *Bug report*, *Unsupported camera or format*, *Feature request*. For an
unsupported or wrongly decoded file, attach the output of `flashraw info FILE` and, if you can, a sample under
CC0 (see below). Vulnerabilities do not go in public issues: see [SECURITY.md](SECURITY.md).

## Building

Rust 1.85 or newer ([rustup](https://rustup.rs)).

```sh
cargo build --release                  # library and CLI (default workspace members)
cargo test                             # unit and integration tests (corpus tests skip without a corpus)
cargo build --release -p flashraw-cli --features gpu,jpegxl   # GPU backend and JPEG XL DNG
```

The WebAssembly, npm, Python and C packages have their own build steps: [docs/building.md](docs/building.md).

## Local CI, and why there are no GitHub workflows

The project does not run hosted CI. The workflows in `.github/workflows/` start only on manual dispatch, and
their steps are mirrored by scripts you run on your own machine before pushing:

```sh
tools/ci/local.sh              # the steps your changes need (since origin/main)
tools/ci/local.sh --quick      # fmt, clippy, tests, corpus, camera database, WASM crate (about 5 minutes)
tools/ci/local.sh --full       # everything, before a release
FLASHRAW_MAC_SSH="-p 22 user@mac" tools/ci/mac.sh   # the same on a macOS machine over ssh
```

The author runs it on Linux x86-64 and macOS arm64. Please run at least `--quick` and say in the pull request
what you ran. Windows is not covered by any runner.

Individual checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --release`, `tools/ci/check-licenses.sh`.

## Tests, corpus and fuzzing

* The regression suite decodes the CC0 raw.pixls.us corpus and compares hashes of the output. Fetch it with
  `python3 tools/corpus/fetch.py` (about 60 GB, resumable) into `corpus/`, then
  `cargo test --release -p flashraw --test regression --test malformed`.
* Fuzz targets live in `fuzz/` (needs nightly and `cargo install cargo-fuzz`):
  `cargo +nightly fuzz run -O open_decode fuzz/corpus/open_decode -- -max_total_time=600`.
  A crash input becomes a file in `fuzz/regressions/<target>/` and a test. See [docs/testing.md](docs/testing.md).
* A fix for a crash on a hostile file needs a regression test with a (synthetic, small) file.

## Adding a camera

1. Check what is there: `flashraw supported`, `flashraw info FILE`.
2. Camera database entries live in `crates/flashraw-camera-db/data/cameras.json` (format: [docs/camera-db.md](docs/camera-db.md)).
   Every value must be backed by a sample file: black and white levels, CFA, crops and colour matrices come from
   files that decode correctly, not from other projects' tables. `flashraw camera-db validate` checks the
   file; `tools/camera_db/rederive.py` checks entries against decodes of the sample files and
   `flashraw camera-db probe` shows what a file states by itself. Run `flashraw camera-db generate` to normalise
   the formatting, and `flashraw camera-db coverage` for the report.
3. Prefer a decoder that reads the values from the file over a database entry. The database refines; it
   must not gate decoding.
4. Add a sample (below) and a regression hash line under `tests/regression/`.

## Adding a format or compression mode

Follow the decoder contract in [docs/contributing.md](docs/contributing.md): cheap `open`, bounds-checked
access, limits before allocation, parallel decoding where the format allows it, truncation handling
and a documentation page under `docs/formats/` with the mode listed in `docs/support/*.tsv` (then
`python3 tools/support_matrix.py` regenerates `docs/formats.md` and the table behind `flashraw supported`).
A mode becomes `verified` only with corpus files whose hashes are pinned.

## Sample files

Samples must be your own photos or files you may release under **CC0** (public domain), for example raw
files uploaded to [raw.pixls.us](https://raw.pixls.us). Link the file in the issue; do not commit large
files. Do not send files with private content or from third parties without their permission.

## Code style

* `cargo fmt` (see `rustfmt.toml`) and `cargo clippy --workspace --all-targets -- -D warnings` must be clean.
  Workspace lints are in the root `Cargo.toml`.
* Public items of `flashraw` are documented (`#![warn(missing_docs)]`). Keep `unsafe` out unless it is
  measured to matter, and give every `unsafe` block a `SAFETY:` comment.
* No panics on file data: use checked arithmetic and the bounds-checked readers of `flashraw-core`.
* Keep comments about why, not what. Prefer American spelling in code and API names.

## Commits and pull requests

* Small, focused commits with a short imperative subject ("Fix Fuji compressed tile order") and a body that
  says why. Do not include generated files in unrelated commits.
* Fill in the pull request template: what changed, how it was tested, which local CI steps you ran.
* Mention other projects only where documented in the README or the benchmark documents; do not paste
  comparisons into code or other docs.

## Licence of contributions

FlashRaw is licensed **MIT OR Apache-2.0**. By submitting a contribution you agree that it is licensed under
both licences, as in the Apache-2.0 licence's section 5, without additional terms.

Do not copy code, tables or data from other projects unless their licence is compatible with
MIT OR Apache-2.0 and you say so in the pull request: in particular not from LGPL, GPL or otherwise
copyleft or unlicensed sources, and not from decompiled or leaked vendor code. Numbers must come from
files you may use, and format knowledge from documentation, specifications or your own analysis of sample
files.
