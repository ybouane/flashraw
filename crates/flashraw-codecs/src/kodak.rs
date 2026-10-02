//! Kodak entropy coders and pixel transforms.
//!
//! - **65000** (DCR, DCS Pro / 7x0 series): rows are split into blocks of up to 256
//!   samples; each block starts with 4-bit difference lengths (two per byte) followed
//!   by the differences, read LSB-first from big-endian 16-bit words. Predictors reset
//!   per block (one per column parity). A length nibble above 12 marks a block stored
//!   as raw 12-bit samples instead. Because a block's byte length follows from its
//!   length nibbles alone, [`scan_65000_rows`] locates every row cheaply so rows
//!   decode in parallel.
//! - **RADC** (DC40/DC50): adaptive Huffman-coded predictive scheme on 4-row bands.
//! - **DC120**: 8-bit rows stored with a per-row rotation.
//! - YCbCr 4:2:x to RGB transforms of the C330/C603 compact cameras.

use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::{Error, Result};

/// Samples per 65000 block.
pub const BLOCK_65000: usize = 256;

/// Byte length of one 65000 block of `len` samples starting at `data[pos..]`, and
/// whether it is a raw (uncompressed 12-bit) block. `None` if the header is truncated.
pub fn block_len_65000(data: &[u8], pos: usize, len: usize) -> Option<(usize, bool)> {
    let bsize = (len + 3) & !3;
    let hdr = data.get(pos..pos + bsize / 2)?;
    if hdr.iter().any(|&c| c & 15 > 12 || c >> 4 > 12) {
        return Some((bsize.div_ceil(8) * 12, true));
    }
    let mut consumed = bsize / 2;
    let mut bits = 0u32;
    if bsize & 7 == 4 {
        consumed += 2;
        bits = 16;
    }
    for &c in hdr {
        for l in [(c & 15) as u32, (c >> 4) as u32] {
            if bits < l {
                consumed += 4;
                bits += 32;
            }
            bits -= l;
        }
    }
    Some((consumed, false))
}

/// Decode one 65000 block of `len` samples at `data[pos..]` into `out` (at least
/// `(len + 3) & !3` rounded up to 8 entries). Returns `true` for a raw block (values
/// are absolute samples rather than differences).
pub fn decode_block_65000(data: &[u8], pos: usize, len: usize, out: &mut [i16]) -> Result<bool> {
    let bsize = (len + 3) & !3;
    let need = bsize.div_ceil(8) * 8;
    if out.len() < need {
        return Err(Error::InvalidArgument("65000: output block too small".into()));
    }
    let hdr = data.get(pos..pos + bsize / 2).ok_or_else(|| Error::corrupt("65000: block header truncated"))?;
    let byte = |p: usize| -> u64 { data.get(p).copied().unwrap_or(0) as u64 };
    if hdr.iter().any(|&c| c & 15 > 12 || c >> 4 > 12) {
        // Raw block: groups of six big-endian 16-bit words hold eight 12-bit samples.
        let mut p = pos;
        for i in (0..bsize).step_by(8) {
            let mut raw = [0u16; 6];
            for r in raw.iter_mut() {
                *r = ((byte(p) as u16) << 8) | byte(p + 1) as u16;
                p += 2;
            }
            out[i] = (raw[0] >> 12 << 8 | raw[2] >> 12 << 4 | raw[4] >> 12) as i16;
            out[i + 1] = (raw[1] >> 12 << 8 | raw[3] >> 12 << 4 | raw[5] >> 12) as i16;
            for j in 0..6 {
                out[i + 2 + j] = (raw[j] & 0xfff) as i16;
            }
        }
        return Ok(true);
    }
    let mut p = pos + bsize / 2;
    let mut bitbuf: u64 = 0;
    let mut bits = 0u32;
    if bsize & 7 == 4 {
        bitbuf = (byte(p) << 8) | byte(p + 1);
        p += 2;
        bits = 16;
    }
    for i in 0..bsize {
        let c = hdr[i / 2];
        let l = if i & 1 == 0 { c & 15 } else { c >> 4 } as u32;
        if bits < l {
            // Two big-endian 16-bit words appended above the remaining bits.
            bitbuf |= ((byte(p) << 8 | byte(p + 1)) | (byte(p + 2) << 8 | byte(p + 3)) << 16) << bits;
            p += 4;
            bits += 32;
        }
        let mut d = (bitbuf & (0xffff >> (16 - l))) as i32;
        bitbuf >>= l;
        bits -= l;
        if l > 0 && d & (1 << (l - 1)) == 0 {
            d -= (1 << l) - 1;
        }
        out[i] = d as i16;
    }
    if p > data.len() + 4 {
        return Err(Error::corrupt("65000: block data truncated"));
    }
    Ok(false)
}

