//! Multi-threaded decoding of one lossless-JPEG frame.
//!
//! Without restart markers the entropy-coded data of a frame is one bit stream, so the threads
//! split it into chunks and synchronise on the symbol boundaries ([`crate::par_entropy`]). That
//! step yields the 16-bit differences of every sample in raster order; a second, row-parallel
//! step turns them into samples. With restart intervals every interval is an independent stream
//! and is decoded by the ordinary row decoder, one task per interval.
//!
//! Every function returns `Ok(true)` only when the rows in `frame` are exactly those of the
//! sequential decoder ([`crate::ljpeg::decode_rows`]); `Ok(false)` sends the caller to it.

use crate::ljpeg::{self, LjpegHeader};
use crate::par_entropy::{self, EndCheck, SymbolStream, Values};
use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result, par};
use std::sync::atomic::{AtomicBool, Ordering};

/// Frames smaller than this decode sequentially (for formats other than lossless JPEG).
pub(crate) const PARALLEL_MIN_SAMPLES: usize = 4 << 20;
/// [`PARALLEL_MIN_SAMPLES`] for lossless JPEG frames.
pub(crate) const LJPEG_PARALLEL_MIN_SAMPLES: usize = 1 << 20;

/// Bits the sequential decoder tolerates having consumed beyond the end of the data after the
/// last row (two bytes).
const END_SLACK_BITS: u64 = 16;
/// The longest symbol: a 16-bit code followed by 16 difference bits.
const MAX_SYMBOL_BITS: u64 = 32;
/// Rounds (one symbol per MCU position each) decoded between checks in the batch decoder.
const MAX_ROUNDS: usize = 16;

fn size_error() -> Error {
    Error::corrupt("lossless JPEG: frame size overflows")
}

/// Decode the whole frame of a scan into `frame` on all threads; `Ok(false)` leaves it to the
/// sequential decoder.
pub fn decode_frame_parallel(data: &[u8], hdr: &LjpegHeader, frame: &mut [u16]) -> Result<bool> {
    decode_frame_chunked(data, hdr, hdr.height as usize, frame, None)
}

/// [`decode_frame_parallel`] for the first `rows` rows only.
pub(crate) fn decode_frame_rows(data: &[u8], hdr: &LjpegHeader, rows: usize, frame: &mut [u16]) -> Result<bool> {
    decode_frame_chunked(data, hdr, rows.min(hdr.height as usize), frame, None)
}

/// [`decode_frame_parallel`] for `height` rows with an explicit chunk size in bytes.
pub(crate) fn decode_frame_chunked(
    data: &[u8],
    hdr: &LjpegHeader,
    height: usize,
    frame: &mut [u16],
    chunk_bytes: Option<usize>,
) -> Result<bool> {
    let row_len = hdr.row_len();
    let total = row_len.checked_mul(height).ok_or_else(size_error)?;
    if hdr.restart_interval != 0 {
        return decode_frame_restarts(data, hdr, height, frame);
    }
    let clrs = hdr.clrs as usize;
    if clrs == 0 || clrs > 6 || hdr.tables.len() < clrs || frame.len() < total || height > hdr.height as usize {
        return Ok(false);
    }
    if total == 0 {
        return Ok(false);
    }
    let Some(scan) = data.get(hdr.scan_offset..) else { return Ok(false) };
    if chunk_bytes.is_none() && par_entropy::default_chunk(scan.len()).is_none() {
        return Ok(false);
    }
    let unstuffed = ljpeg::unstuff_scan(scan, false);
    if unstuffed.segments.len() != 1 {
        return Ok(false);
    }
    let bytes = unstuffed.segment(0);
    let Some(chunk) = chunk_bytes.or_else(|| par_entropy::default_chunk(bytes.len())) else { return Ok(false) };
    let stream = Scan::new(hdr, bytes);
    let Some(diffs) = par_entropy::decode(&stream, total, chunk) else { return Ok(false) };
    rebuild(hdr, &diffs, height, &mut frame[..total])
}

