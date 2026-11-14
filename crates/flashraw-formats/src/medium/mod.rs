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

/// XYZ(D65) → camera matrix from a camera → ROMM (ProPhoto RGB, D50) matrix as Phase One
/// (IIQ tag 0x0106) and Leaf (`CaptProf_color_matrix`) record it. Those matrices apply to
/// white-balanced camera RGB (their rows sum to 1), so the result maps D65 white to a
/// neutral camera response; it is scaled to a unit green response. `None` for a matrix
/// that is not finite, not white-preserving or singular.
pub fn matrix_from_romm(romm_cam: &[f32]) -> Option<ColorMatrices> {
    // ROMM → XYZ(D50), then Bradford D50 → D65.
    const XYZ_FROM_ROMM: [[f64; 3]; 3] =
        [[0.797_674_9, 0.135_191_7, 0.031_353_4], [0.288_040_2, 0.711_874_1, 0.000_085_7], [0.0, 0.0, 0.825_210_0]];
    const D65_FROM_D50: [[f64; 3]; 3] = [
        [0.955_576_6, -0.023_039_3, 0.063_163_6],
        [-0.028_289_5, 1.009_941_6, 0.021_007_7],
        [0.012_298_2, -0.020_483_0, 1.329_909_8],
    ];
    if romm_cam.len() != 9 || !romm_cam.iter().all(|v| v.is_finite()) {
        return None;
    }
    let r: Vec<f64> = romm_cam.iter().map(|&v| v as f64).collect();
    if r.chunks_exact(3).any(|row| (row.iter().sum::<f64>() - 1.0).abs() > 0.05) {
        return None;
    }
    let mul = |a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]| {
        let mut o = [[0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        o
    };
    let rc = [[r[0], r[1], r[2]], [r[3], r[4], r[5]], [r[6], r[7], r[8]]];
    let [[a, b, c], [d, e, f], [g, h, i]] = mul(&mul(&D65_FROM_D50, &XYZ_FROM_ROMM), &rc);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = [
        [(e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det],
        [(f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det],
        [(d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det],
    ];
    const D65: [f64; 3] = [0.950_47, 1.0, 1.088_83];
    let green: f64 = (0..3).map(|k| inv[1][k] * D65[k]).sum();
    if green.abs() < 1e-9 {
        return None;
    }
    let values: Vec<f32> = inv.iter().flatten().map(|&v| (v / green) as f32).collect();
    let mut out = ColorMatrices::default();
    out.color.push(ColorMatrix::new(Illuminant::D65, 3, values)?);
    out.source = ColorSource::File;
    Some(out)
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

/// Fill `out` (zeroed, whole rows of `width` samples) from uncompressed 16-bit samples of
/// sensor-data `unit`, `stride` bytes per row from `offset` of `data` (the whole file).
/// The payload is addressed by geometry: the rows' bytes must be in the file (the last
/// row needs no padding; a declared length that is shorter or longer does not matter).
/// Missing bytes fail under the strict policy; under the recover policy the complete
/// samples are decoded, the rest stay zero and a warning is recorded.
#[allow(clippy::too_many_arguments)]
pub fn decode_u16_rows(
    ctx: &DecodeContext,
    data: &[u8],
    unit: DataUnit,
    offset: usize,
    order: ByteOrder,
    width: usize,
    stride: usize,
    out: &mut [u16],
) -> Result<()> {
    if width == 0 {
        return Ok(());
    }
    let rows = out.len() / width;
    let geometry = RowGeometry { rows, stride, row_bytes: width * 2, samples: width };
    let src = ctx.sensor_rows(data, unit, offset, geometry)?;
    // Plain 16-bit copies are memory-bound: more than a few workers only add contention.
    par::for_each_row_chunk_bounded(out, width, par::memory_bound_workers(), ctx.parallel, |first, chunk| {
        for (i, row) in chunk.chunks_mut(width).enumerate() {
            // Only complete samples are read (missing ones stay zero when recovering).
            match src.get((first + i) * stride..) {
                Some(s) => read_u16s(s, order, row),
                None => break,
            };
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

    #[test]
    fn romm_matrix_to_xyz() {
        // The camera → ROMM matrix of IQ3 100MP files gives the database matrix of that back.
        let romm = [1.193, -0.2, 0.007, -0.018, 1.129, -0.111, 0.028, -0.173, 1.145];
        let m = matrix_from_romm(&romm).unwrap();
        assert_eq!(m.source, ColorSource::File);
        let got: Vec<i32> = m.color[0].values.iter().map(|v| (v * 10000.0).round() as i32).collect();
        assert_eq!(got, [10999, 354, -742, -4590, 13342, 937, -1060, 2166, 8120]);
        // Not white-preserving / not finite / singular / short.
        assert!(matrix_from_romm(&[1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0]).is_none());
        assert!(matrix_from_romm(&[f32::NAN; 9]).is_none());
        assert!(matrix_from_romm(&[1.0 / 3.0; 9]).is_none());
        assert!(matrix_from_romm(&[1.0; 3]).is_none());
    }

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
    fn u16_rows_by_policy() {
        // 3 rows of 2 samples at a 4-byte stride from offset 2; the last row needs 4 bytes.
        let data = [9u8, 9, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0];
        let p = Profiler::default();
        let mut ctx = DecodeContext::new(&p);
        let mut out = vec![0u16; 6];
        decode_u16_rows(&ctx, &data, DataUnit::Strip(0), 2, ByteOrder::Little, 2, 4, &mut out).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6]);
        assert!(ctx.take_warnings().is_empty());
        // One byte short: recover keeps the complete samples, strict fails.
        let mut out = vec![0u16; 6];
        decode_u16_rows(&ctx, &data[..13], DataUnit::Strip(0), 2, ByteOrder::Little, 2, 4, &mut out).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 0]);
        let w = ctx.take_warnings();
        assert_eq!((w.len(), w[0].samples_filled), (1, 1));
        ctx.recover = false;
        let e = decode_u16_rows(&ctx, &data[..13], DataUnit::Strip(0), 2, ByteOrder::Little, 2, 4, &mut out);
        let t = e.unwrap_err().truncation().cloned().unwrap();
        assert_eq!((t.offset, t.expected, t.available), (2, 12, 11));
    }
}
