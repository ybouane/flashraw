//! Medium-format and small-vendor TIFF raws that share infrastructure with the
//! Phase One / Hasselblad / Kodak decoders: Epson ERF, Mamiya MEF, Sinar STI/IA.
//!
//! geometry conventions, raw 16-bit row readers).

pub mod epson;
pub mod mamiya;
pub mod sinar;

use flashraw_core::*;

/// even (shrinking the visible size by one), so the visible area always starts on a
/// vendor geometry has odd margins apply this.
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
/// strips); an error is returned only when not even one row is present.
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
    par::for_each_row_chunk(out, width, 64, parallel, |first, chunk| {
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
