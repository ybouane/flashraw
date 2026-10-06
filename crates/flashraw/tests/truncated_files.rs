//! Truncated real files must never decode a thumbnail or preview as the sensor image.
//!
//! Multi-IFD containers (NEF, CR2, PEF, ARW/SR2, DNG, ORF, RW2, ERF/MEF/3FR, IIQ, SRW,
//! TIFF/EP, Kodak, ...) store small previews next to the raw image. When a file is cut
//! short, the raw IFD or its data may be missing while a preview IFD is intact. Every
//! cut must either report the true sensor geometry (with truncation warnings under
//! `Recover`, a structured `CorruptFile` under `Strict`) or fail with a clear error.
//!
//! Corpus-backed (`FLASHRAW_CORPUS`, skipped when absent): one file per extension and
//! make by default; `FLASHRAW_TRUNCATION_ALL=1` checks one file per camera directory.
//! Synthetic cases cover the same rule without the corpus.

use flashraw::{CorruptionPolicy, DecodeOptions, Error, FlashRaw, OutputFormat};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn corpus() -> Option<PathBuf> {
    let p = std::env::var("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    p.exists().then_some(p)
}

fn opts(policy: CorruptionPolicy) -> DecodeOptions {
    DecodeOptions { output: OutputFormat::RawU16, corruption_policy: policy, ..Default::default() }
}

/// Geometry and sensor-data extent of a complete file.
struct Reference {
    width: u32,
    height: u32,
    /// Start of the first sensor-data unit.
    data_start: usize,
    /// End of the last sensor-data unit (as declared: may include trailing bytes that no
    /// sample depends on).
    data_end: usize,
    /// Hash of the complete decode.
    hash: u64,
}

fn hash(f: &flashraw::RawFrame) -> u64 {
    let bytes: Vec<u8> = match &f.data {
        flashraw::RawPixelData::U16(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        flashraw::RawPixelData::F32(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        flashraw::RawPixelData::U8(v) => v.clone(),
        #[allow(unreachable_patterns)] // GPU-resident data (feature `gpu`)
        _ => vec![],
    };
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn reference(data: &[u8]) -> Option<Reference> {
    let raw = FlashRaw::open(data).ok()?;
    let full = raw.decode(opts(CorruptionPolicy::Strict)).ok()?;
    let info = raw.frame_info();
    let units = raw.units();
    let data_start = units.iter().map(|u| u.offset as usize).filter(|&o| o > 0).min()?;
    let data_end = units.iter().map(|u| (u.offset + u.length) as usize).max()?.min(data.len());
    Some(Reference { width: info.width, height: info.height, data_start, data_end, hash: hash(&full) })
}

/// Cut points: fractions of the file plus the bytes around the start and end of the
/// sensor data.
fn cuts(len: usize, r: &Reference) -> Vec<usize> {
    let mut c: Vec<usize> = [0.001, 0.01, 0.05, 0.1, 0.3, 0.5, 0.9, 0.99]
        .iter()
        .map(|f| (len as f64 * f) as usize)
        .chain([
            r.data_start.saturating_sub(1),
            r.data_start,
            r.data_start + 1,
            r.data_start + (r.data_end - r.data_start) / 2,
            r.data_end.saturating_sub(1),
        ])
        .filter(|&c| c > 8 && c < len)
        .collect();
    c.sort_unstable();
    c.dedup();
    c
}

/// Check one truncated copy; `Err` describes a violation.
fn check_cut(data: &[u8], r: &Reference) -> Result<(), String> {
    let raw = match FlashRaw::open(data) {
        Ok(raw) => raw,
        Err(_) => return Ok(()), // a clear error at open
    };
    let info = raw.frame_info();
    if (info.width, info.height) != (r.width, r.height) {
        return Err(format!("opened as {}x{} (sensor {}x{})", info.width, info.height, r.width, r.height));
    }
    let recovered = raw.decode(opts(CorruptionPolicy::Recover));
    let strict = raw.decode(opts(CorruptionPolicy::Strict));
    match &recovered {
        Ok(f) => {
            if (f.width, f.height) != (r.width, r.height) {
                return Err(format!("decoded {}x{} (sensor {}x{})", f.width, f.height, r.width, r.height));
            }
            // A cut that decodes without a warning must not have lost anything (the
            // declared extent may include bytes no sample depends on).
            if !f.is_incomplete() && hash(f) != r.hash {
                return Err(format!(
                    "sensor data ends at {} but the cut at {} decoded silently",
                    r.data_end,
                    data.len()
                ));
            }
            if f.is_incomplete() {
                match &strict {
                    Err(Error::CorruptFile { .. }) => {}
                    Err(e) => return Err(format!("recover warned but strict failed with {} ({e})", e.kind())),
                    Ok(_) => return Err("recover warned but strict succeeded".into()),
                }
            }
        }
        Err(_) => {
            if strict.is_ok() {
                return Err("recover failed but strict succeeded".into());
            }
        }
    }
    Ok(())
}

fn check_file(path: &Path) -> Vec<String> {
    let Ok(data) = std::fs::read(path) else { return vec![] };
    let Some(r) = reference(&data) else { return vec![] };
    cuts(data.len(), &r)
        .into_iter()
        .filter_map(|c| check_cut(&data[..c], &r).err().map(|e| format!("{} cut at {c}: {e}", path.display())))
        .collect()
}

fn is_raw_candidate(p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    !matches!(ext.as_str(), "" | "jpg" | "jpeg" | "txt" | "md" | "xmp" | "json" | "tsv" | "png" | "pp3" | "html")
}

/// One file per (extension, make) by default; one per camera directory with
/// `FLASHRAW_TRUNCATION_ALL`.
fn sample(root: &Path) -> Vec<PathBuf> {
    let all = std::env::var_os("FLASHRAW_TRUNCATION_ALL").is_some();
    let mut picked: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&d).into_iter().flatten().flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !is_raw_candidate(&p) || std::fs::metadata(&p).map(|m| m.len()).unwrap_or(u64::MAX) > 80_000_000 {
                continue;
            }
            let rel = p.strip_prefix(root).unwrap_or(&p);
            let make = rel.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase());
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let key = if all {
                p.parent().map(|q| q.display().to_string()).unwrap_or_default()
            } else {
                format!("{ext}/{}", make.unwrap_or_default())
            };
            picked.entry(key).or_insert(p);
        }
    }
    picked.into_values().collect()
}