/// Scans with restart markers: every restart interval is decoded by one task.
#[allow(dead_code)]
pub(crate) fn decode_frame_restarts(data: &[u8], hdr: &LjpegHeader, rows: usize, frame: &mut [u16]) -> Result<bool> {
    let rows = rows.min(hdr.height as usize);
    let row_len = hdr.row_len();
    let clrs = hdr.clrs as usize;
    if hdr.restart_interval == 0 || row_len == 0 || clrs == 0 || clrs > 6 {
        return Ok(false);
    }
    let total = row_len.checked_mul(rows).ok_or_else(size_error)?;
    if frame.len() < total || !(hdr.predictor == 1 || ljpeg::fast_rows(hdr)) {
        return Ok(false);
    }
    // Intervals start at row starts whose first MCU number is a multiple of the interval.
    let (width, interval) = (hdr.width as u64, hdr.restart_interval as u64);
    let starts: Vec<usize> = (0..rows).filter(|&r| r == 0 || (r as u64 * width) % interval == 0).collect();
    if starts.len() < 2 {
        return Ok(false);
    }
    let Some(scan) = data.get(hdr.scan_offset..) else { return Ok(false) };
    let unstuffed = ljpeg::unstuff_scan(scan, true);
    if unstuffed.segments.len() < starts.len() {
        return Ok(false);
    }
    let bounds: Vec<(usize, usize)> = starts
        .iter()
        .enumerate()
        .map(|(i, &r0)| (r0 * row_len, starts.get(i + 1).copied().unwrap_or(rows) * row_len))
        .collect();
    let Some(parts) = par::split_disjoint(&mut frame[..total], &bounds) else { return Ok(false) };
    let fast = ljpeg::fast_rows(hdr);
    let init = initial_prediction(hdr.bits);
    let jobs: Vec<(usize, &[u8], &mut [u16])> =
        parts.into_iter().enumerate().map(|(i, out)| (starts[i], unstuffed.segment(i), out)).collect();
    let all_ok = par::map(jobs, true, |(first_row, segment, out)| {
        let mut bits = BitPumpMsb::new(segment);
        let mut vpred = [init; 6];
        // Predictor 1 never reads the row above; give the row decoder something to point at.
        let zeros = if fast { Vec::new() } else { vec![0u16; row_len] };
        for (i, row) in out.chunks_exact_mut(row_len).enumerate() {
            if ljpeg::decode_row(hdr, fast, &mut bits, row, &zeros, first_row + i, &mut vpred).is_err() {
                return false;
            }
        }
        true
    });
    Ok(all_ok.into_iter().all(|ok| ok))
}

/// Half the range: the value that predicts the first sample of a scan or restart interval.
fn initial_prediction(bits: u32) -> u16 {
    1u32.checked_shl(bits.wrapping_sub(1)).unwrap_or(0) as u16
}

/// The unstuffed entropy-coded data of a restart-free scan as a stream of one difference per
/// sample, the Huffman table of an MCU position used by the samples at that position.
struct Scan<'a> {
    hdr: &'a LjpegHeader,
    data: &'a [u8],
    clrs: usize,
    period: usize,
    /// Per MCU position, the first position whose table has the same definition.
    slots: [usize; 6],
}

impl<'a> Scan<'a> {
    fn new(hdr: &'a LjpegHeader, data: &'a [u8]) -> Self {
        let clrs = hdr.clrs as usize;
        let id = |i: usize| hdr.table_ids.get(i).copied().unwrap_or(i as u8);
        let period = (1..=clrs).find(|&p| clrs % p == 0 && (0..clrs).all(|i| id(i) == id(i % p))).unwrap_or(clrs);
        let slots = std::array::from_fn(|c| (0..clrs.min(6)).find(|&i| id(i) == id(c)).unwrap_or(c));
        Scan { hdr, data, clrs, period, slots }
    }

    fn table(&self, phase: usize) -> &HuffTable {
        &self.hdr.tables[self.slots[phase % self.clrs]]
    }

    /// One symbol: the undecodable-code rule is that the walk still moves on by one bit.
    #[inline(always)]
    fn symbol(&self, p: &mut BitPumpMsb<'_>, phase: usize, out: &mut [u16]) -> bool {
        let before = p.bit_position();
        p.fill_to(27);
        let mut bad = false;
        out[0] = self.table(phase).decode_diff_buffered(p, &mut bad) as u16;
        if bad && p.bit_position() == before {
            // An undecodable code consumes nothing; step over one bit so the walk moves on.
            p.fill();
            p.skip_no_fill(1);
        }
        !bad
    }

