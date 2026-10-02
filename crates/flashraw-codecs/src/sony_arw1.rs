//! Sony ARW1 (DSLR-A100 .. A390 era) Huffman-coded column stream.
//!
//! The whole image is a single MSB-first bit stream of JPEG-style difference codes
//! (a length symbol from a fixed 18-entry prefix code, then that many diff bits).
//! Samples are accumulated in one running sum and laid out column by column, from
//! the rightmost column to the leftmost; within a column the even rows come first,
//! then the odd rows. The stream is inherently sequential.

use flashraw_core::{Error, Result};

/// Prefix code as `(code_length << 8) | diff_length`, in code-assignment order: the
/// codes are handed out from the all-zero 15-bit word upwards, longest codes first.
const TABLE: [u16; 18] = [
    0xf11, 0xf10, 0xe0f, 0xd0e, 0xc0d, 0xb0c, 0xa0b, 0x90a, 0x809, 0x708, 0x607, 0x506, 0x405, 0x304, 0x303, 0x300,
    0x202, 0x201,
];

/// 15-bit lookup table: `(code_length << 8) | diff_length` for every 15-bit prefix.
fn lut() -> Vec<u16> {
    let mut t = Vec::with_capacity(1 << 15);
    for &e in &TABLE {
        let n = 32768usize >> (e >> 8);
        t.extend(std::iter::repeat_n(e, n));
    }
    debug_assert_eq!(t.len(), 1 << 15);
    t
}

