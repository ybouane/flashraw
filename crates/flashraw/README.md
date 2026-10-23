# flashraw

Fast, safe camera RAW decoding in Rust: DNG, CR2, CR3, NEF, ARW, RAF, RW2, ORF, PEF and more, with
bounds-checked parsing, configurable `Limits` for untrusted files, multithreaded CPU decoding, an
optional GPU backend and the same API compiled to WebAssembly, Python and C.

```toml
[dependencies]
flashraw = "0.1"
```

```rust,no_run
use flashraw::{DecodeOptions, FlashRaw, OutputFormat};

fn main() -> flashraw::Result<()> {
    let raw = FlashRaw::open_path("photo.CR3")?;                 // headers only, memory-mapped
    println!("{} {}x{}", raw.metadata().camera(), raw.metadata().width, raw.metadata().height);
    let frame = raw.decode(DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() })?;
    println!("{} bytes of RGBA8", frame.as_u8().map_or(0, |d| d.len()));
    Ok(())
}
```

Of the 2,016 files in the raw.pixls.us corpus, 1,980 decode (98.2%); the other 36 are valid files in
formats that are not supported yet (28 Nikon High Efficiency NEF, Light L16, Casio CAM, JPEG-in-DNG), see
[docs/formats.md](https://github.com/ybouane/flashraw/blob/main/docs/formats.md).

* API documentation: <https://docs.rs/flashraw> (feature table on the crate page)
* Guide, benchmarks, CLI, Python, JavaScript and C bindings: [README](https://github.com/ybouane/flashraw/blob/main/README.md), [docs/packages.md](https://github.com/ybouane/flashraw/blob/main/docs/packages.md)
* MSRV 1.85. Licence: MIT OR Apache-2.0.
