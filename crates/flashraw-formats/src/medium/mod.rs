//! Medium-format and small-vendor TIFF raws that share infrastructure with the
//! Phase One / Hasselblad / Kodak decoders: Epson ERF, Mamiya MEF, Sinar STI/IA.
//!
//! Also hosts helpers shared by the medium-format modules (geometry conventions,
//! raw 16-bit row readers).

pub mod epson;
pub mod mamiya;
pub mod sinar;

use flashraw_core::*;

/// Margin convention for Bayer data: odd top/left margins are rounded up to
/// even (shrinking the visible size by one), so the visible area always starts on a
/// 2x2 CFA boundary and the visible CFA phase is stable. Decoders whose vendor
/// geometry has odd margins apply this.
pub fn even_margins(r: Rect) -> Rect {
    let mut r = r;
    if r.y % 2 == 1 && r.height > 0 {
        r.y += 1;
        r.height -= 1;
    }
    if r.x % 2 == 1 && r.width > 0 {
        r.x += 1;
        r.width -= 1;
    }
    r
}

/// Re-anchor a CFA pattern given relative to `(vx, vy)` (e.g. a vendor's visible-area
/// origin) at the buffer origin.
pub fn cfa_at_origin(p: CfaPattern, vx: u32, vy: u32) -> CfaPattern {
    let (w, h) = (p.width(), p.height());
    p.shifted((w - vx as usize % w) % w, (h - vy as usize % h) % h)
}

/// Fallback colour-table lookup: first entry (table order) whose model prefix
/// matches case-insensitively. Returns (black hint, white hint, matrix).
pub fn fallback_table(make: &str, model: &str) -> Option<&'static flashraw_camera_db::FallbackMatrix> {
    let m = model.to_ascii_lowercase();
    flashraw_camera_db::MATRICES
        .iter()
        .find(|e| e.canonical_make.eq_ignore_ascii_case(make) && m.starts_with(&e.model_prefix.to_ascii_lowercase()))
}

pub fn matrix_from(e: &flashraw_camera_db::FallbackMatrix) -> ColorMatrices {
    let mut out = ColorMatrices::default();
    let v: Vec<f32> = e.color_matrix.iter().map(|&x| x as f32 / 10000.0).collect();
    if v.iter().any(|&x| x != 0.0) {
        if let Some(cm) = ColorMatrix::new(Illuminant::D65, v.len() / 3, v) {
            out.color.push(cm);
            out.source = ColorSource::CameraDb;
        }
    }
    out
}

/// Read `out.len()` 16-bit samples from `src` in byte order `order`; returns the number
/// of samples actually available.
pub fn read_u16s(src: &[u8], order: ByteOrder, out: &mut [u16]) -> usize {
    let n = (src.len() / 2).min(out.len());
    match order {
        ByteOrder::Little => {
            for (o, c) in out[..n].iter_mut().zip(src.chunks_exact(2)) {
                *o = u16::from_le_bytes([c[0], c[1]]);
            }
        }
        ByteOrder::Big => {
            for (o, c) in out[..n].iter_mut().zip(src.chunks_exact(2)) {
                *o = u16::from_be_bytes([c[0], c[1]]);
            }
        }
    }
    n
}

/// Fill a full `width x height` buffer from uncompressed 16-bit samples in parallel.
/// Rows past the end of `src` stay zero (short final strips are common in the wild);
/// an error is returned only when not even one row is present.
pub fn unpack_u16_rows(
    src: &[u8],
    order: ByteOrder,
    width: usize,
    out: &mut [u16],
    row_stride_bytes: usize,
    parallel: bool,
) -> Result<()> {
    if width == 0 {
        return Ok(());
    }
    if src.len() < width * 2 {
        return Err(Error::corrupt("uncompressed raw data truncated before the first row"));
    }
    // Plain 16-bit copies are memory-bound: more than a few workers only add contention.
    par::for_each_row_chunk_bounded(out, width, par::memory_bound_workers(), parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(width).enumerate() {
            let off = (first + i) * row_stride_bytes;
            if let Some(s) = src.get(off..) {
                read_u16s(s, order, row);
            }
        }
    });
    Ok(())
}

/// Collect per-row decode errors from parallel workers (first error wins).
#[derive(Default)]
pub struct ErrorSlot(std::sync::Mutex<Option<Error>>);

