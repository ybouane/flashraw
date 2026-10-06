//! `DecodeOptions::preview_fallback`: sensor data FlashRaw cannot decode fails with
//! `UnsupportedCompression` by default; with the option, RGB outputs return the embedded
//! full-size preview, marked `FrameSource::Preview` with a `PreviewFallback` warning.
//!
//! A synthetic Nikon High Efficiency NEF (JPEG XS payload, full-size RGB preview in IFD0)
//! covers the rules; the corpus High Efficiency files are checked when present.

use flashraw::{CorruptReason, DecodeOptions, Error, FlashRaw, FrameSource, Orientation, OutputFormat, RawPixelData};

const W: u32 = 64;
const H: u32 = 48;

/// Little-endian IFD writer: `(tag, type, count, value bytes)`; values over 4 bytes go
/// to `extra` at `extra_base`.
fn ifd(entries: &[(u16, u16, u32, Vec<u8>)], extra_base: usize, next: u32) -> (Vec<u8>, Vec<u8>) {
    let mut e = entries.to_vec();
    e.sort_by_key(|x| x.0);
    let mut out = (e.len() as u16).to_le_bytes().to_vec();
    let mut extra = vec![];
    let dir_len = 2 + e.len() * 12 + 4;
    for (tag, typ, count, v) in &e {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        if v.len() <= 4 {
            let mut v = v.clone();
            v.resize(4, 0);
            out.extend(v);
        } else {
            out.extend(((extra_base + dir_len + extra.len()) as u32).to_le_bytes());
            extra.extend(v);
            if extra.len() % 2 == 1 {
                extra.push(0);
            }
        }
    }
    out.extend(next.to_le_bytes());
    (out, extra)
}

