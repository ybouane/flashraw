# flashraw

Fast, safe camera RAW decoding for Python, with zero-copy NumPy output.

`flashraw` reads RAW files from virtually every camera — DNG, CR2, CR3, CRW, NEF/NRW,
ARW/SR2/SRF, RAF, RW2, ORF, PEF, SRW, IIQ, 3FR/FFF, X3F, MRW, KDC/DCR, ERF, MEF and more —
and returns sensor-domain data (or display-ready RGB) as NumPy arrays, together with everything
needed to interpret them: CFA layout, black/white levels, white balance, colour matrices,
crops, orientation, lens and exposure metadata, and embedded previews.

- **Fast**: the decoder is written in Rust with SIMD kernels and multithreaded decoding
  (including single-stream formats such as CR2, lossless DNG and NEF).
- **Zero-copy**: decoded pixels become the NumPy array's memory; `bytes` inputs and paths
  (memory-mapped) are not copied either.
- **Parallel**: decoding releases the GIL — decode many files from Python threads.
- **Safe**: bounds-checked parsing, safety limits for untrusted input, fuzzed; malformed input
  raises a `FlashRawError`, never crashes the interpreter. Truncated files are recovered with
  structured warnings (or rejected with `corruption_policy="strict"`).
- **Typed**: frozen dataclasses for metadata, `py.typed`, `mypy --strict` clean.

One abi3 wheel per platform supports CPython 3.9 and later (Linux x86-64/ARM64, macOS
x86-64/ARM64, Windows x64). The only dependency is NumPy.

## Install

```sh
pip install flashraw
pip install "flashraw[pil]"   # optional: Frame.to_pil()
```

## Usage

```python
import flashraw

with flashraw.open("photo.NEF") as raw:          # path, bytes, bytearray, memoryview or file object
    m = raw.metadata
    print(m.camera.name, m.lens.model, m.exposure.iso, m.exposure.exposure_time)
    print(m.cfa.pattern, m.black_level.mean, m.white_level, m.white_balance)

    frame = raw.decode()                          # sensor data
    frame.array                                   # numpy uint16, shape (height, width)
    frame.active_view()                           # view of the active area

    rgba = raw.decode("rgba8").array              # display-ready sRGB, uint8 (h, w, 4)
    raw.decode("rgba8").to_pil().save("photo.png")

    jpeg = raw.preview_bytes()                    # embedded JPEG preview, no decoding

# One-liner:
image = flashraw.imread("photo.CR3")              # RGBA8 array
```

Outputs: `raw-u16` (uint16 samples as stored), `raw-f32`, `normalized` (float32 in [0, 1], CFA
layout kept), `linear-rgb` (float32, demosaiced, white-balanced) and `rgba8`. Options:
`backend`, `threads`, `region`, `crop`, `demosaic`, `apply_opcodes`, `corruption_policy`,
`preview_fallback` (RGB outputs of undecodable sensor data return the embedded full-size
preview, `frame.source == "preview"`).

```python
from concurrent.futures import ThreadPoolExecutor

def decode(path):
    return flashraw.imread(path, "raw-u16", threads=1)

with ThreadPoolExecutor(8) as pool:               # runs in parallel: the GIL is released
    arrays = list(pool.map(decode, paths))
```

Errors derive from `flashraw.FlashRawError` (`UnsupportedFormatError`, `CorruptFileError` with
structured truncation details, `InvalidArgumentError`, ...).

Documentation: [docs/python.md](https://github.com/ybouane/flashraw/blob/main/docs/python.md).
FlashRaw is also available as a Rust crate, a command-line tool and an npm package
([repository](https://github.com/ybouane/flashraw)).

## License

MIT OR Apache-2.0.
