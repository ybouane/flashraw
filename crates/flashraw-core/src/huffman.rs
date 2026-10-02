//! Canonical Huffman decoding (JPEG DHT style) with lookup-table acceleration.
//!
//! Tables are described the JPEG way: `counts[i]` codes of length `i + 1` followed
//! by the symbol values in code order. Most RAW codecs (lossless JPEG, Nikon,
//! Pentax, Sony ARW1, Kodak, Hasselblad, Leaf, Samsung...) use this representation
//! with symbols that are the bit-length of a difference value.
//!
//! The decoder uses a single `LUT_BITS`-bit lookup. Each entry either
//! - holds the *fully decoded* difference (code + diff bits fit in the LUT), or
//! - holds the code length and symbol (diff bits are then read separately), or
//! - flags a long code that falls back to the canonical slow path.

use crate::bits::{BitRead, extend_diff};
use crate::{Error, Result};

const LUT_BITS: u32 = 12;
const LUT_SIZE: usize = 1 << LUT_BITS;

// Entry layout (u32):
//   bits 0..5   : total bits consumed (code length, or code+diff length when FULL)
//   bit  5      : FULL flag (payload is the decoded diff)
//   bit  6      : VALID flag
//   bits 8..16  : symbol
//   bits 16..32 : decoded diff (i16) when FULL
const FLAG_FULL: u32 = 1 << 5;
const FLAG_VALID: u32 = 1 << 6;

/// A canonical Huffman table with fast lookup.
#[derive(Clone)]
pub struct HuffTable {
    lut: Box<[u32; LUT_SIZE]>,
    /// For the slow path: max code (exclusive upper bound, left-aligned per length), per length 1..=16.
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [u32; 17],
    values: Vec<u8>,
    /// Lossless JPEG/DNG quirk: a length-16 symbol means diff = -32768 with no extra bits.
    pub dng_bug16: bool,
    /// Whether diff bits follow the symbol (false: the symbol value itself is the result).
    pub has_diffs: bool,
    max_len: u32,
}

impl std::fmt::Debug for HuffTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HuffTable").field("values", &self.values).field("max_len", &self.max_len).finish()
    }
}

impl HuffTable {
    /// Build from JPEG-style `counts` (codes per length 1..=16) and `values`.
    pub fn from_counts(counts: &[u8], values: &[u8], has_diffs: bool) -> Result<Self> {
        if counts.len() > 16 {
            return Err(Error::corrupt("huffman table with code lengths > 16"));
        }
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        if total == 0 || total > values.len() || total > 256 {
            return Err(Error::corrupt(format!("invalid huffman table ({total} codes, {} values)", values.len())));
        }
        let values = values[..total].to_vec();
        // Generate canonical codes.
        let mut maxcode = [-1i32; 18];
        let mut valptr = [0i32; 17];
        let mut mincode = [0u32; 17];
        let mut code: u32 = 0;
        let mut k = 0usize;
        let mut max_len = 0;
        let mut codes: Vec<(u32, u32, u8)> = Vec::with_capacity(total); // (code, len, symbol)
        for len in 1..=16usize {
            let n = counts.get(len - 1).copied().unwrap_or(0) as usize;
            if n > 0 {
                valptr[len] = k as i32;
                mincode[len] = code;
                for _ in 0..n {
                    if code >= (1u32 << len) {
                        return Err(Error::corrupt("over-subscribed huffman table"));
                    }
                    codes.push((code, len as u32, values[k]));
                    code += 1;
                    k += 1;
                }
                maxcode[len] = code as i32 - 1;
                max_len = len as u32;
            }
            code <<= 1;
        }
        maxcode[17] = i32::MAX;
        let mut lut = Box::new([0u32; LUT_SIZE]);
        for &(code, len, sym) in &codes {
            if len > LUT_BITS {
                continue;
            }
            let shift = LUT_BITS - len;
            let base = (code << shift) as usize;
            for fill in 0..(1usize << shift) {
                let idx = base | fill;
                let mut e = len | FLAG_VALID | ((sym as u32) << 8);
                if has_diffs {
                    let dlen = sym as u32;
                    if dlen == 0 {
                        e |= FLAG_FULL;
                    } else if dlen < 16 && len + dlen <= LUT_BITS {
                        // The remaining LUT bits contain the diff bits.
                        let dbits = ((idx as u32) >> (LUT_BITS - len - dlen)) & ((1 << dlen) - 1);
                        let diff = extend_diff(dbits, dlen);
                        e = (len + dlen)
                            | FLAG_VALID
                            | FLAG_FULL
                            | ((sym as u32) << 8)
                            | ((diff as i16 as u16 as u32) << 16);
                    }
                }
                lut[idx] = e;
            }
        }
        Ok(HuffTable { lut, maxcode, valptr, mincode, values, dng_bug16: false, has_diffs, max_len })
    }

    /// Build from a list of `(code_length, symbol)` pairs in canonical order
    /// (helper for vendors that specify tables as code lengths).
    pub fn from_lengths(lengths: &[(u8, u8)], has_diffs: bool) -> Result<Self> {
        let mut counts = [0u8; 16];
        let mut sorted: Vec<(u8, u8)> = lengths.to_vec();
        sorted.sort_by_key(|&(l, _)| l);
        let mut values = Vec::with_capacity(sorted.len());
        for &(l, s) in &sorted {
            if l == 0 || l > 16 {
                return Err(Error::corrupt("invalid huffman code length"));
            }
            counts[l as usize - 1] += 1;
            values.push(s);
        }
        Self::from_counts(&counts, &values, has_diffs)
    }

