//! Canon sRAW / mRAW (CR2 "small raw") decoding.
//!
//! sRAW frames are lossless-JPEG streams of subsampled YCbCr: each MCU holds
//! `n` luma samples followed by Cb and Cr (4:2:2 → `Y Y Cb Cr`, 4:2:0 →
//! `Y Y Y Y Cb Cr`). The stream is cut into vertical slices like ordinary CR2
//! data. Decoding produces linear camera RGB with three samples per pixel:
//!
//! 1. entropy-decode MCUs and scatter luma to every covered pixel and chroma to the
//!    top-left pixel of the MCU (chroma offset by -16384);
//! 2. upsample chroma: vertically for 4:2:0 (average of the rows above/below),
//!    then horizontally (average of the left/right neighbours), rounding up;
//! 3. convert YCbCr → RGB with Canon's per-generation coefficients, applying the
//!    sRAW white-balance multipliers stored in the maker note, clipped to 15 bits.
//!
//! The arithmetic (16-bit storage of intermediate chroma, integer rounding, the

use crate::ljpeg::{self, LjpegHeader};
use flashraw_core::{Error, Result};

/// Colour conversion variant (depends on the camera generation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrawConversion {
    /// Early bodies (40D, 1D Mark III era): `R = Y + Cr`, `B = Y + Cb`,
    /// `G = Y - (778 Cb + 2048 Cr) / 4096`; `subtract_512` removes the luma offset
    /// of bodies older than the 5D Mark II.
    Original { subtract_512: bool },
    /// 5D Mark II, 7D, 50D, 1D Mark IV, 60D: chroma scaled by 4 plus `hue`, full matrix.
    Matrix,
}

/// Geometry and colour parameters of an sRAW frame.
#[derive(Debug, Clone, Copy)]
pub struct SrawParams {
    /// Output width (pixels) — also the row stride of the output.
    pub width: usize,
    pub height: usize,
    /// Width of the encoded area used to terminate the last slice (≥ `width`).
    pub raw_width: usize,
    /// CR2 slice description `[count, width, last_width]` in JPEG sample units
    /// (`[0, 0, 0]` when the frame is not sliced).
    pub slices: [u16; 3],
    pub conversion: SrawConversion,
    /// Chroma offset added after scaling (Matrix conversion only).
    pub hue: i32,
    /// White-balance multipliers `R, G, B` (1024 = unity), applied as `v * m >> 10`.
    pub mul: [u16; 3],
}

impl SrawParams {
    /// Canon's hue offset: `(sraw + 1) * 4` for older firmware, `sraw * 2` for newer bodies.
    pub fn default_hue(sraw: u32, newer: bool) -> i32 {
        if newer { (sraw as i32) << 1 } else { ((sraw as i32) + 1) << 2 }
    }
}

/// Decode an sRAW stream into `out` (`width * height * 3` samples, RGB interleaved).
pub fn decode(data: &[u8], hdr: &LjpegHeader, p: &SrawParams, out: &mut [u16]) -> Result<()> {
    let clrs = hdr.clrs as usize;
    if hdr.sraw == 0 || !(4..=6).contains(&clrs) {
        return Err(Error::corrupt("sRAW: frame is not subsampled YCbCr"));
    }
    let (w, h) = (p.width, p.height);
    let n = w.checked_mul(h).and_then(|v| v.checked_mul(3)).ok_or_else(|| Error::corrupt("sRAW: size overflow"))?;
    if out.len() < n || w < 2 || h < 1 {
        return Err(Error::corrupt("sRAW: output buffer too small"));
    }
    let out = &mut out[..n];
    out.fill(0);
    // Each MCU covers two columns; the frame header counts luma columns.
    let mut jh = hdr.clone();
    jh.width /= 2;
    let jwide = jh.width as usize * clrs;
    if !(32..=65535).contains(&jwide) {
        return Err(Error::corrupt(format!("sRAW: invalid MCU row length {jwide}")));
    }
    scatter(data, &jh, p, out)?;
    interpolate_chroma(out, w, h, hdr.sraw);
    convert(out, p);
    Ok(())
}

/// Iterator over MCU destinations in slice order.
struct Cursor {
    slice: usize,
    slices: usize,
    scol: usize,
    ecol: usize,
    row: usize,
    col: usize,
    step: usize,
    slice_cols: usize,
    raw_width: usize,
    height: usize,
    done: bool,
}

impl Cursor {
    fn new(p: &SrawParams, clrs: usize) -> Cursor {
        let slice_cols = p.slices[1] as usize * 2 / clrs;
        let mut c = Cursor {
            slice: 0,
            slices: p.slices[0] as usize,
            scol: 0,
            ecol: 0,
            row: 0,
            col: 0,
            step: (clrs >> 1) - 1,
            slice_cols,
            raw_width: p.raw_width,
            height: p.height,
            done: false,
        };
        c.start_slice(0);
        c
    }

