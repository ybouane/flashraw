//! Nikon High Efficiency NEF files through the public API: one thread and many give the same
//! sensor buffer, a damaged stream fails the decode (no partial image) while the file still
//! opens, and the corpus files report the metadata the container supplies.
//!
//! The corpus is optional (`FLASHRAW_CORPUS`); without it the corpus tests print a note and pass.

use flashraw::{CorruptionPolicy, DecodeOptions, Error, FlashRaw, OutputFormat};
use std::path::{Path, PathBuf};

fn corpus() -> Option<PathBuf> {
    let p = std::env::var("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    p.exists().then_some(p)
}

/// The corpus files listed as High Efficiency in the regression table.
fn he_files(corpus: &Path) -> Vec<PathBuf> {
    let tsv = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/regression/nikon.tsv");
    let text = std::fs::read_to_string(tsv).unwrap();
    text.lines()
        .filter(|l| l.ends_with("\tnikon-he"))
        .map(|l| corpus.join(l.split('\t').next().unwrap()))
        .filter(|p| p.exists())
        .collect()
}

#[test]
fn one_thread_and_many_give_the_same_buffer() {
    let Some(corpus) = corpus() else {
        eprintln!("corpus not present; skipping");
        return;
    };
    let files = he_files(&corpus);
    for path in files.iter().step_by(3) {
        let raw = FlashRaw::open_path(path).unwrap();
        let one = raw.decode(DecodeOptions { threads: Some(1), ..Default::default() }).unwrap();
        let many = raw.decode(DecodeOptions::default()).unwrap();
        let (a, b) = (one.as_u16().unwrap(), many.as_u16().unwrap());
        assert_eq!((one.width, one.height), (many.width, many.height));
        assert!(a == b, "{}: threads differ", path.display());
        assert!(a.iter().all(|&v| v <= 16383));
    }
    eprintln!("compared {} files", files.len().div_ceil(3));
}

#[test]
fn metadata_of_the_container() {
    let Some(corpus) = corpus() else { return };
    for path in he_files(&corpus) {
        let raw = FlashRaw::open_path(&path).unwrap();
        let info = raw.frame_info();
        let f = path.display();
        assert_eq!(info.compression, flashraw::Compression::NikonHighEfficiency, "{f}");
        assert_eq!(info.bits_per_sample, 14, "{f}");
        assert_eq!(info.white_level.values, vec![16383], "{f}");
        assert_eq!(info.black_level.values, vec![1008.0], "{f}");
        assert_eq!(info.cfa, Some(flashraw::CfaPattern::RGGB), "{f}");
        assert_eq!((info.active_area.width, info.active_area.height), (info.width, info.height), "{f}");
        assert!(!info.crop.is_empty() && info.crop.width <= info.width, "{f}");
        assert!(!raw.previews().is_empty(), "{f}");
    }
}

/// A damaged stream is a hard error under both corruption policies; the file still opens and
/// its previews and metadata stay available.
#[test]
fn damaged_streams_fail_the_decode_but_not_the_open() {
    let Some(corpus) = corpus() else { return };
    let path = corpus.join("NIKON CORPORATION/NIKON Z5_2/High_Efficiency_DX.NEF");
    let Ok(data) = std::fs::read(&path) else { return };
    let strip = FlashRaw::open(&data[..]).unwrap().units().first().map(|u| u.offset as usize).unwrap();
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    // Truncated in the middle of the codestream.
    cases.push(("truncated", data[..strip + (data.len() - strip) / 2].to_vec()));
    // The end marker overwritten.
    let mut no_eoc = data.clone();
    let n = no_eoc.len();
    no_eoc[n - 2..].copy_from_slice(&[0, 0]);
    cases.push(("no end marker", no_eoc));
    // The marker of the second slice overwritten. Walk the first slice: a marker (6 bytes), then
    // 16 precincts of a 12-byte header and the payload length from the header's first 3 bytes.
    let mut bad_slice = data.clone();
    let first = data[strip..].windows(6).position(|w| w == [0xFF, 0x20, 0x00, 0x04, 0x00, 0x00]).unwrap() + strip;
    let mut at = first + 6;
    for _ in 0..16 {
        let len = usize::from(data[at]) << 16 | usize::from(data[at + 1]) << 8 | usize::from(data[at + 2]);
        at += 12 + len;
    }
    assert_eq!(&data[at..at + 2], &[0xFF, 0x20]);
    bad_slice[at + 1] = 0x21;
    cases.push(("bad slice marker", bad_slice));
    for (name, bytes) in &cases {
        let raw = FlashRaw::open(&bytes[..]).unwrap_or_else(|e| panic!("{name}: open: {e}"));
        assert!(!raw.previews().is_empty(), "{name}");
        for policy in [CorruptionPolicy::Recover, CorruptionPolicy::Strict] {
            for output in [OutputFormat::RawU16, OutputFormat::Rgba8] {
                let opts = DecodeOptions { output, corruption_policy: policy, ..Default::default() };
                let e = raw.decode(opts).expect_err(&format!("{name}: {policy:?}"));
                assert!(matches!(e, Error::CorruptFile { .. }), "{name}: {e}");
            }
        }
    }
}
