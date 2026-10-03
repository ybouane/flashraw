//! Pentax PEF Huffman compression (TIFF compression 65535).
//!
//! The image is a single predictive Huffman stream (see
//! [`crate::nikon_huffman`] for the shared engine): JPEG-style difference
//! categories, `hpred`/`vpred` predictors, no linearisation curve.
//!
//! The Huffman table is stored in makernote tag 0x0220 as
//! `u16 n` (number of codes = `(n + 12) & 15`), 12 bytes of padding, `n` u16 code
//! values (left-aligned in 12 bits) and `n` code lengths. Cameras without the tag use
//! the classic default table.

use crate::nikon_huffman::{DirectHuff, PredictiveParams, decode_predictive_par};
use flashraw_core::bytes::ByteOrder;
use flashraw_core::{Error, Result};

/// The default table (JPEG DHT style: 16 counts then symbols) used when a file does
/// not carry makernote tag 0x0220.
const DEFAULT_TABLE: [u8; 29] =
    [0, 2, 3, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 3, 4, 2, 5, 1, 6, 0, 7, 8, 9, 10, 11, 12];

/// Build the 12-bit direct lookup table from makernote tag 0x0220 (`meta` starts at
/// the tag value and may extend further into the file).
pub fn table_from_makernote(meta: &[u8], order: ByteOrder) -> Result<DirectHuff> {
    let rd16 = |off: usize| -> Result<u16> {
        let b = meta.get(off..off + 2).ok_or_else(|| Error::corrupt("Pentax huffman table truncated"))?;
        Ok(order.u16([b[0], b[1]]))
    };
    let dep = ((rd16(0)? as usize) + 12) & 15;
    let codes_at = 2 + 12;
    let lens_at = codes_at + dep * 2;
    let mut direct = vec![0u16; 4096];
    for c in 0..dep {
        let code = rd16(codes_at + 2 * c)? as usize;
        let len = *meta.get(lens_at + c).ok_or_else(|| Error::corrupt("Pentax huffman table truncated"))? as usize;
        // Fill every 12-bit index that starts with this code (reference semantics,
        // including its wrap-around and overwrite behaviour for odd tables).
        let span = if len >= 13 { 0 } else { 4096 >> len };
        let end = (code + span).wrapping_sub(1) & 4095;
        let mut i = code;
        while i <= end {
            direct[i] = ((len as u16) << 8) | c as u16;
            i += 1;
        }
    }
    Ok(DirectHuff::from_direct(12, direct))
}

/// The default table.
pub fn default_table() -> Result<DirectHuff> {
    DirectHuff::from_counts(&DEFAULT_TABLE)
}

/// Decode a Pentax Huffman payload into `out` (rows of `stride` samples).
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
) -> Result<bool> {
    let p = PredictiveParams { width, rows: height, tables: &[(0, table)], vpred: [[0; 2]; 2] };
    decode_predictive_par(src, &p, out, stride, |h| h, parallel, declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makernote_table_roundtrip() {
        // Two codes: '0' (len 1, symbol 0) and '1' (len 1, symbol 1): n = 2 → stored as 2 - 12 & 15 = 6.
        let mut meta = vec![0u8, 6];
        meta.extend_from_slice(&[0; 12]);
        meta.extend_from_slice(&[0x00, 0x00, 0x08, 0x00]); // codes 0x000, 0x800
        meta.extend_from_slice(&[1, 1]);
        let t = table_from_makernote(&meta, ByteOrder::Big).unwrap();
        // Stream: symbol 1 (code '1') + diff bit '1' (=+1), then symbol 0 (code '0') x3.
        let data = [0b1100_0000u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut out = vec![0u16; 4];
        decode(&data, &t, 4, 1, &mut out, 4, false, 0).unwrap();
        assert_eq!(out, vec![1, 0, 1, 0]);
    }

    #[test]
    fn truncated_tables_are_errors() {
        assert!(table_from_makernote(&[0, 6, 0], ByteOrder::Big).is_err());
        assert!(table_from_makernote(&[], ByteOrder::Little).is_err());
        assert!(default_table().is_ok());
    }

    #[test]
    fn garbage_streams_do_not_panic() {
        let t = default_table().unwrap();
        for len in [0usize, 1, 7, 100] {
            let data = vec![0x5Au8; len];
            let mut out = vec![0u16; 64 * 8];
            decode(&data, &t, 64, 8, &mut out, 64, false, 0).unwrap();
        }
    }
}