impl ErrorSlot {
    pub fn set(&self, e: Error) {
        if let Ok(mut g) = self.0.lock() {
            if g.is_none() {
                *g = Some(e);
            }
        }
    }
    pub fn has_error(&self) -> bool {
        self.0.lock().map(|g| g.is_some()).unwrap_or(false)
    }
    pub fn into_result(self) -> Result<()> {
        match self.0.into_inner() {
            Ok(Some(e)) => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Representative corpus samples of every medium-format/Kodak payload kind.
    const SAMPLES: &[&str] = &[
        "Phase One/P25+/CF028662.IIQ",
        "Phase One/IQ180/230214_9564.IIQ",
        "Phase One/H 25/H25_IT8.7-2_Card.TIF",
        "Leaf/Aptus 22/L_003172.mos",
        "Leaf/Aptus 22/L_003173.mos",
        "Hasselblad/CFV/RAW_HASSELBLAD_CFV.3FR",
        "Hasselblad/X1D/B9999910.3FR",
        "Kodak/DCS760C/86L57188.DCR",
        "Kodak/DCS520C/23HK3627.TIF",
        "Kodak/EOS DCS 3/DCS00240.TIF",
        "Kodak/P880/100_3710.KDC",
        "Kodak/DC120/RAW_KODAK_DC120.KDC",
        "Kodak/DC50/RAW_KODAK_DC50.KDC",
        "Kodak/Kodak C330/RAW_KODAK_C330_FORMAT_NONE_YRGB.RAW",
        "Epson/R-D1/_EPS0672.ERF",
        "Mamiya/ZD/RAW_MAMIYA_ZD.MEF",
    ];

    fn try_decode(data: &[u8]) -> Result<RawImage> {
        let d = crate::open(data, &crate::OpenOptions::default())?;
        let p = Profiler::default();
        d.decode(&DecodeContext::new(&p))
    }

    /// Truncated and corrupted corpus files must produce errors (or decode), never panic.
    #[test]
    fn corpus_malformed_inputs_do_not_panic() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        let mut seen = 0;
        for rel in SAMPLES {
            let Ok(data) = std::fs::read(root.join(rel)) else { continue };
            seen += 1;
            assert!(try_decode(&data).is_ok(), "{rel}: intact file must decode");
            for frac in [0.0005, 0.01, 0.2, 0.5, 0.9, 0.999] {
                let n = (data.len() as f64 * frac) as usize;
                let _ = try_decode(&data[..n]);
            }
            let mut seed = 0x9E37_79B9u32;
            let mut bad = data.clone();
            for _ in 0..64 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let i = (seed as usize) % bad.len();
                bad[i] ^= (seed >> 24) as u8 | 1;
            }
            let _ = try_decode(&bad);
            // Header-only corruption (directory/IFD area).
            let mut hdr = data.clone();
            for b in hdr.iter_mut().take(4096).skip(8).step_by(7) {
                *b = b.wrapping_add(0x5B);
            }
            let _ = try_decode(&hdr);
        }
        if seen == 0 {
            eprintln!("corpus not present; malformed-input corpus test skipped");
        }
    }

    /// Heavier randomised corruption run (`cargo test --release -- --ignored`).
    #[test]
    #[ignore]
    fn corpus_fuzz_heavy() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
        for rel in SAMPLES {
            let Ok(data) = std::fs::read(root.join(rel)) else { continue };
            let mut seed: u32 = std::env::var("FLASHRAW_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(12345);
            for round in 0..150 {
                let mut bad = data.clone();
                // Mostly small edits; every 10th round a burst in the first 64 KiB.
                let n = if round % 10 == 0 { 400 } else { 8 };
                for _ in 0..n {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let span = if round % 10 == 0 { bad.len().min(65536) } else { bad.len() };
                    let i = (seed as usize >> 3) % span;
                    bad[i] = (seed >> 24) as u8;
                }
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let cut = if round % 3 == 0 { bad.len() - (seed as usize % bad.len().max(1)) } else { bad.len() };
                let _ = try_decode(&bad[..cut]);
            }
        }
    }

    #[test]
    fn margins_round_up_to_even() {
        assert_eq!(even_margins(Rect::new(41, 21, 7246, 5456)), Rect::new(42, 22, 7245, 5455));
        assert_eq!(even_margins(Rect::new(40, 22, 10, 10)), Rect::new(40, 22, 10, 10));
    }

    #[test]
    fn cfa_reanchor() {
        // RGGB at visible origin (26, 21) -> raw origin row 0 is the G/B row.
        assert_eq!(cfa_at_origin(CfaPattern::RGGB, 26, 21), CfaPattern::GBRG);
        assert_eq!(cfa_at_origin(CfaPattern::RGGB, 41, 22), CfaPattern::GRBG);
        assert_eq!(cfa_at_origin(CfaPattern::RGGB, 0, 0), CfaPattern::RGGB);
    }

    #[test]
    fn u16_rows_truncated() {
        let src = [1u8, 0, 2, 0, 3, 0];
        let mut out = vec![0u16; 4];
        unpack_u16_rows(&src, ByteOrder::Little, 2, &mut out, 4, false).unwrap();
        assert_eq!(out, vec![1, 2, 3, 0]);
        assert!(unpack_u16_rows(&src[..2], ByteOrder::Little, 2, &mut out, 4, false).is_err());
    }
}
