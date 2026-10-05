# LibRaw-compatible API

`crates/flashraw-libraw-compat` implements a pragmatic subset of the
[LibRaw](https://www.libraw.org/) C API on top of FlashRaw, so existing LibRaw clients can
switch by **recompiling against its headers and linking its library** instead of LibRaw's.
It is an independent implementation: it contains no LibRaw source code, and its headers were
written from the published interface (names, types, constants and documented semantics).

## Building and linking

```sh
cargo build --release -p flashraw-libraw-compat
# -> target/release/libraw.{so,dylib,a}   (Windows: raw.dll + raw.dll.lib, raw.lib)
cc -I crates/flashraw-libraw-compat/include app.c -L target/release -lraw -o app
```

Clients keep their `#include <libraw/libraw.h>` and `-lraw`; point the include path at
`crates/flashraw-libraw-compat/include` and the library path at the build output. Static
linking additionally needs the system libraries listed by
`rustc --print native-static-libs` (see [c-api.md](c-api.md#building-and-linking)), but not
`-lstdc++`, zlib, libjpeg or OpenMP.

`libraw_version()` returns `"0.22.0-FlashRaw"` and `libraw_versionNumber()` the 0.22.0 API
level; `LIBRAW_FLASHRAW_COMPAT` is defined for code that needs to tell the implementations
apart, and `libraw_flashraw_version()` returns the FlashRaw version.

## Source compatibility, not binary compatibility

**Rebuild your program against these headers.** Every structure the subset uses
(`libraw_iparams_t`, `libraw_image_sizes_t`, `libraw_colordata_t`, `libraw_imgother_t`,
`libraw_thumbnail_t`, `libraw_thumbnail_list_t`, `libraw_lensinfo_t`, `libraw_shootinginfo_t`,
`libraw_output_params_t`, `libraw_raw_unpack_params_t`, `libraw_rawdata_t`,
`libraw_processed_image_t`, the DNG colour/level structures) has exactly the member names,
types, sizes and offsets of LibRaw 0.22 on the same platform (checked by compiling both headers
and comparing). `libraw_data_t`, however, omits LibRaw's vendor maker-note block
(`imgdata.makernotes`), so every member after `imgdata.lens` sits at a different offset. A
binary compiled against LibRaw's headers must therefore never be run against this library; the
shared library has no LibRaw SONAME, so an existing binary will not pick it up by accident.

The `tests/layout.rs` test checks that the Rust definitions match the C header (Linux and
macOS CI; Windows CI compiles and runs the C client with MSVC).

## Supported subset

| function | status |
|---|---|
| `libraw_init`, `libraw_close`, `libraw_recycle`, `libraw_recycle_datastream` | implemented |
| `libraw_open_file`, `libraw_open_buffer`, `libraw_open_wfile` (Windows) | implemented (`open_buffer` does not copy, as in LibRaw: keep the buffer alive until recycle/close) |
| `libraw_unpack` | implemented: full sensor buffer in `rawdata.raw_image` (one sample per pixel), `rawdata.color3_image` (layered sensors) or `rawdata.color4_image` (other multi-sample data, e.g. linear DNG) |
| `libraw_unpack_thumb`, `libraw_unpack_thumb_ex` | implemented (largest preview / preview *i* of `thumbs_list`) |
| `libraw_dcraw_process` | implemented with FlashRaw's develop pipeline (see deviations) |
| `libraw_dcraw_make_mem_image`, `libraw_dcraw_make_mem_thumb`, `libraw_dcraw_clear_mem` | implemented (`malloc`-allocated; `free()` works too) |
| `libraw_dcraw_ppm_tiff_writer` | PPM/PGM implemented; TIFF output returns `LIBRAW_NOT_IMPLEMENTED` |
| `libraw_dcraw_thumb_writer` | implemented (JPEG as stored, RGB bitmaps as PPM) |
| `libraw_raw2image`, `libraw_free_image`, `libraw_subtract_black` | implemented |
| `libraw_adjust_sizes_info_only`, `libraw_adjust_to_raw_inset_crop` | implemented |
| `libraw_get_raw_width/height`, `libraw_get_iwidth/iheight`, `libraw_get_cam_mul`, `libraw_get_pre_mul`, `libraw_get_rgb_cam`, `libraw_get_color_maximum`, `libraw_get_iparams`, `libraw_get_lensinfo`, `libraw_get_imgother` | implemented |
| `libraw_set_demosaic`, `_output_color`, `_adjust_maximum_thr`, `_user_mul`, `_output_bps`, `_gamma`, `_no_auto_bright`, `_bright`, `_highlight`, `_fbdd_noiserd`, `_output_tif` | implemented (set `params`; see deviations for which ones affect processing) |
| `libraw_COLOR`, `libraw_get_decoder_info`, `libraw_unpack_function_name` | implemented |
| `libraw_strerror`, `libraw_strprogress`, `libraw_version`, `libraw_versionNumber`, `libraw_capabilities` | implemented |
| `libraw_cameraList`, `libraw_cameraCount` | return an empty list / 0 (FlashRaw identifies cameras from metadata) |
| `libraw_set_exifparser_handler`, `_makernotes_handler`, `_dataerror_handler`, `_progress_handler` | accepted, never invoked |
| `libraw_open_bayer` | `LIBRAW_NOT_IMPLEMENTED` |
| `libraw_open_file_ex`, `libraw_open_datastream` and other stream classes | not provided |

C++: `libraw.h` also provides a header-only `class LibRaw` with the commonly used members
(`open_file`, `open_buffer`, `unpack`, `unpack_thumb[_ex]`, `raw2image[_ex]`, `free_image`,
`subtract_black`, `adjust_sizes_info_only`, `dcraw_process`, `dcraw_make_mem_image`,
`dcraw_make_mem_thumb`, `dcraw_clear_mem`, `dcraw_ppm_tiff_writer`, `dcraw_thumb_writer`,
`get_mem_image_format`, `copy_mem_image`, `recycle`, `recycle_datastream`, `COLOR`, `version`,
`versionNumber`, `strerror`, ...). `imgdata` is a reference to the C handle, so
`rp.imgdata.sizes.width` works unchanged; `LibRaw_abstract_datastream` and the other stream
classes are not provided.

Extensions (prefixed `libraw_flashraw_`): `libraw_flashraw_last_error(lr)` returns the message
of the last FlashRaw error on a handle (also recovered data errors after `libraw_unpack`),
`libraw_flashraw_mem_image_format()` the geometry `get_mem_image_format` reports, and
`libraw_flashraw_version()`.

## Data fields

Filled by `libraw_open_*` (metadata) unless noted:

| field | contents |
|---|---|
| `sizes.raw_width/raw_height` | full sensor buffer, including masked borders (images wider or taller than 65535 return `LIBRAW_TOO_BIG`) |
| `sizes.width/height`, `left_margin/top_margin` | the active area (DNG ActiveArea or vendor equivalent) |
| `sizes.iwidth/iheight` | output size (half with `params.half_size`); after `dcraw_process` both pairs hold the processed size |
| `sizes.raw_pitch`, `pixel_aspect`, `flip` (0/3/5/6 and mirrored variants), `mask[]` (masked areas as top, left, bottom, right) | as LibRaw defines them |
| `sizes.raw_inset_crops[0]` | default crop when it differs from the active area; `[1]` DNG DefaultUserCrop; unknown entries have `cleft = ctop = 0xffff` |
| `idata.make/model/normalized_make/normalized_model` | FlashRaw's canonical names (e.g. "Nikon", "Z 6") |
| `idata.colors`, `filters`, `cdesc`, `xtrans`, `xtrans_abs` | LibRaw's CFA encoding (2 bits per site of an 8x2 tile with the second green numbered 3, `9` for X-Trans, 0 without a CFA), relative to the visible area |
| `idata.software`, `dng_version`, `is_foveon`, `raw_count` | from the file |
| `color.black`, `cblack[]` | black level; per-colour differences in `cblack[0..3]`, other patterns in `cblack[4]` x `cblack[5]` at `cblack[6..]`; refreshed by `libraw_unpack` (measured from masked areas where the format requires it) |
| `color.maximum`, `linear_max[]` | white level (refreshed by `libraw_unpack`) |
| `color.data_maximum` | set by `libraw_raw2image` / `libraw_dcraw_process`: largest black-subtracted value in the visible area |
| `color.cam_mul` | as-shot white balance, normalised to green = 1 |
| `color.cam_xyz`, `rgb_cam`, `pre_mul` | XYZ-to-camera matrix (file or FlashRaw camera database), camera-to-sRGB matrix and daylight multipliers derived from it |
| `color.curve` | identity; `model2` (firmware), `UniqueCameraModel`, `raw_bps`, DNG `dng_levels` / `dng_color[]` basics |
| `other.iso_speed`, `shutter`, `aperture`, `focal_len`, `timestamp`, `artist`, `parsed_gps`, `analogbalance` | from EXIF / DNG |
| `lens.*`, `shootinginfo.BodySerial` | lens make, model, serial, focal and aperture ranges, 35 mm focal length; body serial |
| `thumbnail.twidth/theight/tlength` | the default (largest) preview; `tformat`, `tcolors` and `thumb` after `libraw_unpack_thumb` |
| `thumbs_list` | up to 8 previews (format, size, flip, offset, length) |
| `rawdata.*` | after `libraw_unpack`: buffer pointers and copies of `iparams`, `sizes`, `color` |
| `image` | after `libraw_raw2image` or `libraw_dcraw_process` |
| `params`, `rawparams` | LibRaw's defaults (gamma 0.45/4.5, 8-bit sRGB output, automatic brightness, `user_flip` -1, ...) |

## Deviations

Decoding:

- Sensor data comes from FlashRaw's decoders. Formats FlashRaw does not support fail with
  `LIBRAW_FILE_UNSUPPORTED`; FlashRaw supports some files LibRaw does not (GoPro GPR,
  Blackmagic and ARRI files, several early Fujifilm models...).
- Fujifilm SuperCCD sensors are delivered in FlashRaw's layout, not rotated by 45 degrees, so
  their sizes and `filters` differ.
- DNG opcode lists are not applied by `libraw_unpack` (as in LibRaw without the DNG SDK).
- Black levels are reported in normalised form (common part in `black`, differences in
  `cblack`), already at `libraw_open_*` when the file records them. LibRaw sometimes keeps a DNG
  pattern entirely in `cblack[6..]` with `black` = 0; code that evaluates the full formula
  (`black + cblack[c] + pattern`) gets the same result either way. For Phase One and Leaf
  backs the measured black level is reported (LibRaw reports 0 and keeps it in internal tables).
- `cam_mul` is normalised to green = 1 (LibRaw keeps vendor scales). `cam_xyz` is filled from
  the file or the FlashRaw camera database whenever a colour matrix is known.
- Truncated or corrupt sensor data is recovered (missing samples zero-filled) instead of failing;
  the message is available from `libraw_flashraw_last_error`. Data-error callbacks are not called.
- `idata.maker_index` is 0; `imgdata.makernotes` does not exist.
- `other.timestamp` is the recorded capture time as local time (like LibRaw); on Windows it is
  interpreted as UTC.

Processing (`libraw_dcraw_process` + `libraw_dcraw_make_mem_image`):

- Demosaicing is FlashRaw's bilinear interpolation (or 2x2 binning with `half_size`), whatever
  `user_qual` says; there is no AHD/DCB/AAHD, no FBDD, no median filter, no chromatic aberration
  correction, no dark frame or bad-pixel file, no Fuji rotation and no pixel-aspect stretch.
- White balance: `user_mul` if set, else a grey-world balance with `use_auto_wb` (8x8 blocks
  containing clipped samples are skipped), else `cam_mul` with `use_camera_wb` when the file
  records one, else the daylight `pre_mul`; multipliers are normalised to a minimum of 1.
  LibRaw falls back to its automatic balance for some old cameras without a usable as-shot
  balance; this implementation uses `pre_mul` there.
- Highlights are clipped (`highlight` modes are ignored). `adjust_maximum_thr`, `user_black`,
  `user_cblack`, `user_sat`, `bright`, `no_auto_bright`, `auto_bright_thr`, `gamm[0..1]`,
  `output_bps` (8/16), `user_flip` and `half_size` are honoured.
- `output_color`: 0 (raw), 1 (sRGB), 2 (Adobe RGB), 5 (XYZ) and 8 (Rec. 2020) are supported;
  others return `LIBRAW_NOT_IMPLEMENTED`. ICC profiles (`output_profile`, `camera_profile`) are
  ignored.
- Four-colour CFAs (CYGM, RGBE; a few early compacts) are not developed with correct colour:
  FlashRaw's develop pipeline targets RGB colour filter arrays. Their raw data is exact.
- The output curve is a power law with a linear toe solved from `gamm[0]` and `gamm[1]` (BT.709
  by default), and automatic brightness clips `auto_bright_thr` of the pixels, following the
  same conventions; results are close to, not identical with, LibRaw's.
- Half-size output is `floor(width / 2)` wide (LibRaw rounds up).
- `dcraw_make_mem_thumb` returns JPEG previews exactly as stored (LibRaw inserts a minimal
  Exif block when the JPEG has none).
- Processing reads `rawdata`, so changes a program makes to `raw_image` before
  `dcraw_process` are honoured.

## Verification

`tests/c/libraw_client.c` is a typical C client (open, metadata, unpack, raw statistics,
`dcraw_process`, `make_mem_image`, thumbnail) that prints every value it reads. CI builds it
against this crate on Linux and macOS (`tests/client.rs`) and checks the values on repository
fixtures; with `FLASHRAW_CORPUS` set it also runs on one file per camera model and checks that
the raw data equals FlashRaw's own decode. The C++ client (`tests/c/libraw_cpp_client.cpp`)
exercises the `LibRaw` class.

The same client source also compiles against LibRaw 0.22.0. Run both builds over one file of
every camera model in the CC0 corpus (1,034 files; comparison tooling kept outside the
repository) and compare field by field:

Of the 1,034 files, 1,002 open and unpack with both (FlashRaw opens 22 of the 31 files LibRaw
rejects — GoPro GPR, ARRI, several CRW/RAF/RAW variants — and LibRaw opens one high-efficiency
NEF FlashRaw does not decode yet). On those 1,002 files:

| what the client reads | agreement | where it differs |
|---|---|---|
| `rawdata.raw_image` / `color4_image` / `color3_image` (FNV hash of the full buffer) | **1,002 / 1,002 bit-identical** | — |
| buffer layout used (`raw_image`, `color3_image`, `color4_image`) | 1,002 | — |
| `raw_width` x `raw_height` | 1,001 | one Kodak YRGB file |
| `colors`, `cdesc`, `flip` | 1,002 | — |
| `filters`, `COLOR()` of the first 2x2 sites | 993 / 962 of 971 CFA files | Fujifilm SuperCCD layout |
| visible area (`width`, `height`, margins), `iwidth`, `iheight` | 981 | SuperCCD; a dozen Pentax/Samsung/Ricoh/Leica DNGs where LibRaw applies its own crop |
| effective black (`black + cblack[c] + pattern`, after unpack) | 981 | Phase One / Leaf (LibRaw reports 0 and keeps black in its own tables; here the measured level), one Olympus and one Pentax file (±1) |
| `maximum` | 982 | Phase One / Leaf white levels |
| `data_maximum` | 975 | the same files |
| `pre_mul`, `rgb_cam` | 946 / 984 | colour matrices from the file or FlashRaw's camera database where LibRaw has none, or the reverse |
| ISO, shutter, aperture, focal length, timestamp | 944, 999, 989, 1,002, 993 | phone DNGs whose ISO LibRaw does not read; a few Leica M DNGs without an aperture tag |
| make / model strings | 986 / 864 | naming conventions (marketing names such as "EOS Digital Rebel XS" vs "EOS 1000D", "OM System" vs "OM Digital") |
| `unpack_thumb` succeeds | 976 | some MRW and Kodak previews are not exposed by FlashRaw |
| `dcraw_process` succeeds, output size | 1,002, 981 | output size follows the visible area |
| `make_mem_image` mean per channel within 3/255 (8-bit, camera WB) | 894 | different demosaic (bilinear vs AHD), WB fallbacks, four-colour sensors |

The raw data and the calibration a client needs to interpret it are the same; developed images
are close but, by design, not identical.

## Licensing

LibRaw is distributed under the LGPL 2.1 or the CDDL 1.0. This crate is licensed like the rest
of FlashRaw (MIT OR Apache-2.0) and does not include, link or derive from LibRaw code: the
headers are an independent declaration of a compatible interface (function names, parameter
and structure member names, types and order, constant names and values) written from the
published interface — the API documentation and the declarations of the public headers — with
no comments or code copied, and every function is implemented on FlashRaw. Interfaces of this kind are
generally treated as functional rather than creative expression, and reimplementing an API for
compatibility is a common, well-established practice; this is a technical note, not legal
advice, so projects with specific requirements should review it with counsel.

Using this crate does not make a program subject to LibRaw's licences, and programs that keep
using LibRaw are unaffected by FlashRaw's. "LibRaw" is used here only to identify the interface
this crate is compatible with; the project is not affiliated with or endorsed by LibRaw LLC.
