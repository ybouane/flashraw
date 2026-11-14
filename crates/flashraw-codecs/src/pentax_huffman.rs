//! Pentax PEF Huffman compression (TIFF compression 65535).
//!
//! The strip is one MSB-first bit stream of per-sample differences coded with a single
//! prefix table, decoded by the predictive engine of [`crate::nikon_huffman`]. The table is
//! stored by the camera in makernote tag 0x0220 (or, for files without the tag, a built-in
//! default). This module builds the 12-bit direct lookup table from that tag.
//!
//! Tag layout (makernote byte order): `u16 n`, 12 ignored bytes, then `dep = n + 12` code
//! values (`u16`, 12-bit, left aligned) followed by `dep` code lengths (one byte each). Entry
//! `c` is the codeword of difference category `c`: the top `len` bits of its code value.

use crate::nikon_huffman::{DirectHuff, PredictiveParams, decode_predictive_par};
use flashraw_core::bytes::ByteOrder;
use flashraw_core::{Error, Result};

/// Width of the direct lookup, the longest codeword the camera writes.
const PEEK_BITS: u32 = 12;
/// Header bytes before the code values: the count and 12 bytes that are not used.
const HEADER_LEN: usize = 14;
/// Table sizes (`n + 12`) that occur in real files.
const ENTRY_COUNTS: std::ops::RangeInclusive<usize> = 12..=15;

/// Code values and lengths of the built-in table (identical to the K-r family's).
const DEFAULT_CODES: [u16; 13] =
    [0xf00, 0xc00, 0x800, 0x000, 0x400, 0xa00, 0xe00, 0xf80, 0xfc0, 0xfe0, 0xff0, 0xff8, 0xffc];
const DEFAULT_LENGTHS: [u8; 13] = [5, 3, 3, 2, 2, 3, 4, 6, 7, 8, 9, 10, 10];

/// Fill the direct table from `(code value, length)` pairs; entry `c` decodes to symbol `c`.
/// Entries that cannot be a codeword are skipped; later entries win on overlap. `None` when
/// no pattern ends up covered.
fn build(entries: impl Iterator<Item = (u16, u8)>) -> Option<DirectHuff> {
    let mut direct = vec![0u16; 1 << PEEK_BITS];
    let mut covered = false;
    for (symbol, (code, len)) in entries.enumerate() {
        let len = u32::from(len);
        if len == 0 || len > PEEK_BITS || symbol > 0xff {
            continue;
        }
        let span = 1usize << (PEEK_BITS - len);
        let first = usize::from(code) & !(span - 1);
        let Some(slots) = direct.get_mut(first..first + span) else { continue };
        slots.fill((len as u16) << 8 | symbol as u16);
        covered = true;
    }
    covered.then(|| DirectHuff::from_direct(PEEK_BITS, direct))
}

/// Build the Huffman table from the value of Pentax makernote tag 0x0220. `meta` starts at the
/// first byte of the tag value and may extend further into the file; `order` is the byte order
/// of the makernote.
pub fn table_from_makernote(meta: &[u8], order: ByteOrder) -> Result<DirectHuff> {
    let bad = || Error::corrupt("Pentax Huffman table is unusable");
    let count = meta.get(..2).ok_or_else(bad)?;
    let dep = usize::from(order.u16([count[0], count[1]])) + 12;
    if !ENTRY_COUNTS.contains(&dep) {
        return Err(bad());
    }
    let codes = meta.get(HEADER_LEN..HEADER_LEN + 2 * dep).ok_or_else(bad)?;
    let lengths = meta.get(HEADER_LEN + 2 * dep..HEADER_LEN + 3 * dep).ok_or_else(bad)?;
    let pairs = codes.chunks_exact(2).map(|c| order.u16([c[0], c[1]])).zip(lengths.iter().copied());
    build(pairs).ok_or_else(bad)
}

/// The table used when a file has no tag 0x0220.
pub fn default_table() -> Result<DirectHuff> {
    build(DEFAULT_CODES.iter().copied().zip(DEFAULT_LENGTHS)).ok_or_else(|| Error::corrupt("no default table"))
}