fn short(v: u16) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
fn long(v: u32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// A NEF whose sensor data is Nikon High Efficiency (unsupported) and whose IFD0 holds
/// a `pw`x`ph` RGB preview (a horizontal ramp in red, constant green 100, blue 200) with
/// EXIF orientation `orientation`.
pub fn high_efficiency_nef(pw: u32, ph: u32, orientation: u16) -> Vec<u8> {
    let preview: Vec<u8> = (0..ph)
        .flat_map(|_| (0..pw).flat_map(move |x| [(x * 255 / pw.max(2).saturating_sub(1)) as u8, 100, 200]))
        .collect();
    let payload: Vec<u8> = [0xff, 0x10, 0xff, 0x50].into_iter().chain((0..96u8).map(|i| i.wrapping_mul(37))).collect();
    // Layout: header (8) | IFD0 + extra | raw IFD + extra | preview | payload.
    let ifd0_at = 8usize;
    let make = b"NIKON CORPORATION\0".to_vec();
    let model = b"NIKON Z 8\0".to_vec();
    let ifd0_entries = |raw_at: u32, preview_at: u32| {
        vec![
            (0x00FE, 4, 1, long(1)),
            (0x0100, 4, 1, long(pw)),
            (0x0101, 4, 1, long(ph)),
            (0x0102, 3, 3, [8u16, 8, 8].iter().flat_map(|v| v.to_le_bytes()).collect()),
            (0x0103, 3, 1, short(1)),
            (0x0106, 3, 1, short(2)),
            (0x010F, 2, make.len() as u32, make.clone()),
            (0x0110, 2, model.len() as u32, model.clone()),
            (0x0111, 4, 1, long(preview_at)),
            (0x0112, 3, 1, short(orientation)),
            (0x0115, 3, 1, short(3)),
            (0x0116, 4, 1, long(ph)),
            (0x0117, 4, 1, long(pw * ph * 3)),
            (0x014A, 4, 1, long(raw_at)),
        ]
    };
    let raw_entries = |payload_at: u32| {
        vec![
            (0x00FE, 4, 1, long(0)),
            (0x0100, 4, 1, long(W)),
            (0x0101, 4, 1, long(H)),
            (0x0102, 3, 1, short(14)),
            (0x0103, 3, 1, short(34713)),
            (0x0106, 3, 1, short(32803)),
            (0x0111, 4, 1, long(payload_at)),
            (0x0115, 3, 1, short(1)),
            (0x0116, 4, 1, long(H)),
            (0x0117, 4, 1, long(payload.len() as u32)),
        ]
    };
    // Two passes: sizes first, then the real offsets.
    let (d0, x0) = ifd(&ifd0_entries(0, 0), ifd0_at, 0);
    let raw_at = ifd0_at + d0.len() + x0.len();
    let (d1, x1) = ifd(&raw_entries(0), raw_at, 0);
    let preview_at = raw_at + d1.len() + x1.len();
    let payload_at = preview_at + preview.len();
    let (d0, x0) = ifd(&ifd0_entries(raw_at as u32, preview_at as u32), ifd0_at, 0);
    let (d1, x1) = ifd(&raw_entries(payload_at as u32), raw_at, 0);
    let mut out = b"II*\0".to_vec();
    out.extend(long(ifd0_at as u32));
    for part in [d0, x0, d1, x1, preview, payload] {
        out.extend(part);
    }
    out
}

/// The bindings' tests (npm, Python, C) use this file as `npm/tests/fixtures/nikon-he-synthetic.nef`;
/// `FLASHRAW_UPDATE_FIXTURES=1` rewrites it.
#[test]
fn bindings_fixture_is_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures/nikon-he-synthetic.nef");
    let want = high_efficiency_nef(W, H, 1);
    if std::env::var_os("FLASHRAW_UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    assert!(std::fs::read(&path).is_ok_and(|have| have == want), "{} is stale", path.display());
}

fn fallback(output: OutputFormat) -> DecodeOptions {
    DecodeOptions { output, preview_fallback: true, ..Default::default() }
}

#[test]
fn unsupported_sensor_data_errors_by_default() {
    let data = high_efficiency_nef(W, H, 1);
    let raw = FlashRaw::open(&data[..]).unwrap();
    for output in [OutputFormat::RawU16, OutputFormat::Rgba8, OutputFormat::LinearRgbF32] {
        let e = raw.decode(DecodeOptions { output, ..Default::default() }).unwrap_err();
        assert!(matches!(e, Error::UnsupportedCompression { .. }), "{output}: {e}");
    }
}

#[test]
fn rgba8_returns_the_marked_preview() {
    let data = high_efficiency_nef(W, H, 1);
    let raw = FlashRaw::open(&data[..]).unwrap();
    let f = raw.decode(fallback(OutputFormat::Rgba8)).unwrap();
    assert_eq!(f.source, FrameSource::Preview);
    assert_eq!((f.width, f.height, f.cpp), (W, H, 4));
    assert!(f.is_incomplete());
    assert_eq!(f.warnings.len(), 1);
    let w = &f.warnings[0];
    assert_eq!(w.kind(), "preview-fallback");
    assert!(matches!(&w.reason, CorruptReason::PreviewFallback { message } if message.contains("high-efficiency")));
    assert!(w.to_string().contains("embedded full-size preview"), "{w}");
    assert_eq!((f.white_balance, f.cfa, f.bits_per_sample), (None, None, 8));
    let px = f.as_u8().unwrap();
    assert_eq!(&px[..4], &[0, 100, 200, 255]);
    let last = ((W - 1) * 4) as usize;
    assert_eq!(&px[last..last + 4], &[255, 100, 200, 255]);
}

#[test]
fn rgba8_applies_the_preview_orientation() {
    // Rotate 90 CW: the stored top row becomes the right column.
    let data = high_efficiency_nef(W, H, 6);
    let raw = FlashRaw::open(&data[..]).unwrap();
    let f = raw.decode(fallback(OutputFormat::Rgba8)).unwrap();
    assert_eq!((f.width, f.height, f.orientation), (H, W, Orientation::Normal));
    let px = f.as_u8().unwrap();
    // Output (H-1, 0) is stored (0, 0) (red 0); output (H-1, W-1) is stored (W-1, 0).
    let at = |x: u32, y: u32| px[((y * H + x) * 4) as usize];
    assert_eq!((at(H - 1, 0), at(H - 1, W - 1)), (0, 255));
}

#[test]
fn linear_rgb_returns_linearised_preview() {
    let data = high_efficiency_nef(W, H, 6);
    let raw = FlashRaw::open(&data[..]).unwrap();
    let f = raw.decode(fallback(OutputFormat::LinearRgbF32)).unwrap();
    assert_eq!((f.width, f.height, f.cpp, f.source), (W, H, 3, FrameSource::Preview));
    // Not oriented (like every linear-rgb frame): the orientation is still to apply.
    assert_eq!(f.orientation, Orientation::Rotate90);
    let RawPixelData::F32(v) = &f.data else { panic!("f32 data") };
    assert_eq!(v[0], 0.0);
    assert!((v[1] - 0.127).abs() < 0.002, "sRGB 100 → linear {}", v[1]);
    assert!((v[2] - 0.578).abs() < 0.002, "sRGB 200 → linear {}", v[2]);
}

#[test]
fn no_fallback_for_raw_outputs_regions_or_small_previews() {
    let data = high_efficiency_nef(W, H, 1);
    let raw = FlashRaw::open(&data[..]).unwrap();
    for output in [OutputFormat::RawU16, OutputFormat::RawF32, OutputFormat::NormalizedF32] {
        assert!(matches!(raw.decode(fallback(output)), Err(Error::UnsupportedCompression { .. })));
    }
    let region = DecodeOptions { region: Some(flashraw::Rect::new(0, 0, 8, 8)), ..fallback(OutputFormat::Rgba8) };
    assert!(matches!(raw.decode(region), Err(Error::UnsupportedCompression { .. })));
    // A thumbnail is not a stand-in for the image.
    let small = high_efficiency_nef(W / 4, H / 4, 1);
    let raw = FlashRaw::open(&small[..]).unwrap();
    assert!(matches!(raw.decode(fallback(OutputFormat::Rgba8)), Err(Error::UnsupportedCompression { .. })));
}

#[test]
fn decodable_files_never_report_the_preview_source() {
    // A complete decode is unaffected by the option.
    let data = high_efficiency_nef(W, H, 1);
    let raw = FlashRaw::open(&data[..]).unwrap();
    let e = raw.decode(DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() });
    assert!(e.is_err());
    let Some(corpus) = corpus() else { return };
    let nef = corpus.join("Nikon/D3/JD1_8207.NEF");
    let Ok(raw) = FlashRaw::open_path(&nef) else { return };
    let f = raw.decode(fallback(OutputFormat::Rgba8)).unwrap();
    assert_eq!(f.source, FrameSource::Sensor);
    assert!(f.warnings.is_empty());
}

