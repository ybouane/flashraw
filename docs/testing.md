# Testing FlashRaw

FlashRaw processes untrusted binary input and claims bit-exact compatibility, so the
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


```sh
python3 tools/corpus/fetch.py            # CC0 raw.pixls.us corpus → corpus/ (~60 GB, resumable)
```

run out-of-process so crashes are isolated), then compares:

| check | rule |
|---|---|
| active_area | `(left_margin, top_margin, width, height)` |
| cfa | pattern at the visible origin (2x2, 8x2 or X-Trans 6x6) |
| black | per 2x2 position: `black + cblack[colour] + cblack[6 + pattern]`, ±1 |
| white | `maximum`, ±0.2% |
| wb | as-shot multipliers normalised to green, ±1% |
| pixels | FNV-1a 64 hash of the full u16 buffer, exact |

`diff` re-decodes with both and prints the first differing pixel, the count of
differing samples and the maximum difference.

and reported as `KNOWN` rather than silently passing.

## 3. Regression corpus (`cargo test --release -p flashraw --test regression`)

`tests/regression/*.tsv` lists corpus files with the hash of their full decoded
(`tools/compat/regression_rows.py`). The test decodes each listed file and fails on
any change. It is skipped when the corpus is absent.

## 4. Robustness

- `crates/flashraw/tests/malformed.rs`: synthetic DNG/BMFF files with truncated
  payloads, invalid offsets and counts, recursive IFDs, huge/zero/overflowing
  dimensions and tile sizes, invalid bit depths, over-subscribed or missing Huffman
  tables, corrupted entropy data, overlapping strips/tiles, dangling maker-note
  pointers, invalid BMFF sizes, garbage with every magic prefix; plus mutations of a
  real file of every extension in the corpus (truncation at 1–99.9%, header
  corruption, payload overwrite). Each must finish without panicking within a time
  bound; decodes of truncated entropy streams must fail rather than return garbage.
- `fuzz/`: cargo-fuzz targets `open_decode` (whole pipeline, all formats),
  `tiff_parse`, `bmff_parse`, `bit_pumps` (with a model check), `huffman`, `ljpeg`,
  `packed_unpack` (fast path ≡ reference), `deflate_float`, `dct12`. Crash inputs are
  kept in `fuzz/regressions/<target>/` and replayed in CI.

```sh
cargo +nightly fuzz run -O open_decode fuzz/corpus/open_decode -- -max_total_time=600
```

## CI

- `ci.yml` (every PR): fmt, clippy (-D warnings), tests on Linux x86-64/ARM64, macOS
  x86-64/ARM64, Windows; no-default-features build; a small corpus sample with
  regression + malformed tests; camera-db validation; WASM builds (SIMD and not);
  GPU feature build; fuzz smoke (30 s per target + regression replay); bench
  compilation.
  check against the committed baseline; 15-minute fuzzing per target.
- `perf.yml`: benchmark set on PR vs. main on the same runner; fails beyond the
  tolerance (see [benchmarks.md](benchmarks.md)).
