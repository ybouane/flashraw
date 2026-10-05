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
- `fuzz/`: cargo-fuzz targets `open_decode` (whole pipeline, all formats,
  decoding under both corruption policies with consistency checks), `tiff_parse`,
  `bmff_parse`, `bit_pumps` (with a model check), `huffman`, `ljpeg`, `packed_unpack`
  (fast path ≡ reference, partial rows), `deflate_float`, `dct12`. Crash inputs are
  kept in `fuzz/regressions/<target>/` and replayed in CI.

```sh
cargo +nightly fuzz run -O open_decode fuzz/corpus/open_decode -- -max_total_time=600
```

## CI

- `ci.yml` (every PR): fmt, clippy (-D warnings), tests on Linux x86-64/ARM64, macOS
  x86-64/ARM64, Windows; no-default-features build; a small corpus sample with
  regression + malformed tests; camera-db validation; C API: CMake install and the C/C++
  examples built against it on Linux/macOS/Windows, plus a check that the generated
  `flashraw.h` is up to date (see [c-api.md](c-api.md#tests)); WASM builds (SIMD and not);
  GPU feature build; fuzz smoke (30 s per target + regression replay); bench
  compilation.
- `nightly.yml`: decodes the full corpus (verify records as artifact), regression and
  malformed suites over every file; 15-minute fuzzing per target.
- `perf.yml`: benchmark set on PR vs. main on the same runner; fails beyond the
  tolerance (see [benchmarks.md](benchmarks.md)).
- `python.yml`: abi3 wheels for Linux (manylinux x86-64/ARM64), macOS (x86-64/ARM64)
  and Windows plus an sdist; every wheel is installed and its pytest suite run on
  CPython 3.9 and 3.13; strict type check of the package (see [python.md](python.md)).
