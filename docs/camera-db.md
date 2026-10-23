# Camera database

`crates/flashraw-camera-db/data/cameras.json` is the single camera database for the whole library. It is
compiled into static tables by `crates/flashraw-camera-db/build.rs` (no runtime
parsing) and used by the decoders at decode time.

## What it is used for

Decoders follow **file first, database second**: they read everything the file states
(maker notes, DNG tags), then `common::apply_db(info, meta, cam, Known { .. })` fills
only the fields the file did not provide:

| Data | Used for |
|---|---|
| canonical make/model, aliases | `metadata.canonical_*`, matching rebadged bodies |
| CFA | colour layout when the file does not describe it |
| crop / masked areas | active area, optical-black regions |
| image crops | default crop: the camera's own image area (see "Default crops") |
| sensor levels (per ISO) | black/white level fallbacks |
| colour matrix | XYZ→camera matrices for every non-DNG format |
| hints | decoder quirks (e.g. CHDK/headerless file sizes, offsets, bit layouts) |
| fallback matrices | colour matrices and level hints matched by model-name prefix, for bodies without an exact entry |
| model aliases | regional/rebadged names → base model (Panasonic, Olympus, Leica) |
| size rules | sensor margins selected by raw buffer size (older Panasonic bodies) |

DNG files decode without the database; headerless raws depend on it entirely.

## Format

```json
{
  "version": 1,
  "cameras": [
    {
      "make": "NIKON CORPORATION",           // EXIF Make as written by the camera
      "model": "NIKON D850",                 // EXIF Model as written by the camera
      "mode": "14bit-compressed",            // decoder mode; "" = default
      "canonical_make": "Nikon",
      "canonical_model": "D850",
      "aliases": [{ "model": "...", "canonical": "..." }],
      "cfa": "RGGB",                         // "GGRGGB/GGBGGR/..." for wider patterns
      "crop": [0, 0, -32, 0],                // x, y, w, h; w/h <= 0 relative to right/bottom
      "black_areas_v": [[0, 56]],            // [x, width]
      "black_areas_h": [[0, 12]],            // [y, height]
      "sensor": [{ "black": 400, "white": 15520, "black_colors": [400, 401, 400, 399],
                   "iso_min": 100, "iso_max": 6400 }],
      "color_matrix": [10405, -3755, ...],   // XYZ(D65) → camera × 10000
      "hints": { "filesize": "...", "offset": "..." },
      "no_size_match": false,                // never identify a headerless file by size alone
      "orientation": {                       // optional; see "Orientation" below
        "policy": "",                        // overrides the make's rule ("" = use the rule)
        "maker_note_offset": 131,            // Canon CameraInfo byte: orientation sensor
        "auto_rotate_offset": 946            // Canon CameraInfo byte: Auto Rotate setting
      },
      "image_crops": [                       // optional; see "Default crops" below
        { "area": [8288, 5520], "crop": [16, 8, 8256, 5504], "files": 9 }
      ],
      "verification": {
        "corpus_files": 3,                   // corpus files of this camera/mode
        "observed_data_max": 15519,          // largest active-area sample in the corpus files
        "white_below_data": false,           // real samples exceed white by > 6%
        "notes": ["..."]
      }
    }
  ],
  "matrices": [
    { "canonical_make": "Canon", "model_prefix": "EOS 5D Mark IV", "black": 0, "white": 0,
      "color_matrix": [6446, -366, ...] }
  ],
  "model_aliases": [
    { "canonical_make": "Panasonic", "model": "DMC-LX100", "aliases": ["D-LUX (Typ 109)"] }
  ],
  "size_rules": [
    { "canonical_make": "Panasonic", "raw_width": 3130, "raw_height": 2319, "left": 4, "top": 0,
      "width_delta": -6, "height_delta": 0 }
  ],
  "orientation_rules": [
    { "canonical_make": "Canon", "model_prefix": "", "policy": "canon-auto-rotate-aware",
      "notes": ["..."] }
  ]
}
```

Mode strings name the recording mode or payload layout (`12bit-compressed`, `sRaw1`,
`compressed`, `4:3`, `chdk`, `dng`, `naked-<filesize>`, `x3f-<w>x<h>`, `arw2`, ...).
Lookup falls back to the default (`""`) mode entry.