    /// Maximum code length in this table.
    pub fn max_len(&self) -> u32 {
        self.max_len
    }

    #[cold]
    fn slow_symbol<B: BitRead>(&self, bits: &mut B) -> Result<(u32, u32)> {
        // Canonical decode bit by bit beyond LUT_BITS.
        bits.fill();
        let peek = bits.peek_no_fill(16);
        for len in 1..=16u32 {
            let code = (peek >> (16 - len)) as i32;
            if self.maxcode[len as usize] >= 0
                && code <= self.maxcode[len as usize]
                && code as u32 >= self.mincode[len as usize]
            {
                let idx = self.valptr[len as usize] + code - self.mincode[len as usize] as i32;
                let sym = *self.values.get(idx as usize).ok_or_else(|| Error::corrupt("bad huffman index"))?;
                return Ok((sym as u32, len));
            }
        }
        Err(Error::corrupt("invalid huffman code"))
    }

    /// Decode one symbol (no diff handling).
    #[inline(always)]
    pub fn decode_symbol<B: BitRead>(&self, bits: &mut B) -> Result<u32> {
        bits.fill();
        let e = self.lut[bits.peek_no_fill(LUT_BITS) as usize];
        if e & FLAG_VALID != 0 {
            let sym = (e >> 8) & 0xFF;
            // When FULL, the LUT length includes the `sym` diff bits; skip only the code.
            let code_len = if e & FLAG_FULL != 0 { (e & 0x1F) - sym } else { e & 0x1F };
            bits.skip_no_fill(code_len);
            return Ok(sym);
        }
        let (sym, len) = self.slow_symbol(bits)?;
        bits.skip_no_fill(len);
        Ok(sym)
    }

    /// Decode a symbol giving a bit length, then read and sign-extend that many diff bits.
    #[inline(always)]
    pub fn decode_diff<B: BitRead>(&self, bits: &mut B) -> Result<i32> {
        bits.fill();
        let e = self.lut[bits.peek_no_fill(LUT_BITS) as usize];
        if e & FLAG_FULL != 0 {
            bits.skip_no_fill(e & 0x1F);
            return Ok((e >> 16) as u16 as i16 as i32);
        }
        let (sym, len) = if e & FLAG_VALID != 0 { ((e >> 8) & 0xFF, e & 0x1F) } else { self.slow_symbol(bits)? };
        bits.skip_no_fill(len);
        Ok(self.read_diff(bits, sym))
    }

    /// Read the diff for a given length symbol (handles the 16-bit special case).
    #[inline(always)]
    pub fn read_diff<B: BitRead>(&self, bits: &mut B, len: u32) -> i32 {
        match len {
            0 => 0,
            16 if self.dng_bug16 => -32768,
            16 => {
                // Plain 16-bit diff: no sign-extension ambiguity beyond 16 bits.
                let v = bits.get(16);
                extend_diff(v, 16)
            }
            l if l > 16 => {
                // Out-of-spec lengths (seen in corrupt data); read and clamp safely.
                let v = bits.get(16);
                extend_diff(v, 16)
            }
            l => {
                let v = bits.get(l);
                extend_diff(v, l)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::BitPumpMsb;

    struct Writer {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }
    impl Writer {
        fn put(&mut self, v: u32, len: u32) {
            for i in (0..len).rev() {
                self.acc = (self.acc << 1) | ((v >> i) & 1) as u64;
                self.n += 1;
                if self.n == 8 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            while self.n != 0 {
                self.put(0, 1);
            }
            self.out.extend_from_slice(&[0; 8]);
            self.out
        }
    }

    #[test]
    fn roundtrip_diffs_including_long_codes() {
        // Table with codes up to length 16 to exercise the slow path.
        let counts = [0u8, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
        let values: Vec<u8> = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let t = HuffTable::from_counts(&counts, &values, true).unwrap();
        // Rebuild canonical codes for encoding.
        let mut codes = vec![];
        let mut code = 0u32;
        let mut k = 0;
        for len in 1..=16u32 {
            for _ in 0..counts[len as usize - 1] {
                codes.push((values[k], code, len));
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        let diffs: Vec<i32> =
            (0..500).map(|i: i32| ((i * 7919) % 20001) - 10000).map(|d| d / (1 + (d.abs() % 7))).collect();
        let mut w = Writer { out: vec![], acc: 0, n: 0 };
        for &d in &diffs {
            let len = if d == 0 { 0 } else { 32 - d.unsigned_abs().leading_zeros() };
            let (_, c, l) = codes.iter().find(|(s, _, _)| *s as u32 == len).unwrap();
            w.put(*c, *l);
            if len > 0 {
                let v = if d < 0 { (d - 1) as u32 & ((1 << len) - 1) } else { d as u32 };
                w.put(v, len);
            }
        }
        let data = w.finish();
        let mut p = BitPumpMsb::new(&data);
        for &d in &diffs {
            assert_eq!(t.decode_diff(&mut p).unwrap(), d);
        }
    }

    #[test]
    fn rejects_bad_tables() {
        assert!(HuffTable::from_counts(&[3], &[0, 1, 2], true).is_err());
        assert!(HuffTable::from_counts(&[0; 16], &[], true).is_err());
    }
}