#[test]
fn truncated_corpus_files_never_decode_previews() {
    let Some(root) = corpus() else {
        eprintln!("corpus not present; skipping");
        return;
    };
    let files = sample(&root);
    // Files are checked on a few threads (each decode is parallel as well).
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().div_ceil(4));
    let mut failures: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut out = vec![];
                    while let Some(p) = files.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                        out.extend(check_file(p));
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    failures.sort();
    eprintln!("truncation: {} files checked", files.len());
    assert!(failures.is_empty(), "{} violations:\n{}", failures.len(), failures.join("\n"));
}

// --- Synthetic files: a thumbnail IFD0 and the raw image in a SubIFD -----------------------

/// Little-endian IFD with `(tag, type, count, value bytes)` entries; values over 4 bytes go
/// after the directory. Returns the directory followed by its values.
fn ifd(entries: &[(u16, u16, u32, Vec<u8>)], at: usize) -> Vec<u8> {
    let mut e = entries.to_vec();
    e.sort_by_key(|x| x.0);
    let mut out = (e.len() as u16).to_le_bytes().to_vec();
    let mut extra: Vec<u8> = vec![];
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
            out.extend(((at + dir_len + extra.len()) as u32).to_le_bytes());
            extra.extend(v);
        }
    }
    out.extend(0u32.to_le_bytes());
    out.extend(extra);
    out
}

