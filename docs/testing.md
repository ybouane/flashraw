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

`tools/corpus/manifest.tsv` lists the 2,016 files (every make and model available).
`flashraw verify --json` decodes each file out of process and records geometry, CFA,
black/white levels, white balance, colour matrix, orientation, the active-area maximum
and 99.99th percentile, and an FNV-1a 64 hash of the full sensor buffer. New decoders
and changes are checked against these records, the format documentation and the
camera database; all 1,980 decodable files must decode, and the remaining 36 must fail
with a clear error (unsupported codecs, mislabelled files).

## 3. Regression corpus (`cargo test --release -p flashraw --test regression`)

`tests/regression/*.tsv` lists 1,965 corpus files with the hash of their full decoded
buffer (sensor data as stored, without DNG opcode lists). The test decodes each listed
file and fails on any change. It is skipped when the corpus is absent. A listed file that
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
  (fast path ≡ reference, partial rows), `deflate_float`, `dct12`. Crash inputs are
  kept in `fuzz/regressions/<target>/` and replayed in CI.

```sh
cargo +nightly fuzz run -O open_decode fuzz/corpus/open_decode -- -max_total_time=600
```

## CI

CI runs locally, not on hosted runners: the workflows in `.github/workflows/` are kept as
references for the steps but only start on manual dispatch.

```sh
tools/ci/local.sh                 # everything on this machine (Linux or macOS)
tools/ci/local.sh --quick         # without MSRV, fuzzing, browser/bundler tests, benches
tools/ci/local.sh --only clippy,test-release
FLASHRAW_MAC_SSH="-p 2222 -i key.pem user@mac" tools/ci/mac.sh   # same on a macOS host
```

`local.sh` runs, and summarises as PASS / FAIL / SKIP:

- `fmt`, `clippy` (`-D warnings`, also with all features), `test-debug` (unit tests with
  overflow checks), `test-release` (the workspace, including the C/C++ example clients),
  `no-default-features`, `corpus` (regression + malformed suites; needs `FLASHRAW_CORPUS`);
- `msrv` (Rust 1.85 for the library, 1.88 with every feature), `camera-db` (validation and
  the support-matrix check), `c-header` (regenerated `flashraw.h` must match), `gpu`;
- `c-api` (CMake install, then the C/C++ examples built against the install and run with
  ctest) and `c-api-pkgconfig` (link through the installed `flashraw.pc`);
- `wasm` (crate tests, wasm32 builds), `npm` (build, Node tests threaded and not, types,
  pack), `npm-browser` (Chromium, Firefox, WebKit), `npm-bundlers`;
- `python` (`maturin develop`, pytest, `mypy --strict`), `fuzz` (crash-regression replay and
  30 s per target), `benches` (compile only).

Steps whose tools are missing (CMake, Node.js, cbindgen, cargo-fuzz, the wasm32 target) are
reported as SKIP. `mac.sh` copies the tree and a small corpus sample to the host, fetches
portable CMake and Node.js into its work directory when needed, runs `local.sh` there and
removes the directory afterwards (`--keep` to reuse it). Windows has no local runner:
the Windows-specific paths (MSVC builds of the C APIs, `raw.dll`) are untested until one
is available.