    /// Batch decoder for `C` positions per MCU: whole rounds of `C` symbols while the worst case
    /// stays below `limit`. Stops (without consuming the batch it was in) at an invalid code.
    #[inline(always)]
    fn rounds<const C: usize>(
        &self,
        p: &mut BitPumpMsb<'_>,
        phase: usize,
        limit: u64,
        max: usize,
        out: &mut [u16],
    ) -> usize {
        let t: [&HuffTable; C] = std::array::from_fn(|i| self.table(phase + i));
        let mut done = 0;
        while max - done >= C {
            let room = limit.saturating_sub(p.bit_position());
            if room == 0 {
                break;
            }
            let rounds = ((room - 1) / (C as u64 * MAX_SYMBOL_BITS))
                .min(((max - done) / C) as u64)
                .min(MAX_ROUNDS as u64) as usize;
            if rounds == 0 {
                break;
            }
            let saved = *p;
            let mut bad = false;
            let mut k = 0usize;
            for mcu in out[done..done + rounds * C].chunks_exact_mut(C) {
                for (i, d) in mcu.iter_mut().enumerate() {
                    // Every second symbol tops the buffer up to the maximum; the others only
                    // refill when a long code used the reserve.
                    if k & 1 == 0 {
                        p.refill_force();
                    } else {
                        p.fill_to(27);
                    }
                    k += 1;
                    *d = t[i].decode_diff_buffered(p, &mut bad) as u16;
                }
            }
            if bad {
                *p = saved;
                break;
            }
            done += rounds * C;
        }
        done
    }
}

impl SymbolStream for Scan<'_> {
    type Pump<'p>
        = BitPumpMsb<'p>
    where
        Self: 'p;
    const WIDTH: usize = 1;

    fn cycle(&self) -> usize {
        self.clrs
    }

    fn period(&self) -> usize {
        self.period
    }

    fn total_bits(&self) -> u64 {
        self.data.len() as u64 * 8
    }

    fn end_check(&self, pos: u64) -> EndCheck {
        if pos <= self.total_bits() + END_SLACK_BITS { EndCheck::Valid } else { EndCheck::Invalid }
    }

    fn pump_at(&self, bit: u64) -> BitPumpMsb<'_> {
        BitPumpMsb::at_bit(self.data, bit)
    }

    fn position(p: &BitPumpMsb<'_>) -> u64 {
        p.bit_position()
    }

    #[inline(always)]
    fn decode(&self, p: &mut BitPumpMsb<'_>, phase: usize, out: &mut [u16]) -> bool {
        self.symbol(p, phase, out)
    }

    fn bulk(&self, p: &mut BitPumpMsb<'_>, phase: usize, limit: u64, max: usize, out: &mut [u16]) -> (usize, bool) {
        let phase = phase % self.clrs;
        let done = match self.clrs {
            1 => self.rounds::<1>(p, phase, limit, max, out),
            2 => self.rounds::<2>(p, phase, limit, max, out),
            3 => self.rounds::<3>(p, phase, limit, max, out),
            4 => self.rounds::<4>(p, phase, limit, max, out),
            5 => self.rounds::<5>(p, phase, limit, max, out),
            _ => self.rounds::<6>(p, phase, limit, max, out),
        };
        // The last symbols before the limit, and the batch an invalid code was found in, one by one.
        let mut ph = (phase + done) % self.clrs;
        let mut n = done;
        while n < max && p.bit_position() < limit {
            let ok = self.symbol(p, ph, &mut out[n..n + 1]);
            n += 1;
            if !ok {
                return (n, true);
            }
            ph = (ph + 1) % self.clrs;
        }
        (n, false)
    }
}

/// Turn the differences of `rows` rows into samples (T.81 H.1.2.1), `frame` holding exactly them.
fn rebuild(hdr: &LjpegHeader, diffs: &Values, rows: usize, frame: &mut [u16]) -> Result<bool> {
    let clrs = hdr.clrs as usize;
    let sraw = hdr.sraw as usize;
    let psv = hdr.predictor;
    if sraw > 0 && clrs <= sraw {
        return Ok(false);
    }
    if psv == 1 && (sraw > 0 || clrs > 4) {
        return Ok(rebuild_independent_rows(hdr, diffs, rows, frame));
    }
    if psv == 1 {
        match clrs {
            1 => sum_rows::<1>(hdr, diffs, rows, frame),
            2 => sum_rows::<2>(hdr, diffs, rows, frame),
            3 => sum_rows::<3>(hdr, diffs, rows, frame),
            _ => sum_rows::<4>(hdr, diffs, rows, frame),
        }
        return Ok(true);
    }
    if (2..=7).contains(&psv) && sraw == 0 && rows > 1 {
        rebuild_wavefront(hdr, diffs, rows, frame);
        return Ok(true);
    }
    Ok(rebuild_in_order(hdr, diffs, rows, frame))
}

