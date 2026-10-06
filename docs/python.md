# FlashRaw for Python

The `flashraw` Python package wraps the Rust decoder with a small, typed API: open a RAW file
from a path, bytes or a file object, read its metadata and embedded previews, and decode the
sensor data (or display-ready RGB) straight into NumPy arrays without copying.

- **Zero-copy output**: the decoded buffer becomes the NumPy array's memory; no copy is made
  between the decoder and your code.
- **Parallel**: decoding releases the GIL, so Python threads decode files concurrently, and each
  decode still uses FlashRaw's own thread pool.
- **Typed**: metadata are frozen dataclasses, errors a `FlashRawError` hierarchy; the package
  ships `py.typed` and passes `mypy --strict`.
- **One wheel per platform**: abi3 wheels for CPython 3.9 and later on Linux (manylinux x86-64,
  ARM64), macOS (x86-64, ARM64) and Windows (x64).

Sources: `crates/flashraw-python` (native extension in `src/`, Python API in
`python/flashraw/`, tests in `tests/`, benchmark in `bench/`).

## Install

```sh
pip install flashraw            # NumPy is the only dependency
pip install "flashraw[pil]"     # + Pillow, for Frame.to_pil()
```

From a checkout (needs a Rust toolchain, 1.88+ for the JPEG XL support built into wheels):

```sh
pip install maturin
cd crates/flashraw-python
maturin develop --release      # into the active virtualenv
maturin build --release        # or build a wheel into target/wheels
```

An sdist needs the camera database next to its crate (the build script falls back to
`crates/flashraw-camera-db/data/cameras.json` outside the repository):

```sh
mkdir -p crates/flashraw-camera-db/data && cp camera-db/cameras.json crates/flashraw-camera-db/data/
cd crates/flashraw-python && maturin sdist
```

## Quick start

```python
import flashraw

with flashraw.open("photo.NEF") as raw:
    m = raw.metadata
    print(m.camera.name, m.exposure.iso, m.exposure.exposure_time, m.lens.model)

    frame = raw.decode()               # sensor samples as stored
    frame.array                        # numpy.ndarray, uint16, shape (height, width)
    frame.cfa.pattern                  # "RGGB"
    frame.black_level.mean, frame.white_level

    rgba = raw.decode("rgba8").array   # display-ready sRGB, uint8 (h, w, 4), orientation applied

# One-liner (closes the file before returning):
rgba = flashraw.imread("photo.CR3")
```

## Opening files

`flashraw.open(source, *, max_samples=None, max_dimension=None)` returns a `RawFile`.

| `source` | How it is read | Copies |
|---|---|---|
| `str` / `os.PathLike` | memory-mapped | 0 |
| `bytes`, `memoryview` of a whole `bytes` object | borrowed for the lifetime of the `RawFile` | 0 |
| binary file object (`open(..., "rb")`, `io.BytesIO`, ...) | `read()` once, the result borrowed | 0 after `read()` |
| `bytearray`, writable or partial `memoryview`, `mmap`, NumPy arrays, other buffers | copied once | 1 |

Mutable buffers are copied because decoding runs without the GIL: another Python thread could
otherwise change the bytes under the decoder. `RawFile.source` reports what happened
(`"mapped"`, `"borrowed"` or `"copied"`). Pass a path for the cheapest access to large files.

`max_samples` / `max_dimension` tighten the safety limits for untrusted input (defaults: 1.2 G
samples, 65,535 pixels per side). The input is released by `close()` (or at the end of a `with`
block); frames, previews and metadata already returned stay valid.

A `RawFile` may be shared between threads; concurrent decodes of the same file run in parallel,
and `close()` waits for decodes in progress.

## Metadata

`RawFile.metadata` (or `flashraw.read_metadata(source)`, which does not keep the file open) is a
frozen dataclass. Everything is plain data: `dataclasses.asdict(m)` gives JSON-ready dicts.

