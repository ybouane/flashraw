//! Malformed-input tests on real files (Sony, Olympus and Minolta samples).
//!
//! Corpus files are read from `corpus/` at the workspace root when present (the
//! tests skip files that are missing). Every sample is decoded intact, truncated
//! at several points and with corrupted payload bytes: decoding must return an
//! error or an image, never panic, and truncated payloads must be reported.

use crate::OpenOptions;
use flashraw_core::*;

/// Workspace-relative corpus file, if available.
pub fn corpus(rel: &str) -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus").join(rel);
    std::fs::read(p).ok()
}

/// Open and decode; `Ok(true)` when an image came out.
fn try_decode(data: &[u8]) -> Result<bool> {
    let dec = crate::open(data, &OpenOptions::default())?;
    let prof = Profiler::new();
    let ctx = DecodeContext::new(&prof);
    let img = dec.decode(&ctx)?;
    let info = dec.frame_info();
    assert_eq!((img.width, img.height), (info.width, info.height));
    Ok(true)
}

/// Decode `rel` intact, truncated and corrupted. `payload_from` is a byte offset
/// before which corruption is not applied (headers), as a fraction of the file.
pub fn stress(rel: &str) {
    let Some(data) = corpus(rel) else {
        eprintln!("skipping {rel}: not in corpus");
        return;
    };
    assert!(try_decode(&data).unwrap_or_else(|e| panic!("{rel}: intact file failed: {e}")));
    // Truncation in the middle of the payload must be an error; other cuts must
    // simply not panic (previews may follow the sensor data).
    if let Ok(true) = try_decode(&data[..data.len() / 2]) {
        panic!("{rel}: truncated to half decoded without error");
    }
    for frac in [0.8f64, 0.97, 0.999] {
        let _ = try_decode(&data[..(data.len() as f64 * frac) as usize]);
    }
    // Tiny prefixes: header parsing must fail cleanly.
    for cut in [0usize, 8, 16, 64, 300, 4096] {
        let _ = try_decode(&data[..cut.min(data.len())]);
    }
    // Corruption: flip bytes across the second half of the file (payload area).
    let mut x = 0x9e37_79b9u32;
    for round in 0..6 {
        let mut d = data.clone();
        let n = d.len();
        for _ in 0..(64 << round) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let pos = n / 2 + (x as usize % (n / 2));
            d[pos] ^= (x >> 24) as u8 | 1;
        }
        let _ = try_decode(&d);
    }
    // Corrupt the header area too (IFD values, maker notes).
    for round in 0..4 {
        let mut d = data.clone();
        let n = d.len().min(1 << 20);
        for _ in 0..(16 << round) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let pos = 8 + x as usize % (n - 8);
            d[pos] = (x >> 16) as u8;
        }
        let _ = try_decode(&d);
    }
}

#[test]
fn sony_samples_survive_malformed_input() {
    for rel in [
        "Sony/ILCE-7S/DSC04126.ARW",         // ARW2
        "Sony/ILCE-9M3/DSC00115.ARW",        // uncompressed 14-bit
        "Sony/DSLR-A230/DSC08026.ARW",       // ARW1
        "Sony/DSLR-A100/_DSC0258.ARW",       // ARW1 (A100 layout)
        "Sony/DSLR-A850/RAW_SONY_A850.ARW",  // 12-bit packed
        "Sony/ILCE-9M3/DSC00117.ARW",        // lossless JPEG tiles
        "Sony/ILCE-9M3/DSC00114.ARW",        // lossless YCbCr tiles
        "Sony/DSC-R1/_DSC1477.SR2",          // SR2 (big-endian samples)
        "Sony/DSC-F828/DSC06227.SRF",        // SRF encryption
        "SONY/ILCE-7M5/apsc_compressed.ARW", // ARW6
    ] {
        stress(rel);
    }
}

