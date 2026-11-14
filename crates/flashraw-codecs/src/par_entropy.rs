//! Parallel decoding of one sequential prefix-coded stream.
//!
//! A variable-length code cannot be decoded from the middle of a stream without knowing where a
//! codeword starts, but a decoder started at an arbitrary bit position falls onto the true
//! codeword boundaries after a few symbols (it "synchronises") and then follows the true path
//! exactly. The engine uses this in three steps:
//!
//! 1. The stream is cut into chunks at byte positions. Every chunk is decoded by its own task,
//!    starting at the chunk's first bit, as if that were a symbol boundary (a *walk*). Besides the
//!    values, a walk records the bit positions of its symbol boundaries: all of them for the
//!    first [`FINE`] symbols, then one every [`COARSE`] symbols.
//! 2. When the table sequence repeats with a period `P > 1`, the phase of the first symbol of a
//!    chunk is unknown, so `P` walks with different phases are possible. Only the walk of phase
//!    zero runs to the end of the chunk. The others stop as soon as they land on a boundary of
//!    the phase-zero walk with a compatible phase, because from there on both are the same walk.
//! 3. A sequential stitching pass follows the true path: it starts at bit 0, and whenever the
//!    true position is a recorded boundary of a walk of the right phase it adopts that walk's
//!    values up to the end of the chunk; until then it decodes symbols itself. The result is
//!    accepted only if it is provably what a sequential decoder produces, so it never differs
//!    from it: invalid codes, a bad end position, or running out of data give `None`.
//!
//! The decoded values stay in the buffers of the walks; [`Values`] indexes them without copying.

use flashraw_core::par;

/// How the end of a stream is judged, from the bit position after its last symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EndCheck {
    /// The sequential decoder accepts it.
    Valid,
    /// It rejects it.
    Invalid,
    /// Depends on state the engine does not model: decode sequentially.
    Unknown,
}

