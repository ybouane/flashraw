//! Parallel decoding of a single lossless-JPEG scan.
//!
//! A predictor-1 scan without restart markers (every CR2) is one long Huffman stream,
//! normally decoded by one thread. Huffman codes self-synchronise: a decoder started
//! at an arbitrary bit soon lands on a true codeword boundary and from then on decodes
//! exactly what a decoder started at the beginning would. That makes three passes
//! possible:
//!
//! 1. **Speculate** (parallel): the unstuffed stream is cut into chunks; each chunk is
//!    decoded from its first bit into a private buffer of differences, recording the
//!    bit position of its first symbol boundaries.
//! 2. **Resolve** (sequential, cheap): walking from the true end of chunk `k - 1`, the
//!    true path is decoded symbol by symbol until it reaches a boundary that chunk `k`
//!    recorded with the same table phase; everything chunk `k` decoded from there on is
//!    correct. This gives every symbol its absolute index.
//! 3. **Reconstruct** (parallel): rows are filled from the differences by a prefix
//!    sum; only the first sample of each row depends on the row above, and those are
//!    chained sequentially first.
//!
//! Differences depend only on the bit stream and the table sequence, never on decoded
//! values, which is why speculation is possible at all. Whenever the outcome could
//! differ from the sequential decoder (no synchronisation found, an invalid code on
//! the true path, a truncated stream) the caller is told to decode sequentially, so
//! results and errors are always identical.

use crate::ljpeg::{LjpegHeader, unstuff};
use flashraw_core::bits::{BitPumpMsb, BitRead};
use flashraw_core::huffman::HuffTable;
use flashraw_core::{Error, Result, par};

/// Smallest chunk worth a task.
const MIN_CHUNK_BYTES: usize = 128 << 10;
/// Boundaries recorded per chunk for synchronisation (true paths sync within a few
/// dozen symbols; a wrong table phase needs a few times more).
const SYNC_STATES: usize = 1 << 14;
/// Sequential decoding tolerates this many bytes consumed past the end of the scan.
const END_SLACK_BITS: u64 = 16;

/// Decode the whole frame of a predictor-1, restart-free scan into `frame`
/// (`height` rows of [`LjpegHeader::row_len`] samples) using all worker threads.
///
/// Returns `Ok(false)` without a usable result when the stream does not qualify or
/// cannot be decoded speculatively; the caller then uses [`crate::ljpeg::decode_rows`]
/// (which also produces the exact error for corrupt data).
pub fn decode_frame_parallel(data: &[u8], hdr: &LjpegHeader, frame: &mut [u16]) -> Result<bool> {
    decode_frame_chunked(data, hdr, frame, None)
}