    fn start_slice(&mut self, scol: usize) {
        self.scol = scol;
        self.ecol = scol + self.slice_cols;
        if self.slices == 0 || self.ecol + 1 > self.raw_width {
            self.ecol = self.raw_width & !1;
        }
        self.row = 0;
        self.col = scol;
        self.skip_empty();
    }

    /// Advance past empty rows/slices so `(row, col)` is a valid MCU position.
    fn skip_empty(&mut self) {
        loop {
            if self.slice > self.slices || self.height == 0 {
                self.done = true;
                return;
            }
            if self.col < self.ecol && self.row < self.height {
                return;
            }
            if self.col >= self.ecol && self.row < self.height {
                self.row += self.step;
                self.col = self.scol;
                if self.row < self.height && self.col < self.ecol {
                    return;
                }
            }
            if self.row >= self.height || self.scol >= self.ecol {
                self.slice += 1;
                let next = self.ecol;
                if self.slice > self.slices {
                    self.done = true;
                    return;
                }
                self.scol = next;
                self.ecol = next + self.slice_cols;
                if self.slices == 0 || self.ecol + 1 > self.raw_width {
                    self.ecol = self.raw_width & !1;
                }
                self.row = 0;
                self.col = self.scol;
            }
        }
    }

    fn advance(&mut self) {
        self.col += 2;
        self.skip_empty();
    }
}

fn scatter(data: &[u8], jh: &LjpegHeader, p: &SrawParams, out: &mut [u16]) -> Result<()> {
    let clrs = jh.clrs as usize;
    let nluma = clrs - 2;
    let w = p.width;
    let mut cur = Cursor::new(p, clrs);
    let res = ljpeg::decode_rows(data, jh, |_, row| {
        for mcu in row.chunks_exact(clrs) {
            if cur.done {
                return Ok(());
            }
            let (r, c) = (cur.row, cur.col);
            if c < w {
                let base = r * w + c;
                for (k, &y) in mcu[..nluma].iter().enumerate() {
                    let i = base + (k >> 1) * w + (k & 1);
                    if let Some(px) = out.get_mut(i * 3) {
                        *px = y;
                    }
                }
                let i = base * 3;
                if let Some(px) = out.get_mut(i + 1..i + 3) {
                    px[0] = (mcu[nluma] as i16).wrapping_sub(16384) as u16;
                    px[1] = (mcu[nluma + 1] as i16).wrapping_sub(16384) as u16;
                }
            }
            cur.advance();
        }
        Ok(())
    });
    match res {
        // garbage or a truncated tail beyond that point is irrelevant.
        Err(_) if cur.done => Ok(()),
        r => r,
    }
}

#[inline(always)]
fn s(v: u16) -> i32 {
    v as i16 as i32
}

fn interpolate_chroma(px: &mut [u16], w: usize, h: usize, sraw: u32) {
    let vertical = sraw >> 1 != 0;
    for row in 0..h {
        let base = row * w * 3;
        if vertical && row & 1 == 1 {
            for col in (0..w).step_by(2) {
                for c in 1..3 {
                    let i = base + col * 3 + c;
                    let up = s(px[i - w * 3]);
                    px[i] =
                        if row == h - 1 { up as i16 as u16 } else { ((up + s(px[i + w * 3]) + 1) >> 1) as i16 as u16 };
                }
            }
        }
        for col in (1..w).step_by(2) {
            for c in 1..3 {
                let i = base + col * 3 + c;
                let left = s(px[i - 3]);
                px[i] = if col == w - 1 { left as i16 as u16 } else { ((left + s(px[i + 3]) + 1) >> 1) as i16 as u16 };
            }
        }
    }
}

#[inline(always)]
fn clip15(v: i32) -> u16 {
    v.clamp(0, 32767) as u16
}