| Attribute | Content |
|---|---|
| `format`, `compression` | container (`"NEF"`, `"DNG"`, ...) and compression names |
| `camera` | `name`, `make`, `model` (as stored), `canonical_make`, `canonical_model`, `unique_model`, `serial`, `firmware`; `m.make`, `m.model`, `m.canonical_make`, `m.canonical_model` are shortcuts |
| `lens` | `make`, `model`, `serial`, `focal_range`, `aperture_range` |
| `exposure` | `iso`, `exposure_time` (s), `f_number`, `focal_length`, `focal_length_35mm`, `exposure_bias`, `flash`, `metering_mode`, `exposure_program` |
| `capture_time`, `gps`, `software`, `artist`, `copyright` | as recorded |
| `width`, `height`, `raw_width`, `raw_height`, `bits_per_sample` | visible image and sensor buffer sizes |
| `orientation`, `orientation_info` | EXIF orientation (1–8) and how it was resolved: the EXIF tag, the vendor record, the camera's Auto Rotate setting, `source` and the per-vendor `policy` |
| `cfa`, `layout` | `CfaPattern` (`pattern`, `width`, `height`, `color_at(x, y)`), `"bayer"`, `"x-trans"`, `"monochrome"`, `"linear"`, ... |
| `black_level`, `white_level`, `white_balance` | `BlackLevel` (repeating pattern, per-row/column deltas, `mean`, `at(x, y)`), white level per colour, as-shot multipliers |
| `color_matrices` | `color`, `forward`, `calibration` (`ColorMatrix` with `illuminant`, `temperature`, `as_array()`), `analog_balance`, `source` |
| `previews` | `PreviewInfo` for every embedded preview/thumbnail |
| `dng_version`, `dng`, `opcodes` | DNG version, rendering hints (`baseline_exposure`, `as_shot_neutral`, `default_user_crop`, `noise_profile`, profile tags, ...), DNG opcodes with WarpRectilinear / FixVignetteRadial parameters |
| `extra` | format-specific key/values |

`RawFile.frame_info` describes the sensor buffer before decoding (active area, default crop,
masked areas, sample format, ...), `RawFile.capabilities` the random access the format allows
and `RawFile.tiles` its independently decodable tiles/strips.

## Previews

```python
with flashraw.open("photo.ARW") as raw:
    jpeg = raw.preview_bytes()                 # largest embedded preview, encoded (JPEG)
    thumb = raw.preview_bytes("smallest")
    p = raw.preview(decode=True)               # Preview with .info and .array (h, w, 3) uint8
    raw.thumbnail().to_pil().save("thumb.jpg")  # Pillow
```

Selectors: `"largest"` (default), `"smallest"`, an index into `raw.previews` or a `PreviewInfo`.
Decoded previews do not have their orientation applied (`p.info.orientation`).

## Decoding

```python
frame = raw.decode(
    output="raw-u16",            # see the table below
    backend="auto",              # "auto", "cpu" (scalar), "simd", "gpu"
    threads=None,                # worker threads for this decode (None = all cores)
    region=None,                 # Rect / (x, y, width, height) / dict, sensor coordinates
    crop=True,                   # RGB outputs: default crop; "active": raw outputs → active area
    demosaic="bilinear",         # or "half-size" (2x2 binning)
    apply_opcodes=True,          # DNG opcode lists (bad pixels, gain maps, ...)
    corruption_policy="recover", # or "strict"
    exposure=0.0,                # RGB outputs: EV applied to linear values before encoding
    black_point=0.0,             # RGB outputs: linear value mapped to black (after exposure)
    white_point=1.0,             # RGB outputs: linear value mapped to white
    preview_fallback=False,      # RGB outputs of undecodable sensor data: embedded full-size preview
)
raw.decode_region((0, 0, 512, 512))    # same as region=...
raw.decode_tile(3)                      # one tile/strip of raw.tiles
```