/// The differences of row `r` as a slice (copied into `scratch` when they span two pieces).
fn row_diffs<'a>(diffs: &'a Values, r: usize, row_len: usize, scratch: &'a mut Vec<u16>) -> &'a [u16] {
    if let Some(s) = diffs.slice(r * row_len, row_len) {
        return s;
    }
    scratch.resize(row_len, 0);
    diffs.copy_to(r * row_len, scratch);
    scratch
}

/// Predictor 1, plain samples, up to four per MCU: the first MCU of each row chains to the first
/// MCU of the row above, every other sample adds its difference to its left neighbour.
fn sum_rows<const C: usize>(hdr: &LjpegHeader, diffs: &Values, rows: usize, frame: &mut [u16]) {
    let row_len = hdr.row_len();
    let mut first = Vec::with_capacity(rows);
    let mut column = [initial_prediction(hdr.bits); C];
    for r in 0..rows {
        for (c, v) in column.iter_mut().enumerate() {
            *v = v.wrapping_add(diffs.get(r * row_len + c));
        }
        first.push(column);
    }
    let rows_per_task = rows.div_ceil(par::threads().max(1) * 4).max(1);
    par::for_each_row_chunk(frame, row_len, rows_per_task, true, |row0, band| {
        let mut scratch = Vec::new();
        for (i, out) in band.chunks_exact_mut(row_len).enumerate() {
            let r = row0 + i;
            let src = row_diffs(diffs, r, row_len, &mut scratch);
            let mut run = first[r];
            out[..C].copy_from_slice(&run);
            for (o, d) in out.chunks_exact_mut(C).zip(src.chunks_exact(C)).skip(1) {
                for c in 0..C {
                    run[c] = run[c].wrapping_add(d[c]);
                    o[c] = run[c];
                }
            }
        }
    });
}

/// Predictor 1 with Canon sRAW sampling or five and six values per MCU: rows are independent once
/// the first-column state of each row is known; that state only needs the first MCU of each row.
fn rebuild_independent_rows(hdr: &LjpegHeader, diffs: &Values, rows: usize, frame: &mut [u16]) -> bool {
    let row_len = hdr.row_len();
    let (clrs, sraw) = (hdr.clrs as usize, hdr.sraw as usize);
    // Positions that continue the first-column chain: all of them, or with sRAW sampling the
    // first luma value and the chroma values.
    let chained = |c: usize| sraw == 0 || c == 0 || c > sraw;
    let mut state = Vec::with_capacity(rows);
    let mut column = [initial_prediction(hdr.bits); 6];
    for r in 0..rows {
        state.push(column);
        for c in (0..clrs).filter(|&c| chained(c)) {
            column[c] = column[c].wrapping_add(diffs.get(r * row_len + c));
        }
    }
    let failed = AtomicBool::new(false);
    let rows_per_task = rows.div_ceil(par::threads().max(1) * 4).max(1);
    par::for_each_row_chunk(frame, row_len, rows_per_task, true, |row0, band| {
        let mut scratch = Vec::new();
        for (i, out) in band.chunks_exact_mut(row_len).enumerate() {
            let r = row0 + i;
            let src = row_diffs(diffs, r, row_len, &mut scratch);
            let mut next = src.iter();
            let mut vpred = state[r];
            let supply = |_: usize| Ok(next.next().copied().unwrap_or(0) as i16 as i32);
            if ljpeg::reconstruct_row(hdr, supply, out, &[], r, clrs, &mut vpred).is_err() {
                failed.store(true, Ordering::Relaxed);
            }
        }
    });
    !failed.load(Ordering::Relaxed)
}

