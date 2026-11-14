//! Orientation records (standard tag, vendor record, Auto Rotate setting), the resolved
//! orientation and per-preview orientation on real files.
//!
//! Two sets, each skipped cleanly when absent:
//! - the CC0 corpus (`$FLASHRAW_CORPUS` or `<repo>/corpus`, fetched by CI with
//!   `tools/corpus/fetch.py`);
//! - a local collection of files with disagreeing records (`$FLASHRAW_ORIENTATION_SAMPLES`,
//!   a directory laid out as listed in `SAMPLES`; these files are not redistributable).

use flashraw::{FlashRaw, Orientation, OrientationSource};
use std::path::{Path, PathBuf};

/// `(relative path, exif, maker_note, auto_rotate, resolved, source, preview orientations)`;
/// orientations as EXIF codes, `None` = not recorded / invalid. An empty preview list
/// is not checked.
type Row = (&'static str, Option<u32>, Option<u32>, Option<bool>, u32, OrientationSource, &'static [u32]);

use OrientationSource::{Default as Dflt, Exif, MakerNote};

/// CC0 corpus files: records agree, or the vendor record is absent.
const CORPUS: &[Row] = &[
    // Canon: CameraInfo sensor field agrees with EXIF (fixed-offset and record-scan layouts).
    ("Canon/EOS Digital Rebel XS/RAW_CANON_1000D.CR2", Some(8), Some(8), None, 8, Exif, &[8, 8]),
    ("Canon/EOS 40D/_MG_0153.CR2", Some(1), Some(1), None, 1, Exif, &[1, 1]),
    ("Canon/EOS Rebel T3i/IMG_0126.CR2", Some(1), Some(1), None, 1, Exif, &[1, 1]),
    ("Canon/EOS 600D/_MG_2587.CR2", Some(1), Some(1), None, 1, Exif, &[]),
    ("Canon/EOS 100D/IMG_3721.CR2", Some(1), Some(1), None, 1, Exif, &[]),
    ("Canon/EOS-1D Mark III/_K5R7350.CR2", Some(1), Some(1), None, 1, Exif, &[]),
    // EOS 6D: Auto Rotate setting recorded (on).
    ("Canon/EOS 6D/IMG_0001.CR2", Some(1), Some(1), Some(true), 1, Exif, &[1, 1]),
    ("Canon/EOS 6D/EOS_6D_sRAW.CR2", Some(1), Some(1), Some(true), 1, Exif, &[]),
    // Nikon without an edit-record rotation.
    ("Nikon/D70/20170902_0047.NEF", Some(1), None, None, 1, Exif, &[1, 1, 1]),
    ("Nikon/E5700/20080115-DSCN1758.NEF", Some(1), None, None, 1, Exif, &[1, 1]),
    // Kodak EasyShare / P-series: the embedded camera JPEG says Normal too.
    ("Kodak/EasyShare Z990/100_0891.KDC", Some(1), Some(1), None, 1, MakerNote, &[1]),
    ("Kodak/P880/100_3710.KDC", Some(1), Some(1), None, 1, MakerNote, &[1]),
];