fn corpus() -> Option<std::path::PathBuf> {
    let p = std::env::var("FLASHRAW_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    p.exists().then_some(p)
}

/// Every NEF under `dir`, recursively.
fn nefs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            nefs(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nef")) {
            out.push(p);
        }
    }
}

/// Every corpus High Efficiency NEF: an error by default, the full-size preview with the
/// option.
#[test]
fn corpus_high_efficiency_nefs() {
    let Some(corpus) = corpus() else {
        eprintln!("corpus not present; skipping");
        return;
    };
    let mut files = vec![];
    for make in ["Nikon", "NIKON CORPORATION"] {
        nefs(&corpus.join(make), &mut files);
    }
    let mut checked = 0;
    for path in files {
        let Ok(raw) = FlashRaw::open_path(&path) else { continue };
        if raw.frame_info().compression != flashraw::Compression::NikonHighEfficiency {
            continue;
        }
        let f = path.display();
        let e = raw.decode(DecodeOptions { output: OutputFormat::Rgba8, ..Default::default() }).unwrap_err();
        assert!(matches!(e, Error::UnsupportedCompression { .. }), "{f}: {e}");
        let fr = raw.decode(fallback(OutputFormat::Rgba8)).unwrap_or_else(|e| panic!("{f}: {e}"));
        assert_eq!(fr.source, FrameSource::Preview, "{f}");
        let crop = raw.frame_info().crop;
        let (lo, hi) = (fr.width.min(fr.height) as f64, fr.width.max(fr.height) as f64);
        let (clo, chi) = (crop.width.min(crop.height) as f64, crop.width.max(crop.height) as f64);
        assert!(lo >= clo * 0.9 && hi >= chi * 0.9, "{f}: {}x{} vs crop {crop}", fr.width, fr.height);
        // A real picture, not a blank frame.
        let px = fr.as_u8().unwrap();
        let mean = px.iter().step_by(4 * 97).map(|&v| v as f64).sum::<f64>() / (px.len() / (4 * 97)) as f64;
        assert!(mean > 5.0 && mean < 250.0, "{f}: mean {mean}");
        checked += 1;
    }
    eprintln!("high efficiency fallback: {checked} files");
    assert!(checked > 0, "the corpus has High Efficiency NEFs");
}
