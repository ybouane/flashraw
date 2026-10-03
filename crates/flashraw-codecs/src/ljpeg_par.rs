//! Parallel decoding of a single lossless-JPEG scan.
//!
//! A scan without restart markers (every CR2, single-strip DNGs) is one long Huffman
//! stream. [`crate::par_entropy`] decodes its differences on all threads by
//! speculation; this module adapts lossless JPEG to it (unstuffed MSB stream, one
//! difference per symbol, table per MCU position) and rebuilds samples:
//! predictor-1 rows by a parallel prefix sum after chaining the first sample of every
//! row, sRAW rows in parallel after chaining the first-column predictors, other
//! predictors row by row. Whenever speculation cannot prove the result identical to the
//! sequential decoder, the caller is told to decode sequentially.

use crate::ljpeg::LjpegHeader;
use crate::par_entropy::{self, EndCheck, SymbolStream};
use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result, par};

/// Frames smaller than this decode sequentially (speculation has a fixed cost).
pub(crate) const PARALLEL_MIN_SAMPLES: usize = 4 << 20;
/// Sequential decoding tolerates this many bits consumed past the end of the scan
/// (`past_end() <= 2` bytes at the last row).
const END_SLACK_BITS: u64 = 16;

/// The unstuffed scan of a lossless JPEG stream.
struct Scan<'a> {
    stream: &'a [u8],
    tables: &'a [HuffTable],
    period: usize,
}

impl SymbolStream for Scan<'_> {
    type Pump<'p>
        = BitPumpMsb<'p>
    where
        Self: 'p;
    const WIDTH: usize = 1;
    fn cycle(&self) -> usize {
        self.tables.len()
    }
    fn period(&self) -> usize {
        self.period
    }
    fn total_bits(&self) -> u64 {
        self.stream.len() as u64 * 8
    }
    fn end_check(&self, pos: u64) -> EndCheck {
        // `past_end` counts whole bytes consumed past the data, which is exactly this.
        if pos <= self.total_bits() + END_SLACK_BITS { EndCheck::Valid } else { EndCheck::Invalid }
    }
    fn pump_at(&self, bit: u64) -> BitPumpMsb<'_> {
        BitPumpMsb::at_bit(self.stream, bit)
    }
    fn position(p: &BitPumpMsb<'_>) -> u64 {
        p.bit_position()
    }
    #[inline(always)]
    fn decode(&self, p: &mut BitPumpMsb<'_>, phase: usize, out: &mut [u16]) -> bool {
        let before = p.bit_position();
        p.fill();
        let mut err = false;
        out[0] = self.tables[phase].decode_diff_buffered(p, &mut err) as u16;
        if err && p.bit_position() == before {
            p.skip_no_fill(1);
        }
        !err
    }
}

/// Decode the whole frame of a restart-free scan into `frame` (`height` rows of
/// [`LjpegHeader::row_len`] samples) using all worker threads.
///
/// Returns `Ok(false)` without a usable result when the stream does not qualify or
/// cannot be decoded speculatively; the caller then uses [`crate::ljpeg::decode_rows`]
/// (which also produces the exact error for corrupt data).
pub fn decode_frame_parallel(data: &[u8], hdr: &LjpegHeader, frame: &mut [u16]) -> Result<bool> {
    decode_frame_chunked(data, hdr, hdr.height as usize, frame, None)
}

/// [`decode_frame_parallel`] for the first `rows` rows only: data after them is not
/// required to be valid.
pub(crate) fn decode_frame_rows(data: &[u8], hdr: &LjpegHeader, rows: usize, frame: &mut [u16]) -> Result<bool> {
    decode_frame_chunked(data, hdr, rows.min(hdr.height as usize), frame, None)
}