/// Files whose standard tag and vendor record disagree (paths relative to
/// `$FLASHRAW_ORIENTATION_SAMPLES`).
const SAMPLES: &[Row] = &[
    // Nikon: rotation saved by Nikon's software in the edit record; the previews were
    // re-rendered rotated (portrait), so they need no rotation.
    ("datasets/mei/nef/Nikon_D70.nef", Some(1), Some(6), None, 6, MakerNote, &[1, 1, 1]),
    ("web/rawsamples.ch/nikon/e5700/RAW_NIKON_E5700_SRGB.NEF", Some(1), Some(8), None, 8, MakerNote, &[1, 1]),
    // Kodak EasyShare: the camera's orientation sensor is in the embedded JPEG's EXIF,
    // which also describes that (landscape) preview.
    ("ir/z980/Y102_0021.KDC", Some(1), Some(8), None, 8, MakerNote, &[8]),
    ("ir/z980/Z980hSLI0064.KDC", Some(1), Some(1), None, 1, MakerNote, &[1]),
    // Kodak Ektra (Android) DNGs write Orientation 0: invalid, shown as stored.
    ("web/photographyblog/reviews/kodak_ektra/sample_images/kodak_ektra_01.dng", None, None, None, 1, Dflt, &[]),
    ("web/photographyblog/reviews/kodak_ektra/sample_images/kodak_ektra_06.dng", None, None, None, 1, Dflt, &[]),
    ("web/photographyblog/reviews/kodak_ektra/sample_images/kodak_ektra_12.dng", None, None, None, 1, Dflt, &[]),
    // EOS 40D / EOS-1D Mark III write Orientation 0; CameraInfo says horizontal.
    ("ir/e40d/E40DhMULTII0100.CR2", None, Some(1), None, 1, MakerNote, &[1, 1]),
    ("ir/e40d/E40DhMULTII0200.CR2", None, Some(1), None, 1, MakerNote, &[]),
    ("ir/e40d/E40DhMULTII0400.CR2", None, Some(1), None, 1, MakerNote, &[]),
    ("ir/e40d/E40DhMULTII0800.CR2", None, Some(1), None, 1, MakerNote, &[]),
    ("ir/e40d/E40DhSLI0100_SRAW.CR2", None, Some(1), None, 1, MakerNote, &[]),
    ("ir/e1dmk3/E1DMK3hMULTII0050.CR2", None, Some(1), None, 1, MakerNote, &[]),
    ("ir/e1dmk3/E1DMK3hMULTII0100.CR2", None, Some(1), None, 1, MakerNote, &[]),
    ("ir/e1dmk3/E1DMK3hMULTII0200.CR2", None, Some(1), None, 1, MakerNote, &[]),
    ("ir/e1dmk3/E1DMK3hMULTI_ADOBE.CR2", None, Some(1), None, 1, MakerNote, &[]),
    // EOS 6D with Auto Rotate off: the camera deliberately left the image unrotated.
    ("ir/canon-6d/E6DOUTBAP0.CR2", Some(1), Some(8), Some(false), 1, Exif, &[1, 1]),
    // EOS 6D with Auto Rotate on: EXIF and CameraInfo agree.
    ("web/rawsamples.ch/canon/RAW_CANON_6D.CR2", Some(8), Some(8), Some(true), 8, Exif, &[8, 8]),
    // Normal EXIF next to a rotated sensor field, Auto Rotate not recorded: the camera's
    // EXIF choice is kept (the T3i and T6 files are copy-stand shots pointing straight
    // down, where the sensor reading is meaningless).
    ("pixls/25889/Kirstenbosch_Neg_35.CR2", Some(1), Some(6), None, 1, Exif, &[1, 1]),
    ("pixls/30593/1.CR2", Some(1), Some(6), None, 1, Exif, &[]),
    ("datasets/zenodo/4086342/IMG_9787.CR2", Some(1), Some(8), None, 1, Exif, &[]),
    ("ir/canon-sl1/SL1OUTBAP0.CR2", Some(1), Some(8), None, 1, Exif, &[]),
    ("ir/canon-t4i/YIMG_1131.CR2", Some(1), Some(8), None, 1, Exif, &[]),
];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn code(o: Orientation) -> u32 {
    o.exif()
}

/// Check `rows` under `root`; returns (checked, missing).
fn check(root: &Path, rows: &[Row]) -> (usize, usize) {
    let (mut checked, mut missing) = (0, 0);
    let mut failures = Vec::new();
    for &(rel, exif, mn, auto_rotate, resolved, source, previews) in rows {
        let p = root.join(rel);
        if !p.exists() {
            missing += 1;
            continue;
        }
        checked += 1;
        let raw = match FlashRaw::open_path(&p) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let m = raw.metadata();
        let o = &m.orientation_info;
        let got = (o.exif.map(code), o.maker_note.map(code), o.auto_rotate, code(o.resolved), o.source);
        let want = (exif, mn, auto_rotate, resolved, source);
        if got != want {
            failures.push(format!("{rel}: (exif, maker_note, auto_rotate, resolved, source) {got:?} != {want:?}"));
        }
        if code(m.orientation) != resolved || code(raw.frame_info().orientation) != resolved {
            failures.push(format!("{rel}: metadata/frame orientation differ from the resolved value"));
        }
        let p: Vec<u32> = m.previews.iter().map(|p| code(p.orientation)).collect();
        if !previews.is_empty() && p != previews {
            failures.push(format!("{rel}: preview orientations {p:?} != {previews:?}"));
        }
    }
    assert!(failures.is_empty(), "orientation mismatches:\n{}", failures.join("\n"));
    (checked, missing)
}

#[test]
fn corpus_orientation_records() {
    let corpus = std::env::var("FLASHRAW_CORPUS").map(PathBuf::from).unwrap_or_else(|_| repo().join("corpus"));
    if !corpus.exists() {
        eprintln!("corpus not found at {}; skipping (see tools/corpus/fetch.py)", corpus.display());
        return;
    }
    let (checked, missing) = check(&corpus, CORPUS);
    eprintln!("{checked} corpus files checked, {missing} missing");
}

#[test]
fn disagreeing_orientation_records() {
    let Ok(root) = std::env::var("FLASHRAW_ORIENTATION_SAMPLES") else {
        eprintln!("FLASHRAW_ORIENTATION_SAMPLES not set; skipping");
        return;
    };
    let (checked, missing) = check(Path::new(&root), SAMPLES);
    eprintln!("{checked} sample files checked, {missing} missing");
}