/// Decode a Pentax Huffman payload into `out` (rows of `stride` samples). Returns the row in
/// which the data ran out (`None` when the stream was complete); that row and the rows after it
/// hold no real data.
#[allow(clippy::too_many_arguments)]
pub fn decode(
    src: &[u8],
    table: &DirectHuff,
    width: usize,
    height: usize,
    out: &mut [u16],
    stride: usize,
    parallel: bool,
    declared: usize,
) -> Result<Option<usize>> {
    let params = PredictiveParams { width, rows: height, tables: &[(0, table)], vpred: [[0; 2]; 2] };
    decode_predictive_par(src, &params, out, stride, |v| v, parallel, declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(order: ByteOrder, n: u16, codes: &[u16], lengths: &[u8]) -> Vec<u8> {
        let put = |v: u16| match order {
            ByteOrder::Little => v.to_le_bytes(),
            ByteOrder::Big => v.to_be_bytes(),
        };
        let mut t = put(n).to_vec();
        t.extend([7u8; 12]);
        for &c in codes {
            t.extend(put(c));
        }
        t.extend(lengths);
        t
    }

    fn run(table: &DirectHuff, src: &[u8]) -> (Vec<u16>, Option<usize>) {
        let mut out = vec![0u16; 16 * 4];
        let dead = decode(src, table, 16, 4, &mut out, 16, false, 0).unwrap();
        (out, dead)
    }

    #[test]
    fn default_equals_k_r_tag() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            let t = tag(order, 1, &DEFAULT_CODES, &DEFAULT_LENGTHS);
            let a = table_from_makernote(&t, order).unwrap();
            let b = default_table().unwrap();
            let src: Vec<u8> = (0..300u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
            assert_eq!(run(&a, &src), run(&b, &src));
        }
    }

    #[test]
    fn decodes_known_stream() {
        // Codeword "11110" is symbol 0 (difference 0); "00" is symbol 3 followed by 3 bits
        // ("111" = +7). One row: 7, 7 (the two column predictors), then 0 for the rest.
        let t = default_table().unwrap();
        let mut bits = String::from("00111");
        bits += "00111";
        bits += &"11110".repeat(14);
        bits += &"11110".repeat(48);
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let src: Vec<u8> = bits
            .as_bytes()
            .chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect();
        let (out, dead) = run(&t, &src);
        assert_eq!(dead, None);
        // Rows 0 and 2 share predictors (7); rows 1 and 3 start from zero.
        for (r, row) in out.chunks(16).enumerate() {
            assert!(row.iter().all(|&v| v == if r % 2 == 0 { 7 } else { 0 }), "row {r}");
        }
    }

    #[test]
    fn hostile_tags_are_errors_not_panics() {
        for order in [ByteOrder::Little, ByteOrder::Big] {
            assert!(table_from_makernote(&[], order).is_err());
            assert!(table_from_makernote(&[1], order).is_err());
            assert!(table_from_makernote(&[0, 1, 0, 0], order).is_err());
            let full = tag(order, 1, &DEFAULT_CODES, &DEFAULT_LENGTHS);
            for cut in 0..full.len() {
                assert!(table_from_makernote(&full[..cut], order).is_err());
            }
            assert!(table_from_makernote(&tag(order, 9, &[0; 21], &[3; 21]), order).is_err());
            assert!(table_from_makernote(&tag(order, 65535, &[0; 13], &[3; 13]), order).is_err());
            // Nothing usable: lengths 0 and 13..
            assert!(table_from_makernote(&tag(order, 1, &[0; 13], &[0; 13]), order).is_err());
            assert!(table_from_makernote(&tag(order, 1, &[0xfff; 13], &[200; 13]), order).is_err());
            // Garbage codes still build or fail cleanly.
            let _ = table_from_makernote(&tag(order, 3, &[0xffff; 15], &[1; 15]), order);
        }
    }
}
