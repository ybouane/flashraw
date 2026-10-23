# flashraw-cli

The `flashraw` command-line tool of [FlashRaw](https://github.com/ybouane/flashraw): inspect, decode,
convert and benchmark camera RAW files.

```sh
cargo install flashraw-cli                       # CPU only
cargo install flashraw-cli --features gpu,jpegxl # + GPU backend and JPEG XL DNG
```

The default build has neither the `gpu` nor the `jpegxl` feature: `--backend gpu` and JPEG XL
compressed DNG need the second command.

```sh
flashraw info photo.CR3                          # metadata summary (--json for scripts)
flashraw preview photo.CR3 preview.jpg           # embedded JPEG, no sensor decode
flashraw decode photo.CR3 out.tif --output rgb16 # develop to a 16-bit TIFF
flashraw supported                               # formats and the status of each mode
flashraw --help                                  # every command and option
```

Full reference: [README](https://github.com/ybouane/flashraw/blob/main/README.md#cli) and [docs/packages.md](https://github.com/ybouane/flashraw/blob/main/docs/packages.md).
Licence: MIT OR Apache-2.0.
