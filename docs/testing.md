# Testing FlashRaw

FlashRaw processes untrusted binary input and promises exact, stable output, so the
test strategy has four layers.

## 1. Unit and property tests (`cargo test --workspace`)

- Bit pumps vs. bit-by-bit reference models (all 5 pump kinds, random widths).
- Huffman round-trips including 16-bit codes (slow path) and invalid tables.
- Lossless JPEG round-trips for all predictors 1–7 × 1–4 components, 16-bit range,
  tile mapping (2-component rows, clipping, multi-row JPEG rows), truncation.
- Packed unpack: every bit depth 1–16 × 4 packings × several widths, SIMD/fast vs.
  scalar reference — including exact-length and short sources with sentinel-filled
  outputs (catches paths that skip samples).
- TIFF/BMFF hostile structures, camera-db index integrity, types (CFA shifting,
  black-level patterns), develop kernel vs. a naive bilinear reference for Bayer and
  X-Trans at several origins, fused RGBA8 vs. two-pass for all 8 orientations.

## 2. The CC0 corpus

```sh
python3 tools/corpus/fetch.py            # CC0 raw.pixls.us corpus → corpus/ (~60 GB, resumable)
cargo build --release -p flashraw-cli
find corpus -type f -print0 | xargs -0 -n 50 target/release/flashraw verify --json > verify.jsonl
```

`tools/corpus/manifest.tsv` lists the 2,016 files of the raw.pixls.us corpus.
`flashraw verify --json` decodes each file out of process and records geometry, CFA,
black/white levels, white balance, colour matrix, orientation, the active-area maximum
and 99.99th percentile, and an FNV-1a 64 hash of the full sensor buffer. New decoders
and changes are checked against these records, the format documentation and the
camera database; all 1,980 decodable files must decode, and the remaining 36 must fail
with a clear error (unsupported codecs, mislabelled files).

## 3. Regression corpus (`cargo test --release -p flashraw --test regression`)

`tests/regression/*.tsv` lists 1,965 corpus files with the hash of their full decoded
buffer (sensor data as stored, without DNG opcode lists). The test decodes each listed
file and fails on any change. It also fails when the corpus is absent or a listed file is
missing (a green run must mean the hashes were checked); `FLASHRAW_ALLOW_NO_CORPUS=1` skips
deliberately (`tools/ci/local.sh` sets it when there is no corpus directory). The real-file mutations of
the malformed suite follow the same rule. A listed file that
decodes with a recovery warning (see below) also fails: complete files never warn.

`crates/flashraw/tests/orientation.rs` checks orientation records (standard tag, vendor
record, Auto Rotate setting), the resolved orientation, its source and each preview's
orientation on corpus files, and on files whose records disagree (Canon EOS 40D/6D/T3i/
T4i/SL1/T6/5D Mark II/1D Mark III, Nikon D70/E5700, Kodak Z980 and Ektra) when
`FLASHRAW_ORIENTATION_SAMPLES` points at a local copy of them (they are not
redistributable); each set is skipped when absent.

`crates/flashraw/tests/x3f_color.rs` develops every corpus X3F to RGBA8 and checks that the
white balance, colour matrix and lens shading come from the file, that the mean R/G and B/G
follow the embedded preview's, that the exposure is within -2.6..+0.6 EV of it, and that
clipped highlights render white.

`crates/flashraw/tests/canon_cr3_colour.rs` develops the corpus R6 Mark III, PowerShot V1
and R50 V CR3s (with older-layout R6 Mark II / R10 controls) to RGBA8 and checks the
maker-note black level and that the mean R/G and B/G are within 0.1 of the embedded
JPEG preview's.

## 4. Robustness

- `crates/flashraw/tests/malformed.rs`: synthetic DNG/BMFF files with truncated
  payloads, invalid offsets and counts, recursive IFDs, huge/zero/overflowing
  dimensions and tile sizes, invalid bit depths, over-subscribed or missing Huffman
  tables, corrupted entropy data, overlapping strips/tiles, dangling maker-note
  pointers, invalid BMFF sizes, garbage with every magic prefix; plus mutations of a
  real file of every extension in the corpus (truncation at 1–99.9%, header
  corruption, payload overwrite). Each must finish without panicking within a time
  bound; decodes of truncated data must fail under `CorruptionPolicy::Strict` and be
  flagged incomplete with warnings under the default `Recover`, never return silent
  garbage.
- `crates/flashraw/tests/truncation.rs`: synthetic DNGs cut at exact points: exact-length
  files, EOF at the end of the final strip, the final strip one byte short, a strip
  truncated by half, a tile past EOF, a strip offset past EOF, an offset + length that
  overflows, 12/14-bit packed data with an incomplete final sample group, one truncated
  strip among complete ones, every cut point of a multi-strip file, compressed vs.
  uncompressed consistency, region decodes and previews of truncated files. Each case
  checks the structured `Truncation` (unit, expected/available bytes) under Strict and
  the zero-filled samples and `DecodeWarning` under Recover. `crates/flashraw-cli/tests/`,
  `npm/tests/truncation.test.mjs` and `crates/flashraw-python/tests/test_errors.py` check the
  same through the CLI JSON, the JS API and the Python API.