/// Predictors 2..=7 on plain frames: bands of rows advance as a wavefront over blocks of columns,
/// each sample depending on its left, upper and upper-left neighbours.
fn rebuild_wavefront(hdr: &LjpegHeader, diffs: &Values, rows: usize, frame: &mut [u16]) {
    let row_len = hdr.row_len();
    let clrs = hdr.clrs as usize;
    let first = initial_prediction(hdr.bits);
    let block_mcus = (hdr.width as usize).div_ceil(par::threads().max(1) * 4).max(64);
    let geo = par::Wavefront { row_len, halo: 1, margin: clrs, block: block_mcus * clrs };
    let data = &mut frame[..rows * row_len];
    macro_rules! run {
        ($psv:literal) => {
            par::wavefront(data, &[], geo, true, |row0, band, above, lo, a, b| {
                wave_block::<$psv>(diffs, clrs, row_len, first, row0, band, above, (lo, a, b))
            })
        };
    }
    match hdr.predictor {
        2 => run!(2),
        3 => run!(3),
        4 => run!(4),
        5 => run!(5),
        6 => run!(6),
        _ => run!(7),
    }
}

/// Prediction from the left (`ra`), upper (`rb`) and upper-left (`rc`) samples (T.81 Table H.1).
#[inline(always)]
fn predict<const PSV: u32>(ra: i32, rb: i32, rc: i32) -> i32 {
    match PSV {
        2 => rb,
        3 => rc,
        4 => ra + rb - rc,
        5 => ra + ((rb - rc) >> 1),
        6 => rb + ((ra - rc) >> 1),
        _ => (ra + rb) >> 1,
    }
}

/// Columns `a..b` of every row of `band` (rows of `row_len` samples, the first one being row
/// `row0` of the frame); `above` holds the row above the band in columns `lo..b`.
#[allow(clippy::too_many_arguments)]
fn wave_block<const PSV: u32>(
    diffs: &Values,
    clrs: usize,
    row_len: usize,
    first: u16,
    row0: usize,
    band: &mut [u16],
    above: &[u16],
    (lo, a, b): (usize, usize, usize),
) {
    let mut scratch = Vec::new();
    for i in 0..band.len() / row_len {
        let y = row0 + i;
        let (done, rest) = band.split_at_mut(i * row_len);
        let cur = &mut rest[..row_len];
        let start = y * row_len + a;
        let d: &[u16] = match diffs.slice(start, b - a) {
            Some(s) => s,
            None => {
                scratch.resize(b - a, 0);
                diffs.copy_to(start, &mut scratch);
                &scratch
            }
        };
        let d = |idx: usize| d[idx - a];
        if y == 0 {
            for idx in a..b {
                let ra = if idx < clrs { first } else { cur[idx - clrs] };
                cur[idx] = ra.wrapping_add(d(idx));
            }
            continue;
        }
        // The row above, indexed by column (`above` starts at column `lo`).
        let (prev, off): (&[u16], usize) = if i > 0 { (&done[(i - 1) * row_len..], 0) } else { (above, lo) };
        let mut idx = a;
        if a == 0 {
            for c in 0..clrs {
                cur[c] = prev[c - off].wrapping_add(d(c));
            }
            idx = clrs;
        }
        while idx < b {
            let (ra, rb, rc) = (cur[idx - clrs] as i32, prev[idx - off] as i32, prev[idx - clrs - off] as i32);
            cur[idx] = (predict::<PSV>(ra, rb, rc) as u16).wrapping_add(d(idx));
            idx += 1;
        }
    }
}

/// Every other layout: rows one after the other on this thread (the entropy decoding already ran
/// on all threads), each from the row above it.
fn rebuild_in_order(hdr: &LjpegHeader, diffs: &Values, rows: usize, frame: &mut [u16]) -> bool {
    let row_len = hdr.row_len();
    let clrs = hdr.clrs as usize;
    let mut vpred = [initial_prediction(hdr.bits); 6];
    let mut scratch = Vec::new();
    for r in 0..rows {
        let (done, rest) = frame.split_at_mut(r * row_len);
        let prev: &[u16] = if r > 0 { &done[(r - 1) * row_len..] } else { &[] };
        let src = row_diffs(diffs, r, row_len, &mut scratch);
        let mut next = src.iter();
        let supply = |_: usize| Ok(next.next().copied().unwrap_or(0) as i16 as i32);
        if ljpeg::reconstruct_row(hdr, supply, &mut rest[..row_len], prev, r, clrs, &mut vpred).is_err() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests;
