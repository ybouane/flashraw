//! Parallel decoding of a single lossless-JPEG scan.
//!
//! A scan without restart markers (every CR2, single-strip DNGs) is one long Huffman
//! stream. [`crate::par_entropy`] decodes its differences on all threads by
//! speculation; this module adapts lossless JPEG to it (unstuffed MSB stream, one
//! difference per symbol, table per MCU position) and rebuilds samples:
//! predictor-1 rows by a parallel prefix sum after chaining the first sample of every
//! row, sRAW rows in parallel after chaining the first-column predictors, predictors
//! 2-7 by a wavefront over bands of rows and blocks of columns. Whenever speculation cannot prove the result identical to the
//! sequential decoder, the caller is told to decode sequentially.
//!
//! Scans with restart markers need no speculation: every restart interval is its own bit
//! stream with reset predictors, so intervals are decoded one per task
//! ([`decode_frame_restarts`]).

use crate::ljpeg::LjpegHeader;
use crate::par_entropy::{self, EndCheck, SymbolStream};
use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result, par};

/// Frames smaller than this decode sequentially (speculation has a fixed cost).
pub(crate) const PARALLEL_MIN_SAMPLES: usize = 4 << 20;
/// [`PARALLEL_MIN_SAMPLES`] for lossless JPEG frames (CR2 incl. sRAW, DNG, Canon TIF):
/// their unstuffing and speculation already pay off from about a million samples.
pub(crate) const LJPEG_PARALLEL_MIN_SAMPLES: usize = 1 << 20;
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
    if hdr.restart_interval != 0 {
        return decode_frame_restarts(data, hdr, height, frame);
    }
    if clrs == 0 || clrs > 6 || frame.len() < total {
        return Ok(false);
    }
    let Some(scan) = data.get(hdr.scan_offset..) else { return Ok(false) };
    if chunk_bytes.is_none() && par_entropy::default_chunk(scan.len()).is_none() {
        // Too short to split (the unstuffed stream is shorter still).
        return Ok(false);
    }
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
    if hdr.sraw == 0 && (2..=7).contains(&hdr.predictor) && height > 1 {
        // Two-dimensional predictors: differences go into the frame, then rows are rebuilt
        // in place by a wavefront over bands of rows and blocks of columns.
        let rows_per_task = height.div_ceil(par::threads().max(1) * 4).max(1);
        par::for_each_row_chunk(&mut frame[..total], rl, rows_per_task, true, |r0, rows| {
            diffs.copy_to(r0 * rl, rows);
        });
        rebuild_2d(hdr, &mut frame[..total], rl);
        return Ok(true);
    }
    if hdr.predictor != 1 || hdr.sraw != 0 || clrs > 4 {
        // Remaining predictor/sRAW combinations: rows depend on the previous row or on
        // values decoded earlier in the row, so they are rebuilt in order (entropy decoding, the costly
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

/// Decode the first `rows` rows of a scan with restart markers, one restart interval per
/// task. An interval starts a fresh bit stream with reset predictors, so with predictor 1
/// (which never reads the row above) intervals are independent. Each one runs the
/// sequential row decoder over its own segment with the same checks; any failure, or a
/// layout outside that case, returns `Ok(false)` so the sequential decoder reports it.
pub(crate) fn decode_frame_restarts(data: &[u8], hdr: &LjpegHeader, rows: usize, frame: &mut [u16]) -> Result<bool> {
    let rl = hdr.row_len();
    let rows = rows.min(hdr.height as usize);
    let total = rl.checked_mul(rows).ok_or_else(|| Error::corrupt("lossless JPEG: frame too large"))?;
    let fast = crate::ljpeg::fast_rows(hdr);
    let restart = hdr.restart_interval as usize;
    let clrs = hdr.clrs as usize;
    if restart == 0 || rl == 0 || clrs == 0 || clrs > 6 || frame.len() < total || !(fast || hdr.predictor == 1) {
        return Ok(false);
    }
    // Restarts are honoured at row starts only, exactly where the sequential decoder does.
    let width = hdr.width as usize;
    let starts: Vec<usize> = (0..rows).filter(|&r| (r * width) % restart == 0).collect();
    if starts.len() < 2 {
        return Ok(false);
    }
    let Some(scan) = data.get(hdr.scan_offset..) else { return Ok(false) };
    let un = crate::ljpeg::unstuff_scan(scan, true);
    if un.segments.len() < starts.len() {
        return Ok(false);
    }
    let bounds: Vec<(usize, usize)> = starts
        .iter()
        .zip(starts.iter().skip(1).chain(std::iter::once(&rows)))
        .map(|(&a, &b)| (a * rl, b * rl))
        .collect();
    let Some(parts) = par::split_disjoint(&mut frame[..total], &bounds) else { return Ok(false) };
    let init = 1u16 << (hdr.bits - 1);
    let ok = par::map(parts.into_iter().enumerate().collect(), true, |(k, part): (usize, &mut [u16])| {
        let mut bits = BitPumpMsb::new(un.segment(k));
        let mut vpred = [init; 6];
        // Predictor 1 never uses the row above (only the generic decoder indexes it).
        let zeros = if fast { Vec::new() } else { vec![0u16; rl] };
        let mut rest = part;
        let mut jrow = starts[k];
        while !rest.is_empty() {
            let (row, tail) = std::mem::take(&mut rest).split_at_mut(rl);
            if crate::ljpeg::decode_row(hdr, fast, &mut bits, row, &zeros, jrow, &mut vpred).is_err() {
                return false;
            }
            rest = tail;
            jrow += 1;
        }
        true
    });
    Ok(ok.into_iter().all(|v| v))
}

/// Rebuild a frame of predictor 2-7 rows (no sRAW) in place from the differences it
/// holds. Row 0 predicts from the left; later rows start from the sample above and
/// then use the predictor. Sample `(r, i)` depends on `(r, i - clrs)`, `(r - 1, i)` and
/// `(r - 1, i - clrs)` only, so the rows are cut into bands and the columns into
/// blocks: block `j` of band `k` can be rebuilt once block `j - 1` of the same band and
/// block `j` of the band above are done, which lets the bands run as a wavefront.
fn rebuild_2d(hdr: &LjpegHeader, frame: &mut [u16], rl: usize) {
    match hdr.predictor {
        2 => rebuild_2d_psv::<2>(hdr, frame, rl),
        3 => rebuild_2d_psv::<3>(hdr, frame, rl),
        4 => rebuild_2d_psv::<4>(hdr, frame, rl),
        5 => rebuild_2d_psv::<5>(hdr, frame, rl),
        6 => rebuild_2d_psv::<6>(hdr, frame, rl),
        _ => rebuild_2d_psv::<7>(hdr, frame, rl),
    }
}

fn rebuild_2d_psv<const PSV: u32>(hdr: &LjpegHeader, frame: &mut [u16], rl: usize) {
    let clrs = hdr.clrs as usize;
    let init = 1u16 << (hdr.bits - 1);
    let (row0, rest) = frame.split_at_mut(rl);
    for c in 0..clrs {
        row0[c] = init.wrapping_add(row0[c]);
    }
    for i in clrs..rl {
        row0[i] = row0[i - clrs].wrapping_add(row0[i]);
    }
    // Columns `a..b` of a band's rows from the row above it (`above`, columns `lo..b`).
    let geo = par::Wavefront { row_len: rl, halo: 1, margin: clrs, block: block_width(rl, clrs) };
    par::wavefront(rest, row0, geo, true, |_, band, above, lo, a, b| {
        let mut prev: &[u16] = above;
        for row in band.chunks_exact_mut(rl) {
            let cur = &mut row[lo..b];
            rebuild_window::<PSV>(cur, prev, a - lo, clrs);
            prev = cur;
        }
    });
}

/// Wavefront block width: about four blocks per thread, at least 64 MCUs.
fn block_width(rl: usize, clrs: usize) -> usize {
    (rl / clrs).div_ceil(4 * par::threads().max(1)).max(64) * clrs
}

/// Rebuild `cur[start..]` from `prev` (the same columns of the row above); both start
/// `start` samples before the first one rebuilt, which is column 0 when `start == 0`.
#[inline(always)]
fn rebuild_window<const PSV: u32>(cur: &mut [u16], prev: &[u16], start: usize, clrs: usize) {
    let n = cur.len().min(prev.len());
    let (cur, prev) = (&mut cur[..n], &prev[..n]);
    let mut i = start;
    if start == 0 {
        // Column 0 predicts from the sample above.
        while i < clrs.min(n) {
            cur[i] = prev[i].wrapping_add(cur[i]);
            i += 1;
        }
    }
    if i >= n {
        return;
    }
    match PSV {
        2 => {
            for (c, &p) in cur[i..].iter_mut().zip(&prev[i..]) {
                *c = p.wrapping_add(*c);
            }
        }
        3 => {
            for (c, &p) in cur[i..].iter_mut().zip(&prev[i - clrs..]) {
                *c = p.wrapping_add(*c);
            }
        }
        _ => {
            for i in i..n {
                let (ra, rb, rc) = (cur[i - clrs] as i32, prev[i] as i32, prev[i - clrs] as i32);
                let pred = match PSV {
                    4 => ra + rb - rc,
                    5 => ra + ((rb - rc) >> 1),
                    6 => rb + ((ra - rc) >> 1),
                    _ => (ra + rb) >> 1,
                };
                cur[i] = (pred as u16).wrapping_add(cur[i]);
            }
        }
    }
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
    fn wavefront_rebuild_matches_sequential() {
        // Wide enough for many column blocks, tall enough for one band per thread.
        for (w, h) in [(700usize, 61usize), (300, 2), (129, 5)] {
            for comps in 1..=4 {
                for psv in 2..=7 {
                    let samples = pseudo(w * h * comps, 16, comps as u32 * 3 + psv);
                    let enc = encode(w, h, comps, 16, psv, &samples);
                    let hdr = LjpegHeader::parse(&enc, LjpegOptions::default()).unwrap();
                    let want = sequential(&enc, &hdr);
                    assert_eq!(want, samples);
                    let mut frame = vec![0u16; want.len()];
                    let ok = decode_frame_chunked(&enc, &hdr, h, &mut frame, Some(500)).unwrap();
                    assert!(ok, "{w}x{h} comps {comps} psv {psv}");
                    assert_eq!(frame, want, "{w}x{h} comps {comps} psv {psv}");
                }
            }
        }
    }

    #[test]
    fn restart_intervals_match_sequential() {
        let (w, h) = (53usize, 61usize);
        for comps in 1..=4 {
            for psv in 1..=7u32 {
                // One row, three rows, and an interval that does not divide the height.
                for rows_per_interval in [1usize, 3, 8] {
                    let samples = pseudo(w * h * comps, 14, comps as u32 * 13 + psv);
                    let enc =
                        ljpeg::encoder::encode_with_restarts(w, h, comps, 14, psv, &samples, w * rows_per_interval);
                    let hdr = LjpegHeader::parse(&enc, LjpegOptions::default()).unwrap();
                    assert_eq!(hdr.restart_interval as usize, w * rows_per_interval);
                    let want = sequential(&enc, &hdr);
                    assert_eq!(want, samples, "comps {comps} psv {psv} interval {rows_per_interval}");
                    let mut frame = vec![0u16; want.len()];
                    let ok = decode_frame_restarts(&enc, &hdr, h, &mut frame).unwrap();
                    // Only predictor 1 rows are independent of the interval before.
                    assert_eq!(ok, psv == 1, "comps {comps} psv {psv}");
                    if ok {
                        assert_eq!(frame, want, "comps {comps} interval {rows_per_interval}");
                        // A row limit decodes only the intervals it needs.
                        let mut part = vec![0u16; 20 * w * comps];
                        assert!(decode_frame_restarts(&enc, &hdr, 20, &mut part).unwrap());
                        assert_eq!(part[..], want[..part.len()]);
                    }
                    let mut via_rows = Vec::new();
                    ljpeg::decode_rows_par(&enc, &hdr, true, |_, r| {
                        via_rows.extend_from_slice(r);
                        Ok(())
                    })
                    .unwrap();
                    assert_eq!(via_rows, want);
                }
            }
        }
    }

    #[test]
    fn damaged_restart_interval_is_left_to_the_sequential_decoder() {
        let (w, h) = (64usize, 48usize);
        let samples = pseudo(w * h * 2, 12, 5);
        let enc = ljpeg::encoder::encode_with_restarts(w, h, 2, 12, 1, &samples, w * 4);
        let hdr = LjpegHeader::parse(&enc, LjpegOptions::default()).unwrap();
        let rst: Vec<usize> = (hdr.scan_offset..enc.len() - 1)
            .filter(|&i| enc[i] == 0xFF && (0xD0..=0xD7).contains(&enc[i + 1]))
            .collect();
        assert_eq!(rst.len(), h / 4 - 1);
        // Drop the middle of one interval: it runs short and the frame must fall back.
        let mut cut = enc[..(rst[3] + rst[4]) / 2].to_vec();
        cut.extend_from_slice(&enc[rst[4]..]);
        let mut frame = vec![0u16; w * h * 2];
        assert!(!decode_frame_restarts(&cut, &hdr, h, &mut frame).unwrap());
        assert!(ljpeg::decode_rows_par(&cut, &hdr, true, |_, _| Ok(())).is_err());
        // A missing marker (fewer segments than intervals) falls back too.
        let mut merged = enc[..rst[2]].to_vec();
        merged.extend_from_slice(&enc[rst[2] + 2..]);
        assert!(!decode_frame_restarts(&merged, &hdr, h, &mut frame).unwrap());
        let seq = ljpeg::decode_rows(&merged, &hdr, |_, _| Ok(()));
        let par = ljpeg::decode_rows_par(&merged, &hdr, true, |_, _| Ok(()));
        assert_eq!(seq.is_err(), par.is_err());
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