## Orientation

A file can state its orientation twice: the standard TIFF/EXIF `Orientation` tag and a
vendor record. Decoders read both into `RawMetadata::orientation_info` and resolve
`orientation` with the camera's policy: the camera entry's `orientation.policy`, else the
longest matching `orientation_rules` entry (canonical make + model prefix), else
`exif-only`.

| Policy | Resolution |
|---|---|
| `exif-only` | Standard tag only; vendor records are informational. |
| `makernote-preferred` | Vendor record whenever present. |
| `makernote-when-exif-normal` | Vendor record when the standard tag is Normal, missing or invalid. |
| `canon-auto-rotate-aware` | Standard tag when valid and not Normal; vendor record when the tag is missing/invalid, or when the recorded Auto Rotate setting is on while the tag says Normal; otherwise the (Normal) tag. |

Current rules and the camera behaviour behind them:

- **Canon** (`canon-auto-rotate-aware`). EOS bodies record the orientation sensor in
  CameraInfo (MakerNote 0x000D: 0 = horizontal, 1 = 90° CW, 2 = 270° CW) on every shot, but
  write EXIF Orientation from it only when Auto Rotate is on. A Normal EXIF tag next to a
  rotated sensor field is therefore what the camera chose (Auto Rotate off, or the camera
  pointing straight down/up, e.g. copy-stand work) and is kept. The sensor field is used when
  EXIF Orientation is missing or invalid (EOS 40D and EOS-1D Mark III files can carry 0).
  Byte offsets come from `orientation.maker_note_offset` / `auto_rotate_offset` (fixed-layout
  bodies); newer CameraInfo records (starting `0xAAAA`) are searched for their orientation
  record instead. The Auto Rotate offset is listed only where real files confirmed it
  (EOS 6D).
- **Nikon** (`makernote-when-exif-normal`). Nikon's own software saves a user rotation in the
  NEF edit record (MakerNote 0x0E01, rotation in degrees) and re-renders the embedded
  previews rotated, leaving EXIF Orientation at Normal.
- **Kodak** (`makernote-when-exif-normal`). EasyShare KDC files keep Orientation 1 in their
  own IFDs; the camera's orientation sensor is in the EXIF block of its embedded full-size
  JPEG.

Previews are resolved separately from the RAW image (for these formats): a JPEG preview's
own EXIF orientation describes its pixels, and a preview whose aspect already matches the
rotated image was rendered rotated and gets `Normal`.

## Default crops

RGB outputs are cropped to `FrameInfo::crop` (and `RawMetadata::width/height` report
it; staggered and non-square-pixel sensors are then resampled to square pixels); it should be the image the camera itself produces from the active area, e.g.
8256×5504 for a D850 whose active area is 8288×5520. Resolution order, per file:

1. **The format's own crop field**: DNG DefaultCrop (DNG files keep it unchanged), Canon
   SensorInfo's valid-image rectangle narrowed to the AspectInfo size (1:1 / 4:3 / 16:9
   settings), Sony DefaultCrop, Nikon CropArea (maker-note tag 0x0045, recorded by
   newer bodies), Fujifilm RAF crop, Panasonic sensor borders, Olympus image-processing
   crop, Pentax/Samsung/Phase One/Kodak fields.
2. **The image size the file records** (`RawMetadata::image_size`, extra key
   `image_size`): the EXIF pixel dimensions, or a maker-note field (Sony FullImageSize,
   with the EXIF size selecting another aspect ratio).
3. **The embedded full-size JPEG**, the camera's own rendering: its dimensions.

A size from 2 or 3 is accepted when it fits the active area (either orientation),
differs from it, and spans (nearly) the whole area in one direction (border pixels, or
a narrower aspect ratio / DX / APS-C crop of it). It is placed at the offset in the
entry's `image_crops` rule for that active-area size and image size, else centred on
even offsets. 4. **Without a size in the file**, the entry's first `image_crops` rule
for the active-area size applies (the largest image area of that body).

`RawMetadata::extra` key `crop_source` records which one applied (`file`, `image-size`,
`preview`, `camera-db`). Crop origins keep the active area's CFA phase (even offsets);
the vendor rectangles are moved by at most one pixel for that.

