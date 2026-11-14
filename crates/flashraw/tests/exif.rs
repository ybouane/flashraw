//! `FlashRaw::exif_bytes` and `FlashRaw::xmp_bytes`: the EXIF of a file as a standalone TIFF stream and its
//! XMP packet. The bundled DNGs cover the API; every corpus file, when the corpus is present,
//! checks that the stream parses as TIFF and names the camera FlashRaw reports.

use flashraw::FlashRaw;
use flashraw_core::tiff::{Tiff, tags};
use std::path::PathBuf;

fn corpus_files() -> Vec<PathBuf> {
    let root = std::env::var_os("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x != "part" && x != "txt" && x != "json") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../npm/tests/fixtures").join(name)).unwrap()
}

#[test]
fn exif_is_a_standalone_tiff_with_the_camera_and_exposure() {
    let data = fixture("canon-5d3-mlv.dng");
    let raw = FlashRaw::open(data.as_slice()).unwrap();
    let exif = raw.exif_bytes().expect("EXIF");
    assert!(exif.starts_with(b"II*\0") || exif.starts_with(b"MM\0*"));
    // Cached after the first call.
    assert_eq!(raw.exif_bytes().unwrap().as_ptr(), exif.as_ptr());
    let t = Tiff::parse(exif).unwrap();
    let ifd0 = &t.ifds[0];
    assert_eq!(ifd0.string(tags::MAKE).as_deref(), Some("Canon"));
    assert_eq!(ifd0.string(tags::MODEL).as_deref(), Some(raw.metadata().model.as_str()));
    assert!(!ifd0.has(tags::STRIP_OFFSETS) && !ifd0.has(tags::TILE_OFFSETS) && !ifd0.has(tags::SUB_IFDS));
    let e = ifd0.exif.as_ref().expect("EXIF IFD");
    assert_eq!(e.u32(0x8827), raw.metadata().exposure.iso);
    let xmp = raw.xmp_bytes().expect("XMP");
    assert!(xmp.starts_with(b"<?xpacket") && xmp.ends_with(b">"));
}

#[test]
fn files_without_exif_or_xmp() {
    let data = fixture("bmpcc-tiles.dng");
    let raw = FlashRaw::open(data.as_slice()).unwrap();
    assert!(raw.exif_bytes().is_none());
    assert!(raw.xmp_bytes().is_none());
}

#[test]
fn corpus_exif_names_the_camera() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("corpus not present: skipped");
        return;
    }
    let (mut with, mut checked) = (0, 0);
    for p in &files {
        let Ok(data) = std::fs::read(p) else { continue };
        let Ok(raw) = FlashRaw::open(data.as_slice()) else { continue };
        let Some(exif) = raw.exif_bytes() else { continue };
        with += 1;
        let t = Tiff::parse(exif).unwrap_or_else(|e| panic!("{}: EXIF does not parse: {e}", p.display()));
        let make = t.ifds[0].string(tags::MAKE).unwrap_or_default();
        let m = raw.metadata();
        if !make.is_empty() && !m.make.is_empty() {
            checked += 1;
            // FlashRaw may shorten the make ("Eastman Kodak Company" is "Kodak").
            let (a, b) = (make.to_lowercase(), m.make.to_lowercase());
            assert!(a.contains(b.trim()) || b.contains(a.trim()), "{}: {make:?} / {:?}", p.display(), m.make);
        }
    }
    eprintln!("{with} of {} files have EXIF, {checked} with the make checked", files.len());
}