- `crates/flashraw/tests/truncated_files.rs`: real files of every format cut at fractions of
  the file (0.1% to 99%) and around the start and end of their sensor data. A cut file must
  open with the true sensor geometry (never a thumbnail or preview taken for the raw image)
  or fail with a clear error; a decode that reports no warning must equal the complete
  decode, and a recovered one must fail under `Strict`. One file per extension and make by
  default, one per camera directory with `FLASHRAW_TRUNCATION_ALL=1`; synthetic NEF and DNG
  files (thumbnail IFD0, raw SubIFD) cover the rule without the corpus.
- `fuzz/`: cargo-fuzz targets `open_decode` (whole pipeline, all formats,
  decoding under both corruption policies with consistency checks), `tiff_parse`,
  `bmff_parse`, `bit_pumps` (with a model check), `huffman`, `ljpeg`, `packed_unpack`
  (fast path ≡ reference, partial rows), `deflate_float`, `dct12`, `dng_opcodes` (parse and apply
  arbitrary opcode lists), `icc_profile` (arbitrary bytes as an output ICC profile), and four
  that take their options from the last input bytes so the file's magic survives mutation:
  `decode_rgb` (demosaic, colour spaces, crops and the RGB output formats; frame size must match
  its geometry), `decode_parallel` (any thread count must equal the single-threaded result),
  `decode_region` (rectangles and tile indices, including hostile ones; a region must equal the
  same window of the full decode) and `decode_limits` (small and varying `Limits`; no frame or
  preview may exceed them). That is 15 targets, the entries of `fuzz/Cargo.toml`. Crash inputs are
  kept in `fuzz/regressions/<target>/` and replayed in CI; `fuzz/seeds/<target>/` holds small seed
  files (a valid tiled DNG and hostile tile sizes) and `fuzz/dictionaries/tiff.dict` TIFF/DNG tokens:

```sh
cargo +nightly fuzz run -O open_decode fuzz/corpus/open_decode fuzz/seeds/open_decode \
  -- -max_total_time=600 -dict=fuzz/dictionaries/tiff.dict
```

## CI

CI runs locally, not on hosted runners: the workflows in `.github/workflows/` are kept as
references for the steps but only start on manual dispatch.

```sh
tools/ci/local.sh                    # the steps the changes since origin/main need (default)
tools/ci/local.sh --changed HEAD~3   # ... since another base
tools/ci/local.sh --quick            # core steps whatever changed (~5 min)
tools/ci/local.sh --full             # everything (before a release)
tools/ci/local.sh --list             # show the selected steps without running them
FLASHRAW_MAC_SSH="-p 2222 -i key.pem user@mac" tools/ci/mac.sh   # same on a macOS host
```

Steps, summarised as PASS / FAIL / SKIP:

- `fmt`, `clippy` (`-D warnings`, also with all features), `test-debug` (unit tests with
  overflow checks), `test-release` (the workspace, including the C/C++ example clients),
  `no-default-features`, `corpus` (regression + malformed suites; needs `FLASHRAW_CORPUS`);
- `msrv` (Rust 1.85 for the library, 1.88 with every feature), `camera-db` (validation and
  the support-matrix check), `c-header` (regenerated `flashraw.h` must match), `gpu`;
- `c-api` (CMake install, then the C/C++ examples built against the install and run with
  ctest) and `c-api-pkgconfig` (link through the installed `flashraw.pc`);
- `wasm` (crate tests, wasm32 builds), `npm` (all six WASM variants, Node tests threaded and
  not, types, pack), `npm-browser` (Chromium, Firefox, WebKit), `npm-bundlers`;
- `python` (`maturin develop`, pytest, `mypy --strict`), `fuzz` (crash-regression replay and
  30 s per target), `benches` (compile only).

`--changed` maps changed paths to steps: documentation alone runs nothing; decoder crates run
the Rust lints and tests, the corpus suites and the WASM crate check; the facade crate adds
the Python and C API steps; `npm/` and the WASM crate run the npm package; manifests add
MSRV; `fuzz/` runs fuzzing. Browser tests, MSRV on non-manifest changes and fuzzing on
decoder changes are left to `--full`.

Steps whose tools are missing (CMake, Node.js, cbindgen, cargo-fuzz, the wasm32 target) are
reported as SKIP. `mac.sh` keeps a warm work directory on the host (`~/flashraw-ci`: build
cache, npm modules, the wasm-bindgen tool, the Python venv, a corpus sample) so repeat runs
only rebuild what changed; it fetches portable CMake and Node.js into that directory when
needed, drops the build cache when the host has less than `FLASHRAW_MAC_MIN_FREE_GB`
(default 10) GB free, and `--clean` removes the directory. Windows has no local runner:
the Windows-specific paths (MSVC builds of the C APIs, `raw.dll`) are untested until one
is available.

Tested platforms: Linux x86-64 (`tools/ci/local.sh`) and macOS arm64 (`tools/ci/mac.sh`). The hosted
workflows also define Linux ARM64, macOS x86-64 and Windows builds, but they only run on manual dispatch
and have not been run regularly, so those platforms count as untested.