pub(crate) fn decode_frame_chunked(
    data: &[u8],
    hdr: &LjpegHeader,
    height: usize,
    frame: &mut [u16],
    chunk_bytes: Option<usize>,
) -> Result<bool> {
    let clrs = hdr.clrs as usize;
    let rl = hdr.row_len();
    let total = rl.checked_mul(height).ok_or_else(|| Error::corrupt("lossless JPEG: frame too large"))?;
    if hdr.restart_interval != 0 || clrs == 0 || clrs > 6 || frame.len() < total {
        return Ok(false);
    }
    let Some(scan) = data.get(hdr.scan_offset..) else { return Ok(false) };
    let un = crate::ljpeg::unstuff_scan(scan, false);
    if un.segments.len() != 1 {
        return Ok(false);
    }
    let stream = un.segment(0);
    let chunk = match chunk_bytes.or_else(|| par_entropy::default_chunk(stream.len())) {
        Some(c) => c,
        None => return Ok(false),
    };
    let src = Scan { stream, tables: &hdr.tables, period: phase_period(&hdr.table_ids) };
    let Some(diffs) = par_entropy::decode(&src, total, chunk) else { return Ok(false) };

    if hdr.predictor == 1 && (hdr.sraw != 0 || clrs > 4) {
        // Predictor 1 with sRAW layouts: a row depends on the rows above only through the
        // first-column predictors (`vpred`), updated by the first MCU's differences of
        // positions not predicted within the row. Chain those, then rebuild rows in parallel.
        let sraw = hdr.sraw as usize;
        let init = 1u16 << (hdr.bits - 1);
        let mut starts = vec![[init; 6]; height];
        let mut v = [init; 6];
        for (r, st) in starts.iter_mut().enumerate() {
            *st = v;
            for c in (0..clrs).filter(|&c| c == 0 || c > sraw) {
                v[c] = v[c].wrapping_add(diffs.get(r * rl + c));
            }
        }
        let rows_per_task = height.div_ceil(par::threads().max(1) * 4).max(1);
        let failed = std::sync::atomic::AtomicBool::new(false);
        par::for_each_row_chunk(&mut frame[..total], rl, rows_per_task, true, |r0, rows| {
            let mut buf = vec![0u16; rl];
            for (i, row) in rows.chunks_exact_mut(rl).enumerate() {
                let r = r0 + i;
                diffs.copy_to(r * rl, &mut buf);
                let mut vpred = starts[r];
                let mut k = 0usize;
                let next = |_c: usize| {
                    let d = buf[k] as i16 as i32;
                    k += 1;
                    Ok(d)
                };
                // Row 0 semantics: predictor 1 never reads the previous row.
                if crate::ljpeg::reconstruct_row(hdr, next, row, &[], 0, clrs, &mut vpred).is_err() {
                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
        return Ok(!failed.into_inner());
    }
    if hdr.predictor != 1 || hdr.sraw != 0 || clrs > 4 {
        // Other predictors and sRAW: rows depend on the previous row or on values decoded
        // earlier in the row, so they are rebuilt in order (entropy decoding, the costly
        // part, already ran in parallel).
        let init = 1u16 << (hdr.bits - 1);
        let mut vpred = [init; 6];
        let mut buf = vec![0u16; rl];
        for r in 0..height {
            diffs.copy_to(r * rl, &mut buf);
            let (done, rest) = frame.split_at_mut(r * rl);
            let prev = if r > 0 { &done[(r - 1) * rl..] } else { &[][..] };
            let mut k = 0usize;
            let next = |_c: usize| {
                // Differences are kept modulo 2^16, which is all reconstruction needs.
                let d = buf[k] as i16 as i32;
                k += 1;
                Ok(d)
            };
            crate::ljpeg::reconstruct_row(hdr, next, &mut rest[..rl], prev, r, clrs, &mut vpred)?;
        }
        return Ok(true);
    }
    // Pass 3: chain the first sample of every row, then fill rows in parallel.

    let init = 1u16 << (hdr.bits - 1);
    let mut starts = vec![0u16; height * clrs];
    let mut v = [init; 4];
    for (r, s) in starts.chunks_exact_mut(clrs).enumerate() {
        for c in 0..clrs {
            v[c] = v[c].wrapping_add(diffs.get(r * rl + c));
            s[c] = v[c];
        }
    }
    let rows_per_task = height.div_ceil(par::threads().max(1) * 4).max(1);
    par::for_each_row_chunk(&mut frame[..total], rl, rows_per_task, true, |r0, rows| {
        for (i, row) in rows.chunks_exact_mut(rl).enumerate() {
            let r = r0 + i;
            diffs.copy_to(r * rl, row);
            row[..clrs].copy_from_slice(&starts[r * clrs..(r + 1) * clrs]);
            match clrs {
                1 => prefix::<1>(row),
                2 => prefix::<2>(row),
                3 => prefix::<3>(row),
                _ => prefix::<4>(row),
            }
        }
    });
    Ok(true)
}

/// Smallest `p` with `ids[i] == ids[i % p]` (table sequence repeats every `p` symbols).
fn phase_period(ids: &[u8]) -> usize {
    let n = ids.len().max(1);
    (1..=n).find(|&p| n % p == 0 && (0..n).all(|i| ids[i] == ids[i % p])).unwrap_or(n)
}

#[inline(always)]
fn prefix<const N: usize>(row: &mut [u16]) {
    let mut p: [u16; N] = std::array::from_fn(|c| row[c]);
    for px in row[N..].chunks_exact_mut(N) {
        for c in 0..N {
            p[c] = p[c].wrapping_add(px[c]);
            px[c] = p[c];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ljpeg::{self, LjpegOptions, encoder::encode};

    fn pseudo(n: usize, bits: u32, seed: u32) -> Vec<u16> {
        let mut x = seed;
        let mut v = 1u32 << (bits - 1);
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                // Smooth-ish data with occasional jumps: realistic code-length mix.
                let step = ((x >> 16) & 0xff) as i32 - 128;
                let step = if (x >> 8) & 31 == 0 { step * 40 } else { step / 8 };
                v = (v as i32 + step).clamp(0, (1 << bits) - 1) as u32;
                v as u16
            })
            .collect()
    }

    fn sequential(data: &[u8], hdr: &LjpegHeader) -> Vec<u16> {
        let mut out = Vec::new();
        ljpeg::decode_rows(data, hdr, |_, r| {
            out.extend_from_slice(r);
            Ok(())
        })
        .unwrap();
        out
    }

    #[test]
    fn matches_sequential_for_many_chunkings() {
        let cases = [(1usize, 12u32, 1u32), (2, 14, 1), (3, 12, 1), (4, 14, 1), (2, 14, 6), (1, 12, 7)];
        let cases = cases.into_iter().chain((2..=5).map(|p| (2, 14, p)));
        for (comps, bits, psv) in cases {
            let (w, h) = (97usize, 61usize);
            let samples = pseudo(w * h * comps, bits, comps as u32 * 7 + psv);
            let enc = encode(w, h, comps, bits, psv, &samples);
            let hdr = LjpegHeader::parse(&enc, LjpegOptions::default()).unwrap();
            let want = sequential(&enc, &hdr);
            assert_eq!(want, samples);
            for chunk in [7usize, 64, 333, 1000, 5000, 1 << 20] {
                let mut frame = vec![0u16; want.len()];
                let ok = decode_frame_chunked(&enc, &hdr, hdr.height as usize, &mut frame, Some(chunk)).unwrap();
                assert!(ok, "comps {comps} psv {psv} chunk {chunk}: no synchronisation");
                assert_eq!(frame, want, "comps {comps} psv {psv} chunk {chunk}");
            }
        }
    }

    #[test]
    fn truncated_stream_is_left_to_the_sequential_decoder() {
        let (w, h) = (64usize, 40usize);
        let samples = pseudo(w * h * 2, 14, 3);
        let enc = encode(w, h, 2, 14, 1, &samples);
        let hdr = LjpegHeader::parse(&enc, LjpegOptions::default()).unwrap();
        let cut = &enc[..hdr.scan_offset + (enc.len() - hdr.scan_offset) / 2];
        let mut frame = vec![0u16; w * h * 2];
        assert!(!decode_frame_chunked(cut, &hdr, hdr.height as usize, &mut frame, Some(100)).unwrap());
    }

    #[test]
    fn phase_periods() {
        assert_eq!(phase_period(&[0]), 1);
        assert_eq!(phase_period(&[0, 0]), 1);
        assert_eq!(phase_period(&[0, 1]), 2);
        assert_eq!(phase_period(&[0, 1, 0, 1]), 2);
        assert_eq!(phase_period(&[0, 1, 1]), 3);
    }
}