`image_crops` rules are measured from files: the size is the camera's own (step 1–3
above), and the position is where the camera's embedded JPEG lies inside the active
area, found by matching it against a half-size development of the raw data
(`flashraw camera-db crops --register`, sub-pixel normalised cross-correlation of the
central 80% of the frame). A measured position replaces the centred one only when it
differs by more than a pixel and agrees (within 2 px, correlation >= 0.95) across at
least two files (D810, D200, Nikon 1 bodies, Pentax K-5, Sigma Merrill, ...).

Measured over the 2,016 corpus files and 4,321 real-world files: 702 of the 844 non-DNG
models get the camera's image area (every crop inside its active area; RGBA outputs have
the camera's JPEG dimensions, rotated by orientation). Without any size in their files
remain: headerless/CHDK raws, early CRW and Olympus interlaced ORF, Nikon D1/D100/D2H and
Coolpix E-series NEF, Hasselblad 3FR/FFF, Leaf/Mamiya, Kodak DCR/DCS, older Phase One IIQ,
Fujifilm SuperCCD RAF, Sigma SD9/SD10. Nikon Bayer RAW M/S files (D500, D6, D850, Z
bodies) store more photosites than the camera's M/S JPEG has pixels (about 8/7 of the M
size, 3/2 of the S size, in both directions): their crop is the CropArea the camera scaled
its JPEG from, so their RGB outputs stay larger than that JPEG. YCbCr sNEF files (D810,
D4S, ...) are stored at the camera's S size. Sony A500/A700 and Samsung NX3000 record image
sizes wider than the decoded active area and are left unchanged.

```sh
tools/camera_db_crops.py measure --out crops.jsonl   # corpus + real-world files
tools/camera_db_crops.py propose --in crops.jsonl    # per entry / active-area size
tools/camera_db_crops.py propose --in crops.jsonl --write   # store image_crops
tools/camera_db_crops.py check --in crops.jsonl      # crops inside active areas, sizes
```

## Contents (current)

| Section | Entries |
|---|---:|
| cameras (incl. per-mode entries) | 1,631 |
| fallback matrices | 814 |
| model-alias groups | 71 (139 aliases) |
| size rules | 25 |
| orientation rules (per make) | 3 |
| cameras with orientation data | 25 |
| cameras with image crops | 877 (1,026 rules) |

## Verification

Every entry with corpus samples is checked against FlashRaw's decodes of the 2,016
CC0 corpus files (`verification`): the number of files, the largest active-area sample
actually present, and a flag when real samples exceed the entry's white level. Where
the data maximum showed a white level to be wrong the level follows the data (13 entries,
e.g. DMC-FZ2000, DC-GH5S, Nikon 1 J1/V1); the remaining
`white_below_data` flags are explained in their notes (16-bit IIQ data, whites that
depend on the recording mode). 24 cameras present in the corpus but missing from the
database were added from corpus observations.

## Sample-file coverage

Values are checked against the sample files by field group: `identity` (make/model/mode),
`names` (canonical make/model), `aliases`, `cfa`, `crop`, `black_areas`, `black`, `white`,
`color_matrix`, `hints`, `orientation`, `image_crops`, `no_size_match`. A value counts as
taken from files when

- the camera's files state it and the decoders reproduce it: names read from the EXIF
  Make/Model strings, CFA, active area, masked areas, black and white levels the file
  records (the median over the files when each states its own), container maximum as white
  level;
- it is a colour matrix read from camera-written DNG files of that body (`ColorMatrix1/2`
  with their calibration illuminants, interpolated to D65 in inverse colour temperature);
- it is measured from decoded sample data: the saturation plateau of clipped highlights
  (white), the mean of declared optical-black areas (black), sharp margin edges (masked
  bands, crops), file sizes (headerless dumps), image areas registered against the camera's
  JPEG (`image_crops`).

Everything else is kept as stored: it is used as is, and no file in the available sets
contradicts it. `tools/camera_db/rederive.py ledger` writes a ledger (untracked) of which
values the files cover, and `flashraw camera-db coverage` prints a neutral report of how many
entries carry each kind of data per make.