| `output` | `array.dtype` | `array.shape` | Content |
|---|---|---|---|
| `raw-u16` | `uint16` | `(h, w)` or `(h, w, c)` | sensor samples as stored (floating-point DNGs scaled to integers) |
| `raw-f32` | `float32` | `(h, w)` or `(h, w, c)` | the same, as floats (floating-point DNGs unscaled) |
| `normalized` | `float32` | `(h, w)` or `(h, w, c)` | black-subtracted, white-normalised to `[0, 1]`, CFA layout kept |
| `linear-rgb` | `float32` | `(h, w, 3)` | demosaiced, white-balanced, linear sRGB primaries (sensor orientation) |
| `linear-rgb-f16` | `float16` | `(h, w, 3)` | the same as half floats: half the memory, developed straight to `float16` |
| `linear-rgb16` | `uint16` | `(h, w, 3)` | the same clipped to `[0, 1]` and scaled to `[0, 65535]` |
| `rgba8` | `uint8` | `(h, w, 4)` | display-ready sRGB, orientation applied |
| `rgb16` | `uint16` | `(h, w, 3)` | display-ready sRGB at 16 bits, orientation applied |

`exposure`, `black_point` and `white_point` are convenience controls for the RGB outputs:
`v' = (v * 2**exposure - black_point) / (white_point - black_point)` on linear values after
the colour conversion. Integer outputs clip the result to `[0, 1]`; float outputs keep it.
There is no tone curve or highlight recovery: editors do that.

Raw outputs have one sample per pixel for CFA and monochrome sensors and `c` (usually 3) for
linear DNGs and Foveon sensors.

A `Frame` carries everything needed to interpret the pixels:

