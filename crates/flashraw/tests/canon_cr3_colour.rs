//! Recent Canon CR3 bodies develop with the colour balance of their embedded preview:
//! the black levels and white balance come from the right ColorData words for the
//! layout (a wrong black offset reads a tone-curve table and casts the image magenta).
//!
//! Runs on the CC0 corpus (`$FLASHRAW_CORPUS` or `<repo>/corpus`); skipped when absent.

use flashraw::{DecodeOptions, Demosaic, FlashRaw, OutputFormat, PreviewEncoding};
use std::path::PathBuf;

fn corpus() -> Option<PathBuf> {
    let c = std::env::var_os("FLASHRAW_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    c.exists().then_some(c)
}

/// `(R/G, B/G)` of the channel means over interleaved pixels of `stride` bytes.
fn ratios(px: &[u8], stride: usize) -> (f64, f64) {
    let mut s = [0u64; 3];
    for p in px.chunks_exact(stride) {
        for c in 0..3 {
            s[c] += p[c] as u64;
        }
    }
    let g = s[1].max(1) as f64;
    (s[0] as f64 / g, s[2] as f64 / g)
}

/// Bodies whose 3778-word ColorData uses the newer (sub-version 64+) word offsets, plus
/// bodies of the older layout as controls.
const FILES: &[&str] = &[
    "Canon/Canon EOS R6 Mark III/IMG_4241.CR3",
    "Canon/Canon EOS R6 Mark III/IMG_4242.CR3",
    "Canon/Canon EOS R6 Mark III/IMG_4243.CR3",
    "Canon/Canon EOS R6 Mark III/IMG_4244.CR3",
    "Canon/Canon PowerShot V1/IMG_1225.CR3",
    "Canon/Canon EOS R50 V/IMG_0034.CR3",
    "Canon/EOS R6 Mark II/canon-r6m2-img_0347-raw-normal-crop.cr3",
    "Canon/EOS R10/IMG_4276.CR3",
];

#[test]
fn recent_cr3_bodies_match_their_preview_balance() {
    let Some(corpus) = corpus() else {
        eprintln!("canon cr3 colour: corpus not found, skipped");
        return;
    };
    let mut failures = Vec::new();
    for rel in FILES {
        let path = corpus.join(rel);
        if !path.exists() {
            eprintln!("missing {rel}");
            continue;
        }
        let raw = FlashRaw::open_path(&path).unwrap();
        let fi = raw.frame_info();
        let black = fi.black_level.values.iter().copied().fold(f32::INFINITY, f32::min);
        let opts = DecodeOptions { output: OutputFormat::Rgba8, demosaic: Demosaic::HalfSize, ..Default::default() };
        let f = raw.decode(opts).unwrap();
        let (rr, rb) = ratios(f.as_u8().unwrap(), 4);
        let p = raw
            .previews()
            .iter()
            .filter(|p| p.encoding == PreviewEncoding::Jpeg)
            .max_by_key(|p| p.width as u64 * p.height as u64)
            .expect("embedded JPEG preview");
        let img = raw.decode_preview_rgb(p).unwrap();
        let (pr, pb) = ratios(&img.rgb, 3);
        let line = format!(
            "{rel}: black {:?}, raw R/G {rr:.3} B/G {rb:.3}, preview R/G {pr:.3} B/G {pb:.3}",
            fi.black_level.values
        );
        eprintln!("{line}");
        // A maker-note black is 512 or more on all of these (the tone-curve words are
        // below 256); bodies in the sensor table measure it from the masked area.
        let black_ok = fi.black_from_masked || black >= 256.0;
        if !black_ok || (rr - pr).abs() > 0.1 || (rb - pb).abs() > 0.1 {
            failures.push(line);
        }
    }
    assert!(failures.is_empty(), "colour differs from the embedded preview:\n{}", failures.join("\n"));
}
