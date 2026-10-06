//! Default crops on real files: the crop is the camera's own image area (its JPEG size,
//! the size the file records, or the vendor's crop field), it lies inside the active
//! area, metadata reports it, and RGB outputs have its size (rotated by orientation).
//!
//! Runs when the CC0 corpus is present (`$FLASHRAW_CORPUS` or `<repo>/corpus`, fetched
//! with `tools/corpus/fetch.py`); skipped otherwise.

use flashraw::{DecodeOptions, Demosaic, FlashRaw, OutputFormat, Rect};
use std::path::PathBuf;

/// `(corpus path, crop x, y, width, height, crop source)`.
type Row = (&'static str, u32, u32, u32, u32, &'static str);

const CORPUS: &[Row] = &[
    // Nikon: the embedded full-size JPEG gives the size; the position is measured.
    ("Nikon/D850/Nikon-D850-14bit-compressed.NEF", 16, 8, 8256, 5504, "preview"),
    ("Nikon/D850/Nikon-D850-12bit-uncompressed.NEF", 16, 8, 8256, 5504, "preview"),
    ("Nikon/Z 9/Nikon_-_NIKON_Z_9_-_14bit_compressed_(Lossless).NEF", 12, 8, 8256, 5504, "preview"),
    ("Nikon/Z 6/DSC_0750.NEF", 8, 8, 6048, 4024, "preview"),
    ("Nikon/D810/DSC_1882.NEF", 12, 8, 7360, 4912, "preview"),
    // Canon: SensorInfo's valid-image rectangle (crop modes included), AspectInfo.
    ("Canon/EOS R6/Canon_EOS_R6_RAW_ISO_100_nocrop_nodual.CR3", 84, 50, 5472, 3648, "file"),
    ("Canon/EOS R6/Canon_EOS_R6_RAW_ISO_100_crop_nodual.CR3", 156, 108, 3408, 2272, ""),
    ("Canon/EOS R5/Canon_EOS_R5_RAW_ISO_100_nocrop_nodual.CR3", 144, 112, 8192, 5464, ""),
    ("Canon/EOS R5/Canon_EOS_R5_CRAW_ISO_100_crop_dual.CR3", 144, 108, 5088, 3392, ""),
    ("Canon/EOS 5D Mark IV/B13A0729.CR2", 148, 54, 6720, 4480, "file"),
    ("Canon/EOS 250D/Canon250D_RAW-1:1.CR3", 1276, 48, 4000, 4000, "file"),
    ("Canon/EOS 250D/Canon250D_RAW-4:3.CR3", 612, 48, 5328, 4000, "file"),
    ("Canon/EOS 40D/_MG_0153.CR2", 40, 22, 3888, 2592, "file"),
    // Sony: DefaultCrop (newer bodies), FullImageSize (older bodies).
    ("Sony/ILCE-7M4/ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW", 12, 8, 7008, 4672, "file"),
    // Fujifilm: the RAF crop.
    ("Fujifilm/X-T5/DSCF0021.RAF", 12, 21, 7728, 5152, "file"),
];

fn corpus() -> Option<PathBuf> {
    let c = std::env::var("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    c.exists().then_some(c)
}

#[test]
fn corpus_default_crops() {
    let Some(corpus) = corpus() else {
        eprintln!("corpus not found; skipping (see tools/corpus/fetch.py)");
        return;
    };
    let mut failures = Vec::new();
    let mut checked = 0;
    for &(rel, x, y, w, h, source) in CORPUS {
        let p = corpus.join(rel);
        if !p.exists() {
            eprintln!("missing {rel}");
            continue;
        }
        let raw = FlashRaw::open_path(&p).unwrap_or_else(|e| panic!("{rel}: {e}"));
        let info = raw.frame_info();
        let m = raw.metadata();
        checked += 1;
        if info.crop.width != w || info.crop.height != h || (info.crop.x, info.crop.y) != (x, y) {
            failures.push(format!("{rel}: crop {} != {x},{y},{w}x{h}", info.crop));
        }
        if info.crop.intersect(&info.active_area) != Some(info.crop) {
            failures.push(format!("{rel}: crop {} outside active area {}", info.crop, info.active_area));
        }
        if (m.width, m.height) != (w, h) {
            failures.push(format!("{rel}: metadata {}x{} != {w}x{h}", m.width, m.height));
        }
        if m.extra(flashraw::EXTRA_CROP_SOURCE).unwrap_or("") != source {
            failures.push(format!("{rel}: crop source {:?} != {source:?}", m.extra(flashraw::EXTRA_CROP_SOURCE)));
        }
    }
    assert!(failures.is_empty(), "{checked} files checked:\n{}", failures.join("\n"));
}

/// RGB outputs are cropped to the camera's image area and rotated for display; raw
/// outputs keep the full buffer and report the crop.
#[test]
fn corpus_outputs_follow_crop() {
    let Some(corpus) = corpus() else { return };
    for rel in ["Nikon/D850/Nikon-D850-14bit-compressed.NEF", "Canon/EOS R6/Canon_EOS_R6_RAW_ISO_100_nocrop_nodual.CR3"]
    {
        let p = corpus.join(rel);
        if !p.exists() {
            continue;
        }
        let raw = FlashRaw::open_path(&p).unwrap();
        let info = raw.frame_info().clone();
        let jpeg = raw
            .previews()
            .iter()
            .filter(|p| p.kind == flashraw::PreviewKind::FullSize)
            .map(|p| (p.width, p.height))
            .max_by_key(|&(w, h)| w as u64 * h as u64)
            .unwrap();
        let opts = DecodeOptions { output: OutputFormat::Rgba8, demosaic: Demosaic::HalfSize, ..Default::default() };
        let f = raw.decode(opts).unwrap();
        let swap = raw.metadata().orientation.exif() >= 5;
        let (w, h) = if swap { (info.crop.height, info.crop.width) } else { (info.crop.width, info.crop.height) };
        assert_eq!((f.width, f.height), (w / 2, h / 2), "{rel}: half-size RGBA");
        assert_eq!((info.crop.width, info.crop.height), jpeg, "{rel}: crop vs the camera's JPEG");
        let f = raw.decode(DecodeOptions::default()).unwrap();
        assert_eq!((f.width, f.height), (info.width, info.height), "{rel}: raw output is the full buffer");
        assert_eq!(f.crop, info.crop, "{rel}: raw output reports the crop");
        assert_ne!(f.crop, Rect::full(info.width, info.height));
    }
}