Of the 1,631 entries, 110 have every value taken from files; the others keep at least one
value as stored (4,972 of the 10,526 field groups of the entries). Colour
matrices are the largest block: a non-DNG camera's own files rarely carry one (12 of 1,186
entry matrices and 36 of 814 fallback matrices are taken from files), and 543 entries have
no sample file at all. [camera-db-audit.md](camera-db-audit.md) lists, per field group and
make, what the files reproduce, what differs, what has no sample and what cannot be derived
from files.

### Re-deriving the data

```sh
tools/camera_db/rederive.py probe              # probe the corpus + real-world set (empty-database build in target/rederive/)
tools/camera_db/rederive.py audit [--md FILE]  # classify every entry/field (a reproduced, b different, c no sample, d not derivable)
tools/camera_db/rederive.py apply [--write]    # correct values the files contradict
tools/camera_db/rederive.py ledger             # which values the files cover (reference/camera-db-ledger.json, untracked)
tools/camera_db/rederive.py refine [--crops FILE] [--write]   # second pass: white levels, image-crop origins (corpus only)
```

`probe` runs `flashraw camera-db probe` over every file twice: with the normal build, and with
a build compiled against an empty database (`FLASHRAW_CAMERA_DB=<json>` makes `build.rs` compile
another database file). Whatever the empty-database decode reports (EXIF/maker notes/DNG tags
and the decoders' own parsing) is what the file states. Per field group:

- **names, identity**: an entry is attested when a file carries its exact Make/Model (headerless
  dumps: the corpus folder naming the camera); the canonical name must equal the one read
  from the file's strings, up to spacing, hyphens, case and "Mark II" written as "MarkII" or "m2".
- **cfa, crop (active area), masked areas, black and white level**: reproduced when the
  empty-database decode equals the stored value. A value the file states (and the database was not used for) that
  differs is replaced by the file's value (the median over the files when each file states its own black level). Without a statement in the file: black from the mean of the declared
  optical-black areas; white from a saturation plateau (at least 64 samples at the maximum, 8x more than at
  the next value, below the container maximum); the plateau replaces a stored white above or below it; masked bands and crops
  from sharp steps in the column/row profiles at the stored boundaries.
- **colour matrix**: only from camera-written DNG files (makers whose cameras write DNG: pentax/ricoh,
  leica, sigma, hasselblad, phones, drones, action and 360 cameras; no `OriginalRawFileName` tag or
  desktop-software marker, and the matrix of the entry the file resolves to). Matrices of other file-embedded colour data (IIQ/MOS
  profiles, Foveon calibration) are reported, not applied: their convention differs from the stored one.
  An embedded-JPEG fit is not precise enough to replace a matrix and is not used.
- **hints**: `filesize` (and the raster size it implies) from the sample's size; decoder-behaviour
  parameters (`load_flags`, `order`, ...) are not stated by files and are kept as stored.
- **orientation**: CameraInfo offsets are checked against CR2 samples; one rotated sample leaves
  many fitting offsets, so they are kept as stored.
- **image_crops**: measured from files (see "Default crops"). `refine` re-checks the origins of the rules against the
  corpus: a rule moves when at least two samples of one sensor window register the camera's JPEG elsewhere (correlation
  >= 0.9, within 2 px of each other), or when a camera-written DNG of the same raw frame states another origin and the
  JPEG registration lies within 3 px of it. `refine` also sets a white level to a clean plateau below the container
  maximum that lies above the stored one (plain mosaic samples, single sensor row, white not stated by the file).

Each entry's `verification` holds the number of sample files it was confirmed against, the largest
sample value observed in them, and notes. Stored values the tool could not decide (a value a
decode actually uses, files that disagree among themselves) are listed as `KEEP` in the audit
and left unchanged.

## Tooling

```sh
flashraw camera-db validate [crates/flashraw-camera-db/data/cameras.json]   # schema + semantic checks (CI)
flashraw camera-db generate                            # load, sort, rewrite (normalised formatting)
flashraw camera-db compare old.json new.json           # added / removed / changed entries
flashraw camera-db stats                               # compiled-in contents
flashraw camera-db coverage                            # entries per make by the kinds of data they carry
flashraw camera-db crops --register FILES...           # camera image areas (JSON lines)
flashraw camera-db probe FILES...                      # what files state + sample statistics + database entry (JSON lines)
```

`flashraw verify --json` reports `active_max` and `active_p9999` (largest and 99.99th
percentile active-area sample) for checking white levels against real data.