/// An entropy-coded stream that can be decoded from any bit position.
///
/// Decoding one symbol must be a pure function of the stream bytes, the bit position and the
/// table phase (`phase % period()`); symbols come in a cycle of `cycle()` table phases.
pub(crate) trait SymbolStream: Sync {
    /// Bit-reader state of one walk.
    type Pump<'p>
    where
        Self: 'p;
    /// Values each symbol yields (1 to 4).
    const WIDTH: usize;
    /// Symbol `j` uses table phase `j % cycle()`.
    fn cycle(&self) -> usize;
    /// Smallest period of the table sequence (divides `cycle()`).
    fn period(&self) -> usize;
    /// Length of the stream in bits.
    fn total_bits(&self) -> u64;
    /// Bits worth speculating over; defaults to the whole stream.
    fn span_bits(&self) -> u64 {
        self.total_bits()
    }
    /// Judge an end position (the position after the last needed symbol).
    fn end_check(&self, pos: u64) -> EndCheck;
    /// A pump positioned at `bit`.
    fn pump_at(&self, bit: u64) -> Self::Pump<'_>;
    /// Bit position of a pump.
    fn position(p: &Self::Pump<'_>) -> u64;
    /// Decode one symbol in table phase `phase` into `out` (`WIDTH` wrapping `u16` values).
    /// Returns false for an invalid code; the pump must still have advanced.
    fn decode(&self, p: &mut Self::Pump<'_>, phase: usize, out: &mut [u16]) -> bool;
    /// Decode symbols while the position is below `limit`, at most `max`, stopping after the
    /// first invalid one. `phase` is the table phase of the first symbol and `out` receives
    /// `WIDTH` values per symbol. Returns the number of symbols decoded (an invalid one included)
    /// and whether the last of them was invalid. Implementations may batch; the result must be
    /// the same as repeated [`SymbolStream::decode`] calls.
    fn bulk(&self, p: &mut Self::Pump<'_>, phase: usize, limit: u64, max: usize, out: &mut [u16]) -> (usize, bool) {
        step_bulk(self, p, phase, limit, max, out)
    }
}

/// [`SymbolStream::bulk`] one symbol at a time.
pub(crate) fn step_bulk<S: SymbolStream + ?Sized>(
    s: &S,
    p: &mut S::Pump<'_>,
    phase: usize,
    limit: u64,
    max: usize,
    out: &mut [u16],
) -> (usize, bool) {
    let cycle = s.cycle().max(1);
    let mut ph = phase % cycle;
    let mut done = 0;
    while done < max && S::position(p) < limit {
        let ok = s.decode(p, ph, &mut out[done * S::WIDTH..(done + 1) * S::WIDTH]);
        done += 1;
        if !ok {
            return (done, true);
        }
        ph += 1;
        if ph == cycle {
            ph = 0;
        }
    }
    (done, false)
}

/// Symbols of a walk whose boundaries are all recorded (and within which phase classes merge).
const FINE: usize = 2048;
/// After the first [`FINE`] symbols one boundary in this many is recorded.
const COARSE: usize = 128;
/// Streams below this size are never split.
const MIN_STREAM_BYTES: usize = 256 * 1024;
/// Smallest chunk the default rule produces.
const MIN_CHUNK_BYTES: usize = 128 * 1024;
/// Chunks per pool thread in the default rule.
const TASKS_PER_THREAD: usize = 4;
/// More chunks than this are not attempted.
const MAX_CHUNKS: u64 = 1 << 26;

/// Chunk size in bytes for a stream of `bytes`, or `None` when it is not worth splitting.
pub(crate) fn default_chunk(bytes: usize) -> Option<usize> {
    let threads = par::threads();
    if threads <= 1 || bytes < MIN_STREAM_BYTES {
        return None;
    }
    let tasks = threads.saturating_mul(TASKS_PER_THREAD);
    Some(bytes.div_ceil(tasks).max(MIN_CHUNK_BYTES))
}

/// One piece of the result: `len` values of buffer `buf` from `off`, at value index `start`.
#[derive(Clone, Copy)]
struct Run {
    start: usize,
    buf: usize,
    off: usize,
    len: usize,
}

/// Decoded values addressed by absolute value index (`symbol * WIDTH + k`).
pub(crate) struct Values {
    bufs: Vec<Vec<u16>>,
    runs: Vec<Run>,
}

impl Values {
    fn empty() -> Values {
        Values { bufs: Vec::new(), runs: Vec::new() }
    }

    /// Index of the run holding value `i`.
    fn run_of(&self, i: usize) -> Option<usize> {
        let r = self.runs.partition_point(|r| r.start <= i).checked_sub(1)?;
        (i < self.runs[r].start + self.runs[r].len).then_some(r)
    }

    /// Value `i` (0 outside the decoded range).
    pub fn get(&self, i: usize) -> u16 {
        match self.run_of(i) {
            Some(r) => {
                let run = &self.runs[r];
                self.bufs[run.buf][run.off + (i - run.start)]
            }
            None => 0,
        }
    }

    /// Copy values `start .. start + dst.len()` into `dst`.
    pub fn copy_to(&self, start: usize, dst: &mut [u16]) {
        let mut at = 0;
        while at < dst.len() {
            let Some(r) = self.run_of(start + at) else {
                dst[at..].fill(0);
                return;
            };
            let run = &self.runs[r];
            let skip = start + at - run.start;
            let from = run.off + skip;
            let n = (run.len - skip).min(dst.len() - at);
            dst[at..at + n].copy_from_slice(&self.bufs[run.buf][from..from + n]);
            at += n;
        }
    }

    /// Values `start .. start + len` as one slice, when they lie within one piece of storage.
    pub(crate) fn slice(&self, start: usize, len: usize) -> Option<&[u16]> {
        let run = &self.runs[self.run_of(start)?];
        let skip = start - run.start;
        (skip + len <= run.len).then(|| &self.bufs[run.buf][run.off + skip..run.off + skip + len])
    }
}

/// A speculative decode of one chunk from a guessed symbol boundary.
struct Walk {
    /// Table phase of the first symbol.
    class: usize,
    vals: Vec<u16>,
    count: usize,
    /// Position before symbol `i`, for `i < FINE`.
    fine: Vec<u64>,
    /// Position before symbol `FINE + COARSE * t`.
    coarse: Vec<u64>,
    /// Indices of the symbols with an invalid code.
    bad: Vec<usize>,
    /// Position after the last symbol.
    end: u64,
}

impl Walk {
    /// Index of the symbol that starts at `pos`, if it is a recorded boundary.
    fn find(&self, pos: u64) -> Option<usize> {
        if let Ok(i) = self.fine.binary_search(&pos) {
            return Some(i);
        }
        self.coarse.binary_search(&pos).ok().map(|t| FINE + t * COARSE)
    }

    /// Position before symbol `m` (`m <= count`), re-decoding from the nearest recorded boundary.
    fn position_before<S: SymbolStream>(&self, s: &S, m: usize) -> u64 {
        if m >= self.count {
            return self.end;
        }
        if m < FINE {
            return self.fine[m];
        }
        let t = (m - FINE) / COARSE;
        let first = FINE + t * COARSE;
        let mut pump = s.pump_at(self.coarse[t]);
        let cycle = s.cycle().max(1);
        let mut scratch = [0u16; 8];
        for i in first..m {
            s.decode(&mut pump, (self.class + i) % cycle, &mut scratch[..S::WIDTH]);
        }
        S::position(&pump)
    }
}

/// Decode a chunk from `start` in phase 0 until the position reaches `limit`.
fn walk_primary<S: SymbolStream>(s: &S, start: u64, limit: u64, cap: usize, reserve: usize) -> Walk {
    let width = S::WIDTH;
    let cycle = s.cycle().max(1);
    let mut pump = s.pump_at(start);
    let mut vals: Vec<u16> = Vec::with_capacity(reserve.saturating_mul(width));
    let mut fine = Vec::new();
    let mut coarse = Vec::new();
    let mut bad = Vec::new();
    let mut count = 0usize;
    let end;
    loop {
        let pos = S::position(&pump);
        if pos >= limit || count >= cap {
            end = pos;
            break;
        }
        let at = vals.len();
        if count < FINE {
            fine.push(pos);
            vals.resize(at + width, 0);
            if !s.decode(&mut pump, count % cycle, &mut vals[at..]) {
                bad.push(count);
            }
            count += 1;
            continue;
        }
        let into = (count - FINE) % COARSE;
        if into == 0 {
            coarse.push(pos);
        }
        let n = (COARSE - into).min(cap - count);
        vals.resize(at + n * width, 0);
        let (done, was_bad) = s.bulk(&mut pump, count % cycle, limit, n, &mut vals[at..]);
        vals.truncate(at + done * width);
        count += done;
        if was_bad {
            bad.push(count - 1);
        }
    }
    Walk { class: 0, vals, count, fine, coarse, bad, end }
}

/// Decode from `start` with the first symbol in phase `class`, stopping at the first position
/// where this walk coincides with the phase-0 walk `primary` (same boundary, same phase).
fn walk_alt<S: SymbolStream>(s: &S, start: u64, limit: u64, cap: usize, class: usize, primary: &Walk) -> Walk {
    let width = S::WIDTH;
    let cycle = s.cycle().max(1);
    let period = s.period().max(1);
    let mut pump = s.pump_at(start);
    let mut vals: Vec<u16> = Vec::new();
    let mut fine = Vec::new();
    let mut bad = Vec::new();
    let mut count = 0usize;
    let mut end = S::position(&pump);
    while end < limit && count < cap && count < FINE {
        fine.push(end);
        let at = vals.len();
        vals.resize(at + width, 0);
        if !s.decode(&mut pump, (class + count) % cycle, &mut vals[at..]) {
            bad.push(count);
        }
        count += 1;
        end = S::position(&pump);
        if let Ok(idx) = primary.fine.binary_search(&end) {
            if idx % period == (class + count) % period {
                break;
            }
        }
    }
    Walk { class, vals, count, fine, coarse: Vec::new(), bad, end }
}

/// Smallest position above `n` the end check does not call valid (the end check is monotone),
/// `u64::MAX` when there is none.
fn first_rejected<S: SymbolStream>(s: &S, n: u64) -> u64 {
    let valid = |q: u64| s.end_check(q) == EndCheck::Valid;
    let first = n.saturating_add(1);
    if !valid(first) {
        return first;
    }
    let mut good = first;
    let mut step = 1u64;
    let bad = loop {
        if good == u64::MAX {
            return u64::MAX;
        }
        let q = good.saturating_add(step);
        if !valid(q) {
            break q;
        }
        good = q;
        step = step.saturating_mul(2);
    };
    let (mut lo, mut hi) = (good, bad);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if valid(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// Decode the first `symbols` symbols of `s` on all threads, `chunk` bytes per task.
/// `None` means the caller must decode sequentially.
pub(crate) fn decode<S: SymbolStream>(s: &S, symbols: usize, chunk: usize) -> Option<Values> {
    let width = S::WIDTH;
    symbols.checked_mul(width)?;
    if symbols == 0 {
        return (s.end_check(0) == EndCheck::Valid).then(Values::empty);
    }
    let cycle = s.cycle().max(1);
    let mut period = s.period().max(1);
    if cycle % period != 0 {
        period = cycle;
    }
    let total = s.total_bits();
    let span = s.span_bits().min(total);
    let step = (chunk.max(1) as u64).saturating_mul(8);
    let chunks = span.div_ceil(step).max(1);
    if chunks > MAX_CHUNKS {
        return None;
    }
    let chunks = chunks as usize;
    // Where the walks of each chunk stop: the first symbol boundary at or past this position.
    let last_limit = if span < total { span } else { first_rejected(s, total) };
    let limit_of = move |k: usize| if k + 1 < chunks { (k as u64 + 1) * step } else { last_limit };
    let cap = symbols.saturating_add(1);

    let walks: Vec<Vec<Walk>> = par::map((0..chunks).collect(), true, |k| {
        let start = k as u64 * step;
        let limit = limit_of(k);
        let covered = limit.min(span.max(start)).saturating_sub(start);
        let share = (covered as u128 * symbols as u128 / span.max(1) as u128) as usize;
        let reserve = (share + share / 8 + 64).min(cap).min((covered as usize).saturating_add(64));
        let primary = walk_primary(s, start, limit, cap, reserve);
        let mut set = Vec::with_capacity(period);
        if k > 0 {
            for class in 1..period {
                set.push(walk_alt(s, start, limit, cap, class, &primary));
            }
        }
        set.insert(0, primary);
        set
    });

    stitch(s, symbols, step, cycle, period, &limit_of, walks)
}

/// Follow the true path through the walks and collect the values.
fn stitch<S: SymbolStream>(
    s: &S,
    symbols: usize,
    step: u64,
    cycle: usize,
    period: usize,
    limit_of: &dyn Fn(usize) -> u64,
    walks: Vec<Vec<Walk>>,
) -> Option<Values> {
    let width = S::WIDTH;
    let chunks = walks.len();
    // Buffer numbering: the walks in order, then the buffer of symbols decoded here.
    let mut base = Vec::with_capacity(chunks);
    let mut buffers = 0usize;
    for set in &walks {
        base.push(buffers);
        buffers += set.len();
    }
    let own = buffers;

    let mut runs: Vec<Run> = Vec::new();
    let mut solo: Vec<u16> = Vec::new();
    let mut pump: Option<S::Pump<'_>> = None;
    let (mut pos, mut n) = (0u64, 0usize);
    while n < symbols {
        let k = (pos / step).min(chunks as u64 - 1) as usize;
        if pos >= limit_of(k) {
            return None;
        }
        let set = &walks[k];
        let hit = set.iter().enumerate().find_map(|(g, w)| {
            let j = w.find(pos)?;
            ((g + j) % period == n % period).then_some((g, j))
        });
        if let Some((g, j)) = hit {
            let w = &set[g];
            let take = (w.count - j).min(symbols - n);
            let first_bad = w.bad.partition_point(|&b| b < j);
            if w.bad.get(first_bad).is_some_and(|&b| b < j + take) {
                return None;
            }
            runs.push(Run { start: n * width, buf: base[k] + g, off: j * width, len: take * width });
            n += take;
            pos = if n < symbols { w.end } else { w.position_before(s, j + take) };
            pump = None;
            continue;
        }
        // Not on a recorded boundary: decode one symbol of the true path here.
        let pump_ref = pump.get_or_insert_with(|| s.pump_at(pos));
        let at = solo.len();
        solo.resize(at + width, 0);
        if !s.decode(pump_ref, n % cycle, &mut solo[at..]) {
            return None;
        }
        match runs.last_mut() {
            Some(r) if r.buf == own && r.start + r.len == n * width => r.len += width,
            _ => runs.push(Run { start: n * width, buf: own, off: at, len: width }),
        }
        n += 1;
        pos = S::position(pump_ref);
    }
    if s.end_check(pos) != EndCheck::Valid {
        return None;
    }
    let mut bufs: Vec<Vec<u16>> = walks.into_iter().flatten().map(|w| w.vals).collect();
    bufs.push(solo);
    Some(Values { bufs, runs })
}

#[cfg(test)]
impl Values {
    pub(crate) fn from_vec(v: Vec<u16>) -> Values {
        Values::from_pieces(vec![v])
    }

    pub(crate) fn from_pieces(pieces: Vec<Vec<u16>>) -> Values {
        let mut runs = Vec::new();
        let mut start = 0;
        for (buf, p) in pieces.iter().enumerate() {
            runs.push(Run { start, buf, off: 0, len: p.len() });
            start += p.len();
        }
        Values { bufs: pieces, runs }
    }
}

#[cfg(test)]
mod tests;