#[test]
fn olympus_samples_survive_malformed_input() {
    for rel in [
        "Olympus/E-410/PC309398.ORF",            // Olympus compression
        "OM System/OM-1 Mark II/20mp-12bit.orf", // OM System makernote
        "Olympus/C5050Z/RAW_OLYMPUS_C5050Z.ORF", // interlaced 12-bit fields
        "Olympus/E-300/P1252148.ORF",            // 12-bit with pad bytes
        "Olympus/XZ-10/P1240016.ORF",            // 12-bit in 32-bit words
        "Olympus/E-1/E_1__C106743_gredos.ORF",   // 16-bit containers
        "Olympus/E-10/P7198607.ORF",             // big-endian, 10-bit
    ] {
        stress(rel);
    }
}

#[test]
fn minolta_samples_survive_malformed_input() {
    for rel in [
        "MINOLTA/DYNAX 5D/Minolta_Dynax_5D.MRW",    // packed
        "Minolta/DiMAGE 7/RAW_MINOLTA_DIMAGE7.MRW", // unpacked
    ] {
        stress(rel);
    }
    // Headerless dumps are identified by exact size: truncation changes the format.
    for rel in ["Minolta/Dimage Z2/RAW_MINOLTA_DIMAGE_Z2.MRW", "Minolta/RD 175/M001.MDC"] {
        if let Some(d) = corpus(rel) {
            assert!(try_decode(&d).is_ok(), "{rel}");
            let mut c = d.clone();
            for (i, b) in c.iter_mut().enumerate().skip(1000).step_by(97) {
                *b ^= i as u8;
            }
            let _ = try_decode(&c);
        }
    }
}

/// Region decoding (rows / intersecting tiles only) equals cropping a full decode.
#[test]
fn region_decode_matches_full_decode() {
    for rel in [
        "Sony/ILCE-9M3/DSC00117.ARW", // lossless tiles
        "Sony/ILCE-9M3/DSC00114.ARW", // YCbCr tiles
        "Sony/ILCE-7S/DSC04126.ARW",  // ARW2 rows
        "Sony/ILCE-9M3/DSC00115.ARW", // uncompressed rows
        "Sony/ILCE-7RM3/Indoor_Multi-20190712-094102.ARQ",
        "Olympus/E-300/P1252148.ORF",            // padded packed rows
        "MINOLTA/DYNAX 5D/Minolta_Dynax_5D.MRW", // packed rows
    ] {
        let Some(d) = corpus(rel) else { continue };
        let dec = crate::open(&d, &OpenOptions::default()).unwrap();
        let prof = Profiler::new();
        let ctx = DecodeContext::new(&prof);
        let full = dec.decode(&ctx).unwrap();
        let (w, h) = (full.width, full.height);
        for r in [
            Rect::new(0, 0, w.min(17), h.min(9)),
            Rect::new(w / 3, h / 4, w / 5 + 3, h / 7 + 5),
            Rect::new(w.saturating_sub(700), h.saturating_sub(513), 700.min(w), 513.min(h)),
            Rect::new(511, 509, 3, 1030.min(h - 509)),
        ] {
            let got = dec.decode_region(&ctx, r).unwrap();
            let want = full.crop(r).unwrap();
            assert_eq!(got.u16(), want.u16(), "{rel} region {r}");
        }
    }
}

/// The Hasselblad Lunar is a rebadged NEX-7 writing ordinary ARW2 files: the Sony
#[test]
fn hasselblad_lunar_is_an_arw() {
    let Some(d) = corpus("HASSELBLAD/Lunar/DSC00343.ARW") else { return };
    let dec = super::open(&d, &OpenOptions::default()).expect("open");
    let prof = Profiler::new();
    let img = dec.decode(&DecodeContext::new(&prof)).expect("decode");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &v in img.u16() {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    assert_eq!((img.width, img.height), (6048, 4024));
    assert_eq!(format!("{h:016x}"), "82eb880e868b36ea");
}