/// Decode an ARW1 stream into `out` (`width * stored_rows` samples, row-major).
///
/// `coded_rows` rows are present in the stream per column; only rows below
/// `stored_rows` are written. Returns an error if the stream is truncated or the
/// running sum leaves the 12-bit range persistently (corrupt data).
pub fn decode(src: &[u8], out: &mut [u16], width: usize, coded_rows: usize, stored_rows: usize) -> Result<()> {
    if out.len() < width * stored_rows {
        return Err(Error::InvalidArgument("ARW1 output buffer too small".into()));
    }
    let lut = lut();
    // 8-bit prefix table: entry for codes of length <= 8, 0 when the code is longer.
    let short: Vec<u16> = (0..256usize)
        .map(|b| {
            let e = lut[b << 7];
            if (e >> 8) <= 8 { e } else { 0 }
        })
        .collect();
    // Bit position into `src`; each sample peeks a 64-bit big-endian window (at
    // least 57 valid bits: 15 code bits + up to 17 difference bits fit).
    let mut pos = 0usize;
    let peek = |pos: usize| -> u64 {
        let byte = pos >> 3;
        let w = match src.get(byte..byte + 8) {
            Some(b) => u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            None => {
                let mut b = [0u8; 8];
                if let Some(t) = src.get(byte..) {
                    b[..t.len().min(8)].copy_from_slice(&t[..t.len().min(8)]);
                }
                u64::from_be_bytes(b)
            }
        };
        w << (pos & 7)
    };
    let mut sum: i32 = 0;
    let mut bad = 0usize;
    // Row visiting order within a column: even rows, then odd rows (only when the
    // row count is even, matching the reference loop).
    let order: Vec<usize> = {
        let mut v: Vec<usize> = (0..coded_rows).step_by(2).collect();
        if coded_rows % 2 == 0 {
            v.extend((1..coded_rows).step_by(2));
        }
        v
    };
    let limit = (src.len() + 8) * 8;
    // Columns are decoded into a small column-major block and transposed into the
    // row-major output a block at a time: writing one column of a large image
    // directly touches a different page for every row.
    const BLOCK: usize = 32;
    let rows_out = stored_rows.min(coded_rows);
    let mut block = vec![0u16; BLOCK * rows_out];
    let mut col_end = width;
    while col_end > 0 {
        let col_start = col_end.saturating_sub(BLOCK);
        for col in (col_start..col_end).rev() {
            let column = &mut block[(col - col_start) * rows_out..(col - col_start + 1) * rows_out];
            for &row in &order {
                let w = peek(pos);
                // Codes of up to 8 bits resolve from the top byte (L1-resident table);
                // longer codes are the rare "all leading zeros" region.
                let e = match short[(w >> 56) as usize] {
                    0 => lut[(w >> 49) as usize],
                    e => e,
                };
                let clen = (e >> 8) as u32;
                let len = (e & 0xff) as u32;
                // Branch-free JPEG-style difference: `len` bits, negative when the top
                // bit is clear; length 16 means -32768 with no difference bits.
                let v = (((w << clen) >> 1) >> (63 - len)) as i32;
                let half = (1i32 << len) >> 1;
                let d = if v < half { v - ((1 << len) - 1) } else { v };
                let is16 = len == 16;
                let diff = if is16 { -32768 } else { d };
                pos += (clen + if is16 { 0 } else { len }) as usize;
                sum = sum.wrapping_add(diff);
                bad += (sum >> 12 != 0) as usize;
                if row < rows_out {
                    column[row] = sum as u16;
                }
            }
            if pos > limit {
                return Err(Error::corrupt(format!("ARW1 stream truncated at column {col}")));
            }
        }
        let n = col_end - col_start;
        for row in 0..rows_out {
            let dst = &mut out[row * width + col_start..row * width + col_end];
            for (k, d) in dst.iter_mut().enumerate().take(n) {
                *d = block[k * rows_out + row];
            }
        }
        col_end = col_start;
    }
    // Real files never leave the 12-bit range; garbage input does so constantly.
    if bad > width * coded_rows / 64 {
        return Err(Error::corrupt("ARW1 data out of range (corrupt stream)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct W {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }
    impl W {
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = self.acc << 1 | ((v >> i) & 1) as u64;
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
    }

    /// Encoder mirroring the decoder's code assignment.
    fn encode(img: &[u16], width: usize, rows: usize) -> Vec<u8> {
        // codes: start of each table entry in the 15-bit space.
        let mut start = 0u32;
        let mut codes = vec![];
        for &e in &TABLE {
            let cl = (e >> 8) as u32;
            codes.push((start >> (15 - cl), cl, (e & 0xff) as u32));
            start += 32768 >> cl;
        }
        let mut w = W { out: vec![], acc: 0, n: 0 };
        let mut sum = 0i32;
        let mut order: Vec<usize> = (0..rows).step_by(2).collect();
        order.extend((1..rows).step_by(2));
        for col in (0..width).rev() {
            for &row in &order {
                let v = img[row * width + col] as i32;
                let d = v - sum;
                sum = v;
                let len = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
                let &(c, cl, _) = codes.iter().find(|x| x.2 == len).unwrap();
                w.put(c, cl);
                if len > 0 {
                    let bits = if d < 0 { (d - 1) as u32 & ((1 << len) - 1) } else { d as u32 };
                    w.put(bits, len);
                }
            }
        }
        w.put(0, 7);
        w.out.extend_from_slice(&[0; 16]);
        w.out
    }

    #[test]
    fn roundtrip() {
        let (w, h) = (17, 12);
        let mut x = 7u32;
        let img: Vec<u16> = (0..w * h)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                ((x >> 16) % 4096) as u16
            })
            .collect();
        let data = encode(&img, w, h);
        let mut out = vec![0u16; w * h];
        decode(&data, &mut out, w, h, h).unwrap();
        assert_eq!(out, img);
    }

    #[test]
    fn truncated_errors() {
        let (w, h) = (64, 64);
        let img: Vec<u16> = (0..w * h).map(|i| ((i * 37) % 4096) as u16).collect();
        let data = encode(&img, w, h);
        let mut out = vec![0u16; w * h];
        assert!(decode(&data[..data.len() / 3], &mut out, w, h, h).is_err());
    }
}