fn short(v: u16) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
fn long(v: u32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

const SW: u32 = 320;
const SH: u32 = 240;

/// header | IFD0 (160x120 RGB thumbnail, SubIFDs -> raw) | thumbnail | raw IFD | payload.
/// The raw image is SWxSH, 16-bit uncompressed CFA. Returns the file and the offsets of
/// the raw IFD and the payload.
fn thumbnail_then_raw(make: &str, model: &str, dng: bool) -> (Vec<u8>, usize, usize) {
    let thumb: Vec<u8> = (0..160 * 120 * 3).map(|i| (i % 251) as u8).collect();
    let payload: Vec<u8> = (0..SW * SH).flat_map(|i| ((i * 7 % 4000) as u16 + 64).to_le_bytes()).collect();
    let text = |s: &str| {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    };
    let ifd0 = |raw_at: usize, thumb_at: usize| {
        let mut e = vec![
            (0x00FE, 4, 1, long(1)),
            (0x0100, 4, 1, long(160)),
            (0x0101, 4, 1, long(120)),
            (0x0102, 3, 3, [8u16, 8, 8].iter().flat_map(|v| v.to_le_bytes()).collect()),
            (0x0103, 3, 1, short(1)),
            (0x0106, 3, 1, short(2)),
            (0x010F, 2, make.len() as u32 + 1, text(make)),
            (0x0110, 2, model.len() as u32 + 1, text(model)),
            (0x0111, 4, 1, long(thumb_at as u32)),
            (0x0115, 3, 1, short(3)),
            (0x0116, 4, 1, long(120)),
            (0x0117, 4, 1, long(thumb.len() as u32)),
            (0x014A, 4, 1, long(raw_at as u32)),
        ];
        if dng {
            e.push((0xC612, 1, 4, vec![1, 4, 0, 0]));
        }
        e
    };
    let raw = |payload_at: usize| {
        vec![
            (0x00FE, 4, 1, long(0)),
            (0x0100, 4, 1, long(SW)),
            (0x0101, 4, 1, long(SH)),
            (0x0102, 3, 1, short(16)),
            (0x0103, 3, 1, short(1)),
            (0x0106, 3, 1, short(32803)),
            (0x0111, 4, 1, long(payload_at as u32)),
            (0x0115, 3, 1, short(1)),
            (0x0116, 4, 1, long(SH)),
            (0x0117, 4, 1, long(payload.len() as u32)),
            (0x828D, 3, 2, [2u16, 2].iter().flat_map(|v| v.to_le_bytes()).collect()),
            (0x828E, 1, 4, vec![0, 1, 1, 2]),
            (0xC61D, 4, 1, long(65535)),
        ]
    };
    let ifd0_len = ifd(&ifd0(0, 0), 8).len();
    let thumb_at = 8 + ifd0_len;
    let raw_at = thumb_at + thumb.len();
    let raw_len = ifd(&raw(0), raw_at).len();
    let payload_at = raw_at + raw_len;
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend(ifd(&ifd0(raw_at, thumb_at), 8));
    out.extend(&thumb);
    out.extend(ifd(&raw(payload_at), raw_at));
    out.extend(&payload);
    (out, raw_at, payload_at)
}

fn check_synthetic(make: &str, model: &str, dng: bool) {
    let (data, raw_at, payload_at) = thumbnail_then_raw(make, model, dng);
    let what = format!("{make} {model}");
    let full = FlashRaw::open(&data[..]).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert_eq!((full.frame_info().width, full.frame_info().height), (SW, SH), "{what}");
    let f = full.decode(opts(CorruptionPolicy::Strict)).unwrap();
    assert!(!f.is_incomplete());

    // Cut before (or inside) the raw IFD: a clear error, never the 160x120 thumbnail.
    for cut in [raw_at - 100, raw_at, raw_at + 1, raw_at + 20] {
        match FlashRaw::open(&data[..cut]) {
            Err(e) => assert_eq!(e.kind(), "CorruptFile", "{what} cut at {cut}: {e}"),
            Ok(raw) => panic!("{what} cut at {cut}: opened as {}x{}", raw.frame_info().width, raw.frame_info().height),
        }
    }
    // Cut in the sensor data: the sensor geometry, warnings under recover, a structured
    // truncation under strict.
    for cut in [payload_at, payload_at + 1, payload_at + 1000, data.len() - 1] {
        let raw = FlashRaw::open(&data[..cut]).unwrap_or_else(|e| panic!("{what} cut at {cut}: {e}"));
        assert_eq!((raw.frame_info().width, raw.frame_info().height), (SW, SH), "{what} cut at {cut}");
        let f = raw.decode(opts(CorruptionPolicy::Recover)).unwrap();
        assert_eq!((f.width, f.height), (SW, SH));
        assert!(f.is_incomplete(), "{what} cut at {cut}");
        let e = raw.decode(opts(CorruptionPolicy::Strict)).unwrap_err();
        assert!(e.truncation().is_some(), "{what} cut at {cut}: {e}");
    }
}

#[test]
fn synthetic_nef_truncated_before_raw_ifd() {
    check_synthetic("NIKON CORPORATION", "NIKON D850", false);
}

#[test]
fn synthetic_dng_truncated_before_raw_ifd() {
    check_synthetic("Test", "Camera", true);
}
