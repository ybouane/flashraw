//! Sony ARW1 (DSLR-A100 .. A390 era) Huffman-coded column stream.
//!
//! The whole image is a single MSB-first bit stream of JPEG-style difference codes
//! (a length symbol from a fixed 18-entry prefix code, then that many diff bits).
//! Samples are accumulated in one running sum and laid out column by column, from
//! the rightmost column to the leftmost; within a column the even rows come first,
//! then the odd rows. The stream is inherently sequential.

use flashraw_core::bits::{BitPumpMsb, BitRead};
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
    let mut bits = BitPumpMsb::new(src);
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
    for col in (0..width).rev() {
        for &row in &order {
            bits.fill();
            let e = lut[bits.peek_no_fill(15) as usize];
            bits.skip_no_fill((e >> 8) as u32);
            let len = (e & 0xff) as u32;
            let diff = match len {
                0 => 0,
                16 => -32768,
                _ => {
                    let v = bits.get(len) as i32;
                    if v & (1 << (len - 1)) == 0 { v - ((1 << len) - 1) } else { v }
                }
            };
            sum = sum.wrapping_add(diff);
            if sum >> 12 != 0 {
                bad += 1;
            }
            if row < stored_rows {
                out[row * width + col] = sum as u16;
            }
        }
        if bits.exhausted() {
            return Err(Error::corrupt(format!("ARW1 stream truncated at column {col}")));
        }
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