pub(crate) fn decode_frame_chunked(
    data: &[u8],
    hdr: &LjpegHeader,
    frame: &mut [u16],
    chunk_bytes: Option<usize>,
) -> Result<bool> {
    let clrs = hdr.clrs as usize;
    let rl = hdr.row_len();
    let height = hdr.height as usize;
    let total = rl.checked_mul(height).ok_or_else(|| Error::corrupt("lossless JPEG: frame too large"))?;
    if hdr.predictor != 1 || hdr.sraw != 0 || hdr.restart_interval != 0 || clrs == 0 || clrs > 4 || frame.len() < total
    {
        return Ok(false);
    }
    let Some(scan) = data.get(hdr.scan_offset..) else { return Ok(false) };
    let un = unstuff(scan);
    if un.segments.len() != 1 {
        return Ok(false);
    }
    let stream = un.segment(0);
    let chunk = match chunk_bytes {
        Some(c) => c.max(1),
        None => {
            let tasks = par::threads() * 4;
            if tasks <= 4 || stream.len() < 2 * MIN_CHUNK_BYTES {
                return Ok(false);
            }
            stream.len().div_ceil(tasks).max(MIN_CHUNK_BYTES)
        }
    };
    let period = phase_period(&hdr.table_ids);
    let nchunks = stream.len().div_ceil(chunk).max(1);
    let total_bits = stream.len() as u64 * 8;

    // Pass 1: speculative decoding of every chunk.
    // A start with the wrong table phase rarely locks onto the true path when the
    // components use different tables, so every phase of the period is tried.
    let jobs: Vec<(usize, usize)> = (0..nchunks).flat_map(|k| (0..period).map(move |g| (k, g))).collect();
    let flat: Vec<Spec> = par::map(jobs, true, |(k, g)| {
        let start = (k * chunk) as u64 * 8;
        let limit = if k + 1 == nchunks { None } else { Some(((k + 1) * chunk) as u64 * 8) };
        speculate(stream, &hdr.tables, g, start, limit, total_bits, total)
    });
    let specs: Vec<&[Spec]> = flat.chunks(period).collect();

    // Pass 2: resolve the true start of every chunk.
    let res = resolve(stream, &hdr.tables, &specs, period, total, total_bits);
    let Some(segments) = res else { return Ok(false) };

    // Pass 3: chain the first sample of every row, then fill rows in parallel.
    let diffs = Segmented { segments: &segments };
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

/// Result of speculatively decoding one chunk.
struct Spec {
    /// Table phase of the first symbol.
    phase: usize,
    /// Bit position before symbol `j`, for the first [`SYNC_STATES`] symbols.
    states: Vec<u64>,
    /// Difference of every decoded symbol (as wrapping `u16`).
    diffs: Vec<u16>,
    /// Symbols at which an invalid code was met (decoding skipped one bit there).
    errors: Vec<usize>,
    /// Position after the last decoded symbol: the first boundary at or past the chunk
    /// end (or, for the last chunk, past the end of the data).
    end: u64,
    /// Last chunk: symbols that end within the tolerated slack past the data.
    valid: usize,
}

fn speculate(
    stream: &[u8],
    tables: &[HuffTable],
    phase: usize,
    start: u64,
    limit: Option<u64>,
    total_bits: u64,
    cap: usize,
) -> Spec {
    let clrs = tables.len();
    let span = limit.unwrap_or(total_bits).saturating_sub(start);
    let mut diffs: Vec<u16> = Vec::with_capacity(((span / 6) as usize).min(cap));
    let mut states = Vec::with_capacity(SYNC_STATES.min(span as usize));
    let mut errors = Vec::new();
    let stop = limit.unwrap_or(total_bits + END_SLACK_BITS + 1);
    let mut bits = BitPumpMsb::at_bit(stream, start);
    let mut pos = start;
    let mut c = phase % clrs;
    let mut err = false;
    // Never decode more symbols than the frame holds (bounds work on garbage input).
    while pos < stop && diffs.len() <= cap {
        if states.len() < SYNC_STATES {
            states.push(pos);
        }
        bits.fill();
        let d = tables[c].decode_diff_buffered(&mut bits, &mut err);
        let np = bits.bit_position();
        if err {
            err = false;
            errors.push(diffs.len());
            if np == pos {
                bits.skip_no_fill(1);
            }
        }
        pos = bits.bit_position();
        diffs.push(d as u16);
        c += 1;
        if c == clrs {
            c = 0;
        }
    }
    // For the last chunk the loop ran one symbol past the slack (or hit the cap).
    let valid = if limit.is_none() && pos > total_bits + END_SLACK_BITS { diffs.len() - 1 } else { diffs.len() };
    Spec { phase, states, diffs, errors, end: pos, valid }
}

/// Absolute index of a run's first symbol, symbols decoded on the true path before
/// it reached a chunk's synchronisation point, and the verified speculative output.
type Resolved<'a> = Vec<(usize, Vec<u16>, &'a [u16])>;