/// Locate the start of every row of a 65000 CFA payload of `width x height`
/// samples starting at `start` (sequential header scan, no entropy decoding).
pub fn scan_65000_rows(data: &[u8], start: usize, width: usize, height: usize) -> Result<Vec<usize>> {
    let mut rows = Vec::with_capacity(height);
    let mut pos = start;
    for _ in 0..height {
        rows.push(pos);
        let mut col = 0;
        while col < width {
            let len = BLOCK_65000.min(width - col);
            let (n, _) = block_len_65000(data, pos, len).ok_or_else(|| Error::corrupt("65000: data truncated"))?;
            pos += n;
            col += BLOCK_65000;
        }
    }
    Ok(rows)
}

/// Decode one CFA row of 65000 data starting at `data[pos..]`, mapping values through
pub fn decode_65000_row(data: &[u8], mut pos: usize, out: &mut [u16], curve: &[u16]) -> Result<()> {
    let width = out.len();
    let mut buf = [0i16; 272];
    let mut col = 0;
    while col < width {
        let len = BLOCK_65000.min(width - col);
        let raw = decode_block_65000(data, pos, len, &mut buf)?;
        let (n, _) = block_len_65000(data, pos, len).ok_or_else(|| Error::corrupt("65000: data truncated"))?;
        pos += n;
        let mut pred = [0i32; 2];
        for (i, o) in out[col..col + len].iter_mut().enumerate() {
            let idx = if raw {
                buf[i] as i32
            } else {
                pred[i & 1] += buf[i] as i32;
                pred[i & 1]
            };
            if (0..0xffff).contains(&idx) {
                *o = curve.get(idx as usize).copied().unwrap_or(*curve.last().unwrap_or(&0));
            }
        }
        col += BLOCK_65000;
    }
    Ok(())
}

/// DC120: unrotate one 848-byte row.
pub fn dc120_row(src: &[u8], row: usize, out: &mut [u16]) {
    const MUL: [usize; 4] = [162, 192, 187, 92];
    const ADD: [usize; 4] = [0, 636, 424, 212];
    let shift = row * MUL[row & 3] + ADD[row & 3];
    let n = src.len();
    if n == 0 {
        return;
    }
    for (col, o) in out.iter_mut().enumerate() {
        *o = src[(col + shift) % n] as u16;
    }
}

/// Run `f` for the RGB of a Kodak YCbCr triple.
#[inline(always)]
fn ycc_rgb(y: i32, cb: i32, cr: i32, curve: &[u16], out: &mut [u16]) {
    let g = y - ((cb + cr + 2) >> 2);
    let rgb = [g + cr, g, g + cb];
    for (o, v) in out.iter_mut().zip(rgb) {
        *o = curve[v.clamp(0, 255) as usize];
    }
}

