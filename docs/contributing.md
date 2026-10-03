# Contributing to FlashRaw

This guide is the contract every format decoder follows. Read it before adding or
changing a decoder.

## Layout

| Path | Responsibility |
|---|---|
| `crates/flashraw-core` | Errors, bounds-checked byte access (`bytes`), bit pumps (`bits`), Huffman (`huffman`), TIFF/BMFF parsers, EXIF, value types (`types`), `FrameInfo`/`RawDecoder` contract (`decoder`), parallel helpers (`par`), profiling. No format knowledge. |
| `crates/flashraw-codecs` | Reusable compression codecs (lossless JPEG, packed unpack + SIMD, deflate/float, lossy JPEG, JPEG XL, vendor entropy coders). Codecs decode into caller buffers or row sinks; they never know about containers. |
| `crates/flashraw-formats` | One module per vendor/container: parse, build `FrameInfo` + `RawMetadata`, decode payload with codecs. `common.rs` has shared helpers. |
| `crates/flashraw-camera-db` + `camera-db/` | Camera database (`cameras.json` → compiled static tables). |
| `crates/flashraw` | Public API (`FlashRaw`, `RawFrame`, options, backend selection, optional post-processing). |
| `crates/flashraw-cli` | The `flashraw` binary. |
| `tools/` | Corpus download (`corpus/`), benchmark runner (`bench/`), support matrix. |

## Decoder contract

A format module returns `Box<dyn RawDecoder + 'a>` (usually `common::SimpleDecoder`)
from either `open(data, opts)` (magic-signature formats, with `probe(data)`) or
`open_tiff(data, tiff, opts)` (TIFF-based formats dispatched by EXIF Make in
`flashraw-formats/src/lib.rs`).

**Opening must be cheap**: parse headers and metadata only. Pixel decoding happens in
the decode closure. Do not copy the payload; borrow `&'a [u8]` slices of the input.

### `FrameInfo` semantics (coordinate convention)

All geometry is in the coordinate system of the *full decoded buffer* (including
masked/optical-black borders), origin at the first stored sample.

- `width`/`height`: full buffer dimensions: every stored sample, including masked
  borders (regression hashes cover the full buffer).
- `active_area`: valid image pixels.
- `crop`: recommended default crop (DNG DefaultCrop, vendor crop, or camera-db crop).
- `cfa`: pattern anchored at buffer `(0, 0)` (shift vendor patterns accordingly).
- `black_level`: pattern anchored at `(0, 0)`; set `black_from_masked = true` (with
  `masked_areas`) when the black level must be measured from optical black pixels.
- `white_level`: saturation in decoded-sample units.
- `white_balance`: as-shot multipliers normalised to G = 1 (`WhiteBalance::from_multipliers`).
- `color_matrices`: XYZ→camera matrices; file data first, `common::db_color` fallback.
- `compression`, `bits_per_sample`, `orientation`.

Use `common::db_lookup(&mut meta, mode)` to resolve canonical names and get the
camera-db entry, then `common::apply_db(&mut info, &meta, cam, Known { .. })` to fill
fields the file did not provide. **Unknown cameras must still decode** when the file
carries enough information; the database refines, it does not gate.

`mode` strings name the recording mode or payload layout (e.g. `12bit-compressed`,
`sRaw1`, `compressed`, `4:3`) so database entries resolve.

### Decoding

- Allocate output with `RawImage::new_u16(w, h, cpp, &ctx.limits)` (limit checked,
  lazily zeroed). Never allocate based on unchecked header values.
- Record stages via `ctx.profiler.time(Stage::EntropyDecode, || ...)` etc.
- Parallelise over independent units (strips, tiles, slices, blocks) with
  `flashraw_core::par` (`for_each_row_chunk`, `for_each_index`, `GridWriter` for tile
  grids). Respect `ctx.parallel`. Do not fake random access for sequential streams;
  set `random_access`/`units` honestly on `SimpleDecoder`.
- Use the core bit pumps / `HuffTable`. Codecs that need new primitives add them to
  `flashraw-codecs` (or `flashraw-core` if format-agnostic), with tests.
- Keep a simple scalar reference for any optimised kernel and test equality.

### Safety rules (untrusted input)

- No panics on any input: no unchecked indexing derived from file data, no
  `unwrap()` on parse results, checked arithmetic for offsets/sizes.
- No unbounded allocation from header values; no infinite loops (check
  `BitRead::overrun()` / `exhausted()` periodically in entropy decoders).
- Truncated payloads: return `Error::CorruptFile`, except where cameras routinely write
  slightly short final strips — then decode what exists.
- `unsafe` only with a `// SAFETY:` comment, tests, and a benchmark justifying it.

## Testing and validation

1. Corpus: `tools/corpus/fetch.py` downloads the CC0 raw.pixls.us samples to `corpus/`.
2. `cargo build --release -p flashraw-cli`
3. `flashraw verify --json <files>` decodes out of process and prints geometry, CFA,
   levels, white balance, colour matrix, orientation, data maxima and a full-buffer
   hash for each file; `flashraw decode --output rgba8 file out.jpg` gives a visual
   check. Validate against the format documentation and the camera's metadata.
4. Once a format's files are verified, pin them in the regression set
   (`tests/regression/<vendor>.tsv`: path, `hash_full` from `flashraw verify --json`,
   format, compression), checked by `cargo test --release -p flashraw --test regression`.

Add unit tests for every codec (synthetic round-trips where possible) and malformed
input tests (truncation, corrupted bytes) that assert errors instead of panics.

## Style

Idiomatic Rust, `cargo fmt` (120 columns), `cargo clippy` clean. Small hot-path
abstractions, explicit error context, comments that explain *why* (camera quirks,
format oddities) rather than restating code. Implement algorithms from
understanding of the format.