fn resolve<'a>(
    stream: &[u8],
    tables: &[HuffTable],
    specs: &[&'a [Spec]],
    period: usize,
    total: usize,
    total_bits: u64,
) -> Option<Resolved<'a>> {
    let clrs = tables.len();
    let mut out = Vec::with_capacity(specs.len());
    let mut n = 0usize;
    let mut pos = 0u64;
    let mut bridge: Vec<u16> = Vec::new();
    let mut bits: Option<BitPumpMsb> = None;
    let mut k = 0usize;
    while n < total {
        let Some(&phases) = specs.get(k) else { break };
        let last = k + 1 == specs.len();
        // Synchronised: the true path is at a boundary one of chunk `k`'s speculations
        // recorded, with the same table phase.
        let hit = phases.iter().find_map(|s| {
            let j = s.states.binary_search(&pos).ok()?;
            ((s.phase + j) % period == n % period).then_some((s, j))
        });
        if let Some((s, j)) = hit {
            let avail = if last { s.valid } else { s.diffs.len() };
            let count = avail.checked_sub(j)?.min(total - n);
            if last && count < total - n {
                return None; // truncated
            }
            if s.errors.iter().any(|&e| e >= j && e < j + count) {
                return None; // the true path holds an invalid code
            }
            out.push((n - bridge.len(), std::mem::take(&mut bridge), &s.diffs[j..j + count]));
            n += count;
            if n == total {
                // Verified above: `valid` covers the slack for the last chunk, and an
                // earlier chunk ends well inside the data.
                return Some(out);
            }
            pos = s.end;
            bits = None;
            k += 1;
            continue;
        }
        if !last && phases.iter().all(|s| pos >= s.end) {
            // The true path walked past this whole (short) chunk; try the next one.
            k += 1;
            continue;
        }
        if phases.iter().all(|s| s.states.len() == SYNC_STATES && s.states.last().is_some_and(|&l| pos > l)) {
            return None; // no synchronisation within the recorded boundaries
        }
        // Advance the true path by one symbol.
        let b = bits.get_or_insert_with(|| BitPumpMsb::at_bit(stream, pos));
        b.fill();
        let mut err = false;
        let d = tables[n % clrs].decode_diff_buffered(b, &mut err);
        if err {
            return None;
        }
        bridge.push(d as u16);
        pos = b.bit_position();
        n += 1;
    }
    if n < total || pos > total_bits + END_SLACK_BITS {
        return None;
    }
    if !bridge.is_empty() {
        out.push((n - bridge.len(), bridge, &[]));
    }
    Some(out)
}

/// Differences addressed by absolute sample index.
struct Segmented<'s, 'a> {
    segments: &'s [(usize, Vec<u16>, &'a [u16])],
}

impl Segmented<'_, '_> {
    /// Segment index holding sample `i` and the offset within it.
    fn locate(&self, i: usize) -> (usize, usize) {
        let k = self.segments.partition_point(|s| s.0 <= i).saturating_sub(1);
        (k, i - self.segments[k].0)
    }

    fn get(&self, i: usize) -> u16 {
        let (k, off) = self.locate(i);
        let (_, bridge, body) = &self.segments[k];
        if off < bridge.len() { bridge[off] } else { body[off - bridge.len()] }
    }

    fn copy_to(&self, start: usize, dst: &mut [u16]) {
        let (mut k, mut off) = self.locate(start);
        let mut done = 0;
        while done < dst.len() {
            let (_, bridge, body) = &self.segments[k];
            for part in [&bridge[..], body] {
                if off >= part.len() {
                    off -= part.len();
                    continue;
                }
                let n = (part.len() - off).min(dst.len() - done);
                dst[done..done + n].copy_from_slice(&part[off..off + n]);
                done += n;
                off = 0;
                if done == dst.len() {
                    return;
                }
            }
            k += 1;
            off = 0;
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
        for (comps, bits) in [(1usize, 12u32), (2, 14), (3, 12), (4, 14)] {
            let (w, h) = (97usize, 61usize);
            let samples = pseudo(w * h * comps, bits, comps as u32 * 7 + 1);
            let enc = encode(w, h, comps, bits, 1, &samples);
            let hdr = LjpegHeader::parse(&enc, LjpegOptions::default()).unwrap();
            let want = sequential(&enc, &hdr);
            assert_eq!(want, samples);
            for chunk in [7usize, 64, 333, 1000, 5000, 1 << 20] {
                let mut frame = vec![0u16; want.len()];
                let ok = decode_frame_chunked(&enc, &hdr, &mut frame, Some(chunk)).unwrap();
                assert!(ok, "comps {comps} chunk {chunk}: no synchronisation");
                assert_eq!(frame, want, "comps {comps} chunk {chunk}");
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
        assert!(!decode_frame_chunked(cut, &hdr, &mut frame, Some(100)).unwrap());
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