/// C330: one output row of `width` RGB pixels from a `2 * raw_width`-byte YUYV-like row.
pub fn c330_row(pixel: &[u8], width: usize, curve: &[u16], out: &mut [u16]) {
    for col in 0..width {
        let at = |i: usize| pixel.get(i).copied().unwrap_or(0) as i32;
        let y = at(col * 2);
        let cb = at(((col * 2) & !3) | 1) - 128;
        let cr = at(((col * 2) & !3) | 3) - 128;
        ycc_rgb(y, cb, cr, curve, &mut out[col * 3..col * 3 + 3]);
    }
}

/// C603: one output row from a `3 * width`-byte record holding two luma rows and one
/// chroma row (`odd` selects the second luma row).
pub fn c603_row(pixel: &[u8], width: usize, odd: bool, curve: &[u16], out: &mut [u16]) {
    let at = |i: usize| pixel.get(i).copied().unwrap_or(0) as i32;
    for col in 0..width {
        let y = at(width * 2 * odd as usize + col);
        let cb = at(width + (col & !1)) - 128;
        let cr = at(width + (col & !1) + 1) - 128;
        ycc_rgb(y, cb, cr, curve, &mut out[col * 3..col * 3 + 3]);
    }
}

/// compact cameras without a stored curve (inverse BT.709-style transfer).
pub fn gamma_curve(pwr: f64, ts: f64, imax: u32) -> Vec<u16> {
    let mut g = [pwr, ts, 0.0, 0.0, 0.0, 0.0];
    let mut bnd = [0.0f64, 0.0];
    bnd[(g[1] >= 1.0) as usize] = 1.0;
    if g[1] != 0.0 && (g[1] - 1.0) * (g[0] - 1.0) <= 0.0 {
        for _ in 0..48 {
            g[2] = (bnd[0] + bnd[1]) / 2.0;
            if g[0] != 0.0 {
                let k = ((g[2] / g[1]).powf(-g[0]) - 1.0) / g[0] - 1.0 / g[2] > -1.0;
                bnd[k as usize] = g[2];
            } else {
                let k = g[2] / (1.0 - 1.0 / g[2]).exp() < g[1];
                bnd[k as usize] = g[2];
            }
        }
        g[3] = g[2] / g[1];
        if g[0] != 0.0 {
            g[4] = g[2] * (1.0 / g[0] - 1.0);
        }
    }
    let mut curve = vec![0xffffu16; 0x10000];
    for (i, c) in curve.iter_mut().enumerate() {
        let r = i as f64 / imax as f64;
        if r < 1.0 {
            let v = if r < g[2] {
                r / g[1]
            } else if g[0] != 0.0 {
                ((r + g[4]) / (1.0 + g[4])).powf(1.0 / g[0])
            } else {
                ((r - 1.0) / g[2]).exp()
            };
            *c = (0x10000 as f64 * v) as u16;
        }
    }
    curve
}

