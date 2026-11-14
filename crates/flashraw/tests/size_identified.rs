//! Header-less dumps whose file size is shared by several cameras: the layout is told
//! apart by the data.
//!
//! Runs on corpus files (`$FLASHRAW_CORPUS` or `<repo>/corpus`); skipped when absent.

use flashraw::{CfaPattern, DecodeOptions, FlashRaw, SensorLayout};
use std::path::PathBuf;

fn corpus() -> PathBuf {
    std::env::var_os("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

/// SJCam 5000X Elite dumps have the size of the Kodak "12MP" YCbCr dump (18,000,000 bytes)
/// but hold 12-bit packed RGGB rows.
#[test]
fn sjcam_5000x_is_packed_bayer() {
    let dir = corpus().join("SJCam/5000X Elite");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("SJCam 5000X: corpus not found, skipped");
        return;
    };
    let mut seen = 0;
    for f in entries.flatten() {
        let path = f.path();
        let raw = FlashRaw::open_path(&path).unwrap();
        let info = raw.frame_info();
        assert_eq!(raw.metadata().canonical_model, "SJ5000X Elite", "{}", path.display());
        assert_eq!((info.width, info.height, info.cpp, info.bits_per_sample), (4000, 3000, 1, 12));
        assert_eq!(info.layout, SensorLayout::Bayer);
        assert_eq!(info.cfa, Some(CfaPattern::RGGB));
        let frame = raw.decode(DecodeOptions::default()).unwrap();
        let px = frame.as_u16().unwrap();
        // Same-colour neighbours (two apart) are much closer than adjacent samples of
        // different colours; a misread layout gives noise or stripes instead.
        let (mut near, mut same) = (0u64, 0u64);
        for row in px.chunks_exact(4000).step_by(37) {
            for x in 0..3996 {
                near += row[x].abs_diff(row[x + 1]) as u64;
                same += row[x].abs_diff(row[x + 2]) as u64;
            }
        }
        assert!(same * 2 < near, "{}: same-colour {same} vs adjacent {near}", path.display());
        assert!(px.iter().all(|&v| v <= 4095));
        seen += 1;
    }
    assert!(seen > 0);
}