| Attribute | Content |
|---|---|
| `array` | the NumPy array (owns the decoder's buffer); `np.asarray(frame)` returns it |
| `output`, `width`, `height`, `channels`, `bits_per_sample` | what was decoded |
| `active_area`, `crop`, `masked_areas` | `Rect`s in frame coordinates; `active_view()` / `crop_view()` return array views |
| `origin` | position of the frame in the sensor buffer (region/tile decodes, `crop="active"`) |
| `cfa`, `layout`, `color_at(x, y)` | CFA pattern (anchored at sensor `(0, 0)`) and the colour at frame coordinates |
| `black_level`, `white_level`, `white_balance`, `color_matrices`, `orientation` | calibration for this frame (`orientation` is 1 for `rgba8` and `rgb16`, which have it applied) |
| `warnings`, `incomplete` | repairs made under `corruption_policy="recover"` (and the `preview-fallback` substitution) |
| `source` | `"sensor"`, or `"preview"` when `preview_fallback=True` returned the embedded preview of undecodable sensor data (Nikon High Efficiency NEF) |
| `stats` | `total_ms`, per-stage `stages`, `backend` (`decode`, `simd`, `threads`, ...), `call_ms` (native call), `wall_ms` (Python call) |
| `metadata` | the file's `Metadata` |

`Rect` is a named tuple `(x, y, width, height)` with `right`, `bottom` and `slices`
(`array[rect.slices]` is a view).

## NumPy, Pillow, OpenCV

```python
import numpy as np

frame = raw.decode()                          # uint16 CFA data
active = frame.active_view()                  # view, no copy
black = np.float32(frame.black_level.mean)
white = np.float32(max(frame.white_level))
norm = (active.astype(np.float32) - black) / (white - black)

# In-place processing is fine: the array is writable and owned by you.
lin = raw.decode("linear-rgb").array          # float32 (h, w, 3)
lin *= 2.0                                    # +1 EV, no copy
np.clip(lin, 0.0, 1.0, out=lin)
```

Pillow (`rgba8` frames; lazy import):

```python
img = raw.decode("rgba8").to_pil()            # PIL.Image in mode RGBA
img.convert("RGB").save("photo.jpg", quality=92)
```

OpenCV expects BGR(A) channel order and names its Bayer codes after the pattern seen from the
second row and column (an `RGGB` sensor is `COLOR_BayerBG2*`):

```python
import cv2

bgr = cv2.cvtColor(raw.decode("rgba8").array, cv2.COLOR_RGBA2BGR)
cv2.imwrite("photo.png", bgr)

f = raw.decode(crop="active")                 # raw CFA data, active area only
pattern = f.cfa.shifted(*f.origin).pattern    # pattern at the array's (0, 0)
code = {"RGGB": cv2.COLOR_BayerBG2RGB, "BGGR": cv2.COLOR_BayerRG2RGB,
        "GRBG": cv2.COLOR_BayerGB2RGB, "GBRG": cv2.COLOR_BayerGR2RGB}[pattern]
rgb16 = cv2.cvtColor(f.array, code)           # OpenCV demosaic of the uint16 mosaic
```

imageio-style one-liner: `flashraw.imread(path_or_bytes, output="rgba8", **decode_options)`
returns the array and closes the file.

## Threads and the GIL

Every decode, file open and preview decode runs with the GIL released. Two levels of parallelism
are available:

- **Within a decode**: `threads=None` (default) uses FlashRaw's pool on all cores. Formats with a
  single sequential entropy stream are decoded on all cores by speculation, so one file at a time
  usually saturates the machine.
- **Across files**: decode from several Python threads (`concurrent.futures.ThreadPoolExecutor`).
  With `threads=None` all decodes share the global pool; `threads=1` decodes on the calling Python
  thread itself (the GIL is released), and an explicit `threads=n` gives each Python thread its own
  pool of `n` workers, so per-decode thread counts scale with the number of Python threads.

```python
from concurrent.futures import ThreadPoolExecutor

def thumb(path):
    with flashraw.open(path) as raw:
        return raw.decode("rgba8", demosaic="half-size", threads=1).array

with ThreadPoolExecutor(max_workers=8) as pool:
    images = list(pool.map(thumb, paths))
```

On the development machine (Ryzen 7 9700X, 8 cores/16 threads), 16 `raw-u16` decodes of a
10 MP DNG with `threads=1` each took 969 ms serially, 256 ms on 4 Python threads (3.8x) and
201 ms on 8 (4.8x). No process pool, pickling or shared memory is needed.

## Errors

All errors derive from `flashraw.FlashRawError` and carry a `kind`:

| Exception | Also a | When |
|---|---|---|
| `UnsupportedFormatError` | | not a RAW format FlashRaw understands |
| `UnsupportedCompressionError` | | known container, unsupported compression |
| `CorruptFileError` | | structurally invalid; `corrupt` / `truncation` describe truncated or undecodable sensor data |
| `InvalidMetadataError`, `CameraUnsupportedError` | | metadata unusable, camera/mode unsupported |
| `LimitExceededError`, `OutOfMemoryError` | `MemoryError` (OOM) | safety limits, allocation failure |
| `NotAvailableError` | | e.g. no preview, tile index out of range |
| `GpuUnavailableError` | | `backend="gpu"` without a GPU build/adapter |
| `InvalidArgumentError` | `ValueError` | unknown output/backend, empty region, ... |
| `IoError` | `OSError` (`errno` set) | reading the input failed |
| `ClosedError` | `ValueError` | the `RawFile` was closed |
| `InternalError` | | a bug in FlashRaw (a Rust panic never crosses into Python) |

Truncated or corrupt sensor data is recovered by default: every complete sample is decoded, the
rest zero-filled, and each repair reported:

```python
frame = raw.decode()
if frame.incomplete:
    for w in frame.warnings:          # DecodeWarning
        print(w.kind, w.unit, w.index, w.missing, w.samples_filled, w.message)

try:
    raw.decode(corruption_policy="strict")
except flashraw.CorruptFileError as e:
    t = e.truncation                  # CorruptInfo: unit, index, offset, expected, available, file_length
```

## Performance

The binding adds a fixed cost per call and never touches the pixels. Medians of 9 runs from
`crates/flashraw-python/bench/bench.py` (Ryzen 7 9700X, 16 threads, memory-mapped input),
`raw-u16` output:

| File | MP | CLI decode | Python `decode()` | native call inside it | binding overhead |
|---|---:|---:|---:|---:|---:|
| Canon 5D Mark IV CR2 | 31.3 | 32.2 ms | 30.2 ms | 30.1 ms | 0.18 ms |
| Canon R5 CR3 | 46.7 | 62.5 ms | 71.7 ms | 71.5 ms | 0.21 ms |
| Nikon Z 8 NEF (lossless) | 45.7 | 57.8 ms | 49.6 ms | 49.4 ms | 0.22 ms |
| Sony A7R V ARW (lossless) | 64.7 | 27.8 ms | 30.8 ms | 30.5 ms | 0.22 ms |
| Fujifilm X100V RAF | 26.7 | 45.7 ms | 46.7 ms | 46.5 ms | 0.27 ms |
| Pentax K-1 II DNG | 36.6 | 43.9 ms | 41.2 ms | 40.9 ms | 0.24 ms |
| Olympus E-M10 IV ORF | 20.3 | 88.7 ms | 72.2 ms | 72.0 ms | 0.22 ms |
| Nikon D850 NEF (uncompressed) | 45.7 | 7.3 ms | 9.1 ms | 8.8 ms | 0.25 ms |

"CLI decode" (`flashraw benchmark`) and the Python columns are separate runs on a shared
machine and differ by run-to-run noise; "binding overhead" is measured within the same call
(`frame.stats.wall_ms - frame.stats.call_ms`): about 0.2 ms per decode (argument parsing,
releasing and re-acquiring the GIL, wrapping the buffer, building the frame description),
~15 µs for a tiny region decode, and the same ~0.2 ms for `rgba8`. Opening a file costs
0.1–0.2 ms in Python against 0.05–0.15 ms natively. Metadata are converted once per file and
cached.

Tips:

- Pass paths rather than `bytearray`s (memory-mapped, no copy); `bytes` are not copied either.
- Reuse a `RawFile` for several decodes (regions, outputs) instead of reopening.
- For thumbnails, `raw.preview_bytes()` avoids decoding the sensor at all; `demosaic="half-size"`
  halves the RGB work.
- The first decode in a process starts the thread pool; benchmark after a warm-up.

## Implementation notes

- `flashraw._flashraw` (PyO3, abi3) holds a `FlashRaw` over the input (a memory map, a borrowed
  `bytes` object kept alive next to the decoder, or a copy) behind a read/write lock: decodes take
  it shared without the GIL, `close()` exclusively.
- Decoded `Vec`s are moved into NumPy arrays with `PyArray::from_owned_array`: the array's base
  object owns the Rust allocation and frees it when the array is collected. Frame descriptions
  cross the boundary as one dict per call; `python/flashraw/_types.py` turns them into frozen
  dataclasses.
- Rust panics are caught and raised as `InternalError`; exceptions are created by
  `flashraw._errors._make` so the Python layer owns the hierarchy.
- The crate is a workspace member but not a default member; `cargo build --workspace` builds it
  without a Python development library (`extension-module` is a default feature), and its tests
  are the pytest suite.

## Testing

```sh
cd crates/flashraw-python
maturin develop --release
pip install pytest pillow
pytest                                                        # API, zero-copy, threads, errors
FLASHRAW_CORPUS=/path/to/corpus pytest tests/test_corpus.py   # + corpus equivalence with the CLI
python bench/bench.py                                         # binding overhead vs the CLI
```

The suite covers every input kind and output, dtype/shape per output, zero-copy (array base,
ownership and buffer addresses, borrowed `bytes` inputs), GIL release (a Python thread keeps
running during a decode; threaded decodes beat serial ones), the exception hierarchy, truncated
data under both policies, thousands of truncated and corrupted inputs (no crash, only
`FlashRawError`s), and closing. `tests/test_corpus.py` decodes a sample of the corpus across
every file extension (`FLASHRAW_CORPUS_SAMPLE` files per extension, default 2) and requires the
FNV-1a hash of each `raw-u16` array to equal the `hash_full` of the native
`flashraw verify --apply-opcodes` (`FLASHRAW_CLI` or `target/release/flashraw`).