/// RADC (Kodak DC40/DC50): decode `height x width` samples (both at most 512 x 768)
/// into `out` (row stride `width`). `cbpp` is the EXIF CompressedBitsPerPixel
/// numerator, which selects the quantisation of the literal-value tree.
pub fn decode_radc(data: &[u8], width: usize, height: usize, cbpp: u32, out: &mut [u16]) -> Result<()> {
    const SRC: &[i8] = &[
        1, 1, 2, 3, 3, 4, 4, 2, 5, 7, 6, 5, 7, 6, 7, 8, 1, 0, 2, 1, 3, 3, 4, 4, 5, 2, 6, 7, 7, 6, 8, 5, 8, 8, 2, 1, 2,
        3, 3, 0, 3, 2, 3, 4, 4, 6, 5, 5, 6, 7, 6, 8, 2, 0, 2, 1, 2, 3, 3, 2, 4, 4, 5, 6, 6, 7, 7, 5, 7, 8, 2, 1, 2, 4,
        3, 0, 3, 2, 3, 3, 4, 7, 5, 5, 6, 6, 6, 8, 2, 3, 3, 1, 3, 2, 3, 4, 3, 5, 3, 6, 4, 7, 5, 0, 5, 8, 2, 3, 2, 6, 3,
        0, 3, 1, 4, 4, 4, 5, 4, 7, 5, 2, 5, 8, 2, 4, 2, 7, 3, 3, 3, 6, 4, 1, 4, 2, 4, 5, 5, 0, 5, 8, 2, 6, 3, 1, 3, 3,
        3, 5, 3, 7, 3, 8, 4, 0, 5, 2, 5, 4, 2, 0, 2, 1, 3, 2, 3, 3, 4, 4, 4, 5, 5, 6, 5, 7, 4, 8, 1, 0, 2, 2, 2, -2, 1,
        -3, 1, 3, 2, -17, 2, -5, 2, 5, 2, 17, 2, -7, 2, 2, 2, 9, 2, 18, 2, -18, 2, -9, 2, -2, 2, 7, 2, -28, 2, 28, 3,
        -49, 3, -9, 3, 9, 4, 49, 5, -79, 5, 79, 2, -1, 2, 13, 2, 26, 3, 39, 4, -16, 5, 55, 6, -37, 6, 76, 2, -26, 2,
        -13, 2, 1, 3, -39, 4, 16, 5, -55, 6, -76, 6, 37,
    ];
    const PT: [i32; 12] = [0, 0, 1280, 1344, 2320, 3616, 3328, 8000, 4095, 16383, 65535, 16383];
    const COLS: usize = 386;
    if width > 768 || height > 512 || width < 4 || height % 4 != 0 || out.len() < width * height {
        return Err(Error::corrupt(format!("RADC: unsupported geometry {width}x{height}")));
    }
    // Output curve.
    let mut curve = vec![0u16; 0x10000];
    for i in (2..12).step_by(2) {
        for c in PT[i - 2]..=PT[i] {
            let v = (c - PT[i - 2]) as f32 / (PT[i] - PT[i - 2]) as f32 * (PT[i + 1] - PT[i - 1]) as f32
                + PT[i - 1] as f32
                + 0.5f32;
            curve[c as usize] = v as u16;
        }
    }
    // Lookup trees: 19 x 256 entries of (length << 8 | value).
    let mut huff = vec![0u16; 19 * 256];
    let mut s = 0usize;
    for pair in SRC.chunks_exact(2) {
        let n = 256usize >> pair[0];
        for _ in 0..n {
            if let Some(h) = huff.get_mut(s) {
                *h = ((pair[0] as u16) << 8) | (pair[1] as u8 as u16);
            }
            s += 1;
        }
    }
    let q = if cbpp == 243 { 2 } else { 3 };
    for c in 0..256usize {
        huff[18 * 256 + c] = (((8 - q) as u16) << 8) | ((c >> q << q) | 1 << (q - 1)) as u16;
    }
    let mut bits = BitPumpMsb::new(data);
    let token = |bits: &mut BitPumpMsb, tree: usize| -> i32 {
        let e = huff[tree * 256 + bits.peek(8) as usize];
        bits.skip((e >> 8) as u32);
        (e & 0xff) as u8 as i8 as i32
    };
    let mut buf = vec![2048i16; 3 * 3 * COLS];
    let at = |c: usize, y: usize, x: usize| (c * 3 + y) * COLS + x;
    let mut last = [16i32; 3];
    let half = width / 2;
    let raw = |out: &mut [u16], r: usize, x: usize, v: i32| {
        if let Some(o) = out.get_mut(r * width + x) {
            *o = v as u16;
        }
    };
    for row in (0..height).step_by(4) {
        let mut mul = [0i32; 3];
        for m in mul.iter_mut() {
            *m = bits.get(6) as i32;
        }
        if mul.contains(&0) {
            return Err(Error::corrupt("RADC: zero multiplier"));
        }
        for c in 0..3 {
            let mut val = ((0x100_0000 / last[c] + 0x7ff) >> 12) * mul[c];
            let s = if val > 65564 { 10 } else { 12 };
            let x = (1i64 << (s - 1)) - 1;
            val <<= 12 - s;
            for e in buf[c * 3 * COLS..(c + 1) * 3 * COLS].iter_mut() {
                *e = ((((*e as i64) * val as i64 + x).min(0x7FFF_FFFF) >> s) & 0xffff) as u16 as i16;
            }
            last[c] = mul[c];
            let pred = |buf: &[i16], y: usize, x: usize| -> i32 {
                if c != 0 {
                    (buf[at(c, y - 1, x)] as i32 + buf[at(c, y, x + 1)] as i32) / 2
                } else {
                    (buf[at(c, y - 1, x + 1)] as i32 + 2 * buf[at(c, y - 1, x)] as i32 + buf[at(c, y, x + 1)] as i32)
                        / 4
                }
            };
            for r in 0..=(c == 0) as usize {
                let v = (mul[c] << 7) as i16;
                buf[at(c, 1, half)] = v;
                buf[at(c, 2, half)] = v;
                let mut tree = 1usize;
                let mut col = half as i32;
                let mut guard = 0usize;
                while col > 0 {
                    guard += 1;
                    if guard > 4 * width || bits.exhausted() {
                        return Err(Error::corrupt("RADC: data truncated"));
                    }
                    let t = token(&mut bits, tree);
                    if !(0..=8).contains(&t) {
                        return Err(Error::corrupt("RADC: invalid tree token"));
                    }
                    tree = t as usize;
                    if tree != 0 {
                        col -= 2;
                        if col >= 0 {
                            for y in 1..3 {
                                for x in [col as usize + 1, col as usize] {
                                    let nv = if tree == 8 {
                                        (token(&mut bits, 18) as u8 as i32) * mul[c]
                                    } else {
                                        token(&mut bits, tree + 10) * 16 + pred(&buf, y, x)
                                    };
                                    buf[at(c, y, x)] = nv as i16;
                                }
                            }
                        }
                    } else {
                        loop {
                            let nreps = if col > 2 { token(&mut bits, 9) + 1 } else { 1 };
                            let mut rep = 0;
                            while rep < 8 && rep < nreps && col > 0 {
                                col -= 2;
                                if col >= 0 {
                                    for y in 1..3 {
                                        for x in [col as usize + 1, col as usize] {
                                            buf[at(c, y, x)] = pred(&buf, y, x) as i16;
                                        }
                                    }
                                }
                                if rep & 1 != 0 {
                                    let step = token(&mut bits, 10) << 4;
                                    for y in 1..3 {
                                        for x in [col.max(0) as usize + 1, col.max(0) as usize] {
                                            if col >= 0 {
                                                let e = &mut buf[at(c, y, x)];
                                                *e = e.wrapping_add(step as i16);
                                            }
                                        }
                                    }
                                }
                                rep += 1;
                            }
                            if nreps != 9 {
                                break;
                            }
                        }
                    }
                }
                for y in 0..2 {
                    for x in 0..half {
                        let v = ((buf[at(c, y + 1, x)] as i32) << 4) / mul[c];
                        let v = v.max(0);
                        if c != 0 {
                            raw(out, row + y * 2 + c - 1, x * 2 + 2 - c, v);
                        } else {
                            raw(out, row + r * 2 + y, x * 2 + y, v);
                        }
                    }
                }
                // Row 2 becomes row 0 (shifted by one for luma).
                let skip = (c == 0) as usize;
                let (dst, src) = (at(c, 0, 0), at(c, 2, 0));
                let tmp: Vec<i16> = buf[src..src + COLS - skip].to_vec();
                buf[dst + skip..dst + COLS].copy_from_slice(&tmp);
            }
        }
        for y in row..row + 4 {
            for x in 0..width {
                if (x + y) & 1 == 1 {
                    let r = if x > 0 { x - 1 } else { x + 1 };
                    let s = if x + 1 < width { x + 1 } else { x - 1 };
                    let g = |xx: usize| out[y * width + xx] as i32;
                    let v = ((g(x) - 2048) * 2 + (g(r) + g(s)) / 2).max(0);
                    out[y * width + x] = v as u16;
                }
            }
        }
    }
    for v in out[..width * height].iter_mut() {
        *v = curve[*v as usize];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a 65000 block (lengths + LSB-first diffs in BE 16-bit words).
    fn encode_block(diffs: &[i32]) -> Vec<u8> {
        let bsize = (diffs.len() + 3) & !3;
        let mut d = diffs.to_vec();
        d.resize(bsize, 0);
        let lens: Vec<u32> =
            d.iter().map(|&v| if v == 0 { 0 } else { 32 - v.unsigned_abs().leading_zeros() }).collect();
        let mut out: Vec<u8> = lens.chunks(2).map(|p| (p[0] | p[1] << 4) as u8).collect();
        // Bit stream: values LSB-first; serialise as BE 16-bit words, with the special
        // 2-byte prefix when bsize % 8 == 4.
        let mut bits: Vec<u8> = Vec::new();
        for (i, &v) in d.iter().enumerate() {
            let l = lens[i];
            let code = if v >= 0 { v as u32 } else { (v + (1 << l) - 1) as u32 };
            for b in 0..l {
                bits.push(((code >> b) & 1) as u8);
            }
        }
        let mut words: Vec<u16> = Vec::new();
        for chunk in bits.chunks(16) {
            let mut w = 0u16;
            for (k, &b) in chunk.iter().enumerate() {
                w |= (b as u16) << k;
            }
            words.push(w);
        }
        // Mimic the decoder's fetch granularity: emit enough words (pad to word pairs).
        words.resize(words.len() + 4, 0);
        for w in words {
            out.extend(w.to_be_bytes());
        }
        out
    }

    #[test]
    fn block_65000_round_trip() {
        for n in [256usize, 100, 8, 4, 3] {
            let diffs: Vec<i32> = (0..n).map(|i| ((i as i32 * 37) % 201) - 100).collect();
            let data = encode_block(&diffs);
            let mut out = [0i16; 272];
            let raw = decode_block_65000(&data, 0, n, &mut out).unwrap();
            assert!(!raw);
            assert_eq!(&out[..n].iter().map(|&v| v as i32).collect::<Vec<_>>(), &diffs);
        }
    }

    #[test]
    fn raw_block_and_lengths() {
        // A header nibble of 15 selects raw mode; 8 samples from 12 bytes.
        let mut data = vec![0xFFu8; 12];
        data[0] = 0x1A;
        data[1] = 0xBC;
        let mut out = [0i16; 16];
        assert!(decode_block_65000(&data, 0, 8, &mut out).unwrap());
        assert_eq!(out[2], 0xABC);
        assert_eq!(block_len_65000(&data, 0, 8), Some((12, true)));
    }

    #[test]
    fn dc120_rotation() {
        let src: Vec<u8> = (0..848u32).map(|i| (i % 251) as u8).collect();
        let mut out = vec![0u16; 848];
        dc120_row(&src, 1, &mut out);
        assert_eq!(out[0], src[192 + 636] as u16);
    }

    #[test]
    fn gamma_curve_monotonic() {
        let c = gamma_curve(0.0, 3.875, 255);
        assert_eq!(c[0], 0);
        assert!(c.windows(2).take(255).all(|w| w[0] <= w[1]));
        assert_eq!(c[255], 0xffff);
    }

    #[test]
    fn radc_garbage_no_panic() {
        let mut seed = 1u32;
        let data: Vec<u8> = (0..5000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8 | 1
            })
            .collect();
        let mut out = vec![0u16; 768 * 512];
        let _ = decode_radc(&data, 768, 512, 152, &mut out);
        assert!(decode_radc(&data, 1000, 512, 152, &mut out).is_err());
    }
}
