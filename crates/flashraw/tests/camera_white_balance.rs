//! Images the camera white-balanced itself (Sony lossless YCbCr S/M, Nikon sNEF) report
//! `white_balance_applied`, keep the as-shot multipliers, and develop like the full-size
//! Bayer files of the same scene instead of having the balance applied a second time.
//!
//! Runs on the CC0 corpus (`$FLASHRAW_CORPUS` or `<repo>/corpus`); skipped when absent.

use flashraw::{DecodeOptions, Demosaic, FlashRaw, OutputFormat};
use std::path::PathBuf;

fn corpus() -> Option<PathBuf> {
    let c = std::env::var_os("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    c.exists().then_some(c)
}

/// Mean R, G, B of the central 20% of an RGBA8 development.
fn centre_mean(raw: &FlashRaw) -> [f64; 3] {
    let opts = DecodeOptions { output: OutputFormat::Rgba8, demosaic: Demosaic::HalfSize, ..Default::default() };
    let f = raw.decode(opts).unwrap();
    let px = f.as_u8().unwrap();
    let (w, h) = (f.width as usize, f.height as usize);
    let mut s = [0u64; 3];
    let mut n = 0u64;
    for y in h * 2 / 5..h * 3 / 5 {
        for x in w * 2 / 5..w * 3 / 5 {
            let p = &px[(y * w + x) * 4..][..3];
            for c in 0..3 {
                s[c] += p[c] as u64;
            }
            n += 1;
        }
    }
    s.map(|v| v as f64 / n.max(1) as f64)
}

/// `(camera-balanced file, full-size Bayer file of the same scene, tolerance)`: centre
/// means per channel within `tolerance` (relative) for the Sony pairs; R/G and B/G
/// within `tolerance` (absolute) for the Nikon pairs, whose sNEF tone curve differs.
const PAIRS: &[(&str, &str, f64)] = &[
    ("Sony/ILCE-7RM5/7RM5-LosslessCompressedMedium.ARW", "Sony/ILCE-7RM5/7RM5-LosslessCompressedLarge.ARW", 0.1),
    ("Sony/ILCE-7RM5/7RM5-LosslessCompressedSmall.ARW", "Sony/ILCE-7RM5/7RM5-LosslessCompressedLarge.ARW", 0.1),
    ("Sony/ILCE-7RM5/7RM5-S35-LosslessCompressedSmall.ARW", "Sony/ILCE-7RM5/7RM5-S35-LosslessUncompressed.ARW", 0.1),
    ("Nikon/D810/DSC_1888.NEF", "Nikon/D810/DSC_1887.NEF", 0.12),
    ("Nikon/D4S/FU9_3802.NEF", "Nikon/D4S/FU9_3803.NEF", 0.12),
];

#[test]
fn camera_balanced_images_develop_like_full_size() {
    let Some(corpus) = corpus() else {
        eprintln!("camera white balance: corpus not found, skipped");
        return;
    };
    let mut failures = Vec::new();
    for &(small, full, tol) in PAIRS {
        let (ps, pf) = (corpus.join(small), corpus.join(full));
        if !ps.exists() || !pf.exists() {
            eprintln!("missing {small} or {full}");
            continue;
        }
        let rs = FlashRaw::open_path(&ps).unwrap();
        let rf = FlashRaw::open_path(&pf).unwrap();
        assert!(rs.frame_info().white_balance_applied, "{small}: white balance not marked as applied");
        assert!(rs.frame_info().white_balance.is_some(), "{small}: as-shot multipliers kept");
        assert!(!rf.frame_info().white_balance_applied, "{full}");
        let raw = rs.decode(DecodeOptions::default()).unwrap();
        assert!(raw.white_balance_applied && raw.white_balance.is_some(), "{small}: raw frame");
        let (a, b) = (centre_mean(&rs), centre_mean(&rf));
        let ok = if small.starts_with("Sony") {
            (0..3).all(|c| (a[c] - b[c]).abs() <= tol * b[c])
        } else {
            let r = |m: [f64; 3]| (m[0] / m[1], m[2] / m[1]);
            let ((ar, ab), (br, bb)) = (r(a), r(b));
            (ar - br).abs() <= tol && (ab - bb).abs() <= tol
        };
        let line = format!("{small}: {a:.0?} vs {full}: {b:.0?}");
        eprintln!("{line}");
        if !ok {
            failures.push(line);
        }
    }
    assert!(failures.is_empty(), "colour differs from the full-size file:\n{}", failures.join("\n"));
}