fn convert(px: &mut [u16], p: &SrawParams) {
    let mul = [p.mul[0] as i32, p.mul[1] as i32, p.mul[2] as i32];
    let scale = |v: i32, m: i32| clip15(v.wrapping_mul(m) >> 10);
    match p.conversion {
        SrawConversion::Matrix => {
            let hue = p.hue;
            for t in px.chunks_exact_mut(3) {
                let y = s(t[0]);
                let cb = ((s(t[1]) << 2) + hue) as i16 as i32;
                let cr = ((s(t[2]) << 2) + hue) as i16 as i32;
                let r = y + ((50 * cb + 22929 * cr) >> 14);
                let g = y + ((-5640 * cb - 11751 * cr) >> 14);
                let b = y + ((29040 * cb - 101 * cr) >> 14);
                t[0] = scale(r, mul[0]);
                t[1] = scale(g, mul[1]);
                t[2] = scale(b, mul[2]);
            }
        }
        SrawConversion::Original { subtract_512 } => {
            for t in px.chunks_exact_mut(3) {
                let mut y = s(t[0]);
                if subtract_512 {
                    y = (y - 512) as i16 as i32;
                }
                let (cb, cr) = (s(t[1]), s(t[2]));
                let r = y + cr;
                let b = y + cb;
                let g = y + ((-778 * cb - (cr << 11)) >> 12);
                t[0] = scale(r, mul[0]);
                t[1] = scale(g, mul[1]);
                t[2] = scale(b, mul[2]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(w: usize, h: usize, slices: [u16; 3]) -> SrawParams {
        SrawParams {
            width: w,
            height: h,
            raw_width: w,
            slices,
            conversion: SrawConversion::Original { subtract_512: false },
            hue: 0,
            mul: [1024; 3],
        }
    }

    #[test]
    fn cursor_walks_slices_in_order() {
        // 4:2:2 (clrs 4): slice width 8 JPEG samples → 4 columns; 2 full slices + remainder.
        let p = params(10, 2, [2, 8, 4]);
        let mut c = Cursor::new(&p, 4);
        let mut seen = vec![];
        while !c.done {
            seen.push((c.row, c.col));
            c.advance();
        }
        let expect = vec![(0, 0), (0, 2), (1, 0), (1, 2), (0, 4), (0, 6), (1, 4), (1, 6), (0, 8), (1, 8)];
        assert_eq!(seen, expect);
    }

    #[test]
    fn cursor_420_steps_two_rows() {
        let p = params(4, 4, [0, 0, 0]);
        let mut c = Cursor::new(&p, 6);
        let mut seen = vec![];
        while !c.done {
            seen.push((c.row, c.col));
            c.advance();
        }
        assert_eq!(seen, vec![(0, 0), (0, 2), (2, 0), (2, 2)]);
    }

    #[test]
    fn chroma_interpolation_rounds_up() {
        // 4x2 image, 4:2:0: chroma present at (0,0) and (0,2).
        let (w, h) = (4, 2);
        let mut px = vec![0u16; w * h * 3];
        let set = |px: &mut Vec<u16>, x: usize, y: usize, cb: i16, cr: i16| {
            px[(y * w + x) * 3 + 1] = cb as u16;
            px[(y * w + x) * 3 + 2] = cr as u16;
        };
        set(&mut px, 0, 0, 10, -10);
        set(&mut px, 2, 0, 13, -13);
        interpolate_chroma(&mut px, w, h, 3);
        let get = |px: &Vec<u16>, x: usize, y: usize| (px[(y * w + x) * 3 + 1] as i16, px[(y * w + x) * 3 + 2] as i16);
        assert_eq!(get(&px, 1, 0), (12, -11)); // (10 + 13 + 1) >> 1, (-10 - 13 + 1) >> 1
        assert_eq!(get(&px, 3, 0), (13, -13)); // right edge copies
        assert_eq!(get(&px, 0, 1), (10, -10)); // last row copies the row above
        assert_eq!(get(&px, 1, 1), (12, -11));
    }

    #[test]
    fn conversion_matches_reference_formulas() {
        let mut px = vec![4000u16, 100i16 as u16, (-50i16) as u16];
        let p = SrawParams { conversion: SrawConversion::Original { subtract_512: true }, ..params(1, 1, [0; 3]) };
        convert(&mut px, &p);
        let y = 4000 - 512;
        assert_eq!(px, vec![(y - 50) as u16, (y + ((-778 * 100 - (-50 << 11)) >> 12)) as u16, (y + 100) as u16]);
        let mut px = vec![4000u16, 100i16 as u16, (-50i16) as u16];
        let p =
            SrawParams { conversion: SrawConversion::Matrix, hue: 6, mul: [2048, 1024, 512], ..params(1, 1, [0; 3]) };
        convert(&mut px, &p);
        let (cb, cr) = (406, -194);
        let r = 4000 + ((50 * cb + 22929 * cr) >> 14);
        let g = 4000 + ((-5640 * cb - 11751 * cr) >> 14);
        let b = 4000 + ((29040 * cb - 101 * cr) >> 14);
        assert_eq!(px, vec![clip15(r * 2), clip15(g), clip15(b / 2)]);
    }

    #[test]
    fn rejects_non_sraw_frames() {
        let hdr_data = crate::ljpeg::encoder::encode(4, 2, 2, 12, 1, &[0; 16]);
        let hdr = LjpegHeader::parse(&hdr_data, Default::default()).unwrap();
        let mut out = vec![0u16; 4 * 2 * 3];
        assert!(decode(&hdr_data, &hdr, &params(4, 2, [0; 3]), &mut out).is_err());
    }
}
