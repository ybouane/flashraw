//! Parallel decoding of one sequential entropy-coded stream by speculation.
//!
//! Many raw codecs store the whole image as one prefix-coded stream (lossless JPEG
//! without restart markers, Hasselblad's pair coding, ...), normally decoded by one
//! thread. Prefix codes self-synchronise: a decoder started at an arbitrary bit soon
//! lands on a true symbol boundary and from then on decodes exactly what a decoder
//! started at the beginning would. Symbol decoding must not depend on decoded *values*
//! (only on the bit stream and on which table a position uses), which holds whenever
//! prediction is applied after entropy decoding. Then:
//!
//! 1. **Speculate** (parallel): the stream is cut into chunks; each chunk is decoded from
//!    its first bit into a private buffer, recording the bit position of its first
//!    symbol boundaries. Streams that cycle through tables are speculated once per table
//!    phase; a phase other than 0 runs only until it joins phase 0's path.
//! 2. **Resolve** (sequential, cheap): from the true end of chunk `k - 1`, the true path
//!    is decoded symbol by symbol until it meets a boundary chunk `k` recorded in the same
//!    table phase; everything chunk `k` decoded from there on is correct. This gives every
//!    symbol its absolute index.
//! 3. The caller rebuilds samples from the values ([`Values`]).
//!
//! Whenever the outcome could differ from the sequential decoder (no synchronisation, an
//! invalid code on the true path, an end the rules cannot classify) `decode` returns
//! `None` and the caller decodes sequentially, so results and errors stay identical.

use flashraw_core::par;

/// Boundaries recorded per chunk for synchronisation (true paths sync within a few
/// dozen symbols; other table phases need a few times more).
const SYNC_STATES: usize = 1 << 14;
/// Smallest chunk worth a task.
const MIN_CHUNK_BYTES: usize = 128 << 10;

/// How a stream's end is judged, from the bit position after its last symbol.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EndCheck {
    /// The sequential decoder accepts it.
    Valid,
    /// It rejects it (truncated stream).
    Invalid,
    /// Depends on decoder state the speculation does not model: decode sequentially.
    Unknown,
}

/// An entropy-coded stream that can be decoded from any bit position.
pub(crate) trait SymbolStream: Sync {
    type Pump<'p>
    where
        Self: 'p;
    /// Values each symbol yields.
    const WIDTH: usize;
    /// Symbol `j` uses table phase `j % cycle()`.
    fn cycle(&self) -> usize;
    /// Smallest period of the table sequence (divides `cycle()`); phases equal modulo it
    /// decode identically.
    fn period(&self) -> usize;
    /// Length of the stream in bits.
    fn total_bits(&self) -> u64;
    /// Bits worth speculating over (the declared payload when the data runs on past
    /// it); a true path that needs more is left to the sequential decoder.
    fn span_bits(&self) -> u64 {
        self.total_bits()
    }
    /// Judge an end position (bits consumed after the last symbol).
    fn end_check(&self, pos: u64) -> EndCheck;
    /// A pump positioned at `bit`.
    fn pump_at(&self, bit: u64) -> Self::Pump<'_>;
    fn position(p: &Self::Pump<'_>) -> u64;
    /// Decode one symbol in table phase `phase` into `out` (`WIDTH` values, wrapping
    /// `u16`). Returns false for an invalid code; the pump must still have advanced.
    fn decode(&self, p: &mut Self::Pump<'_>, phase: usize, out: &mut [u16]) -> bool;
}

/// Speculative decoding of one chunk in one table phase.
struct Spec {
    /// Table phase of the first symbol.
    phase: usize,
    /// `(j, j0)`: symbol `j` of this speculation starts where symbol `j0` of the phase-0
    /// speculation of the same chunk does, in the same table phase; this one stopped there.
    merge: Option<(usize, usize)>,
    /// Bit position before symbol `j`, for the first [`SYNC_STATES`] symbols.
    states: Vec<u64>,
    /// `WIDTH` values per decoded symbol.
    values: Vec<u16>,
    /// Symbols at which an invalid code was met.
    errors: Vec<usize>,
    /// Position after the last symbol: the first boundary at or past the chunk end.
    end: u64,
    /// Last chunk: symbols whose end the stream rules accept.
    valid: usize,
    /// Decoded up to the end of the data (no chunk limit).
    limited_by_end: bool,
}

impl Spec {
    fn symbols(&self, width: usize) -> usize {
        self.values.len() / width
    }
}

/// Decoded values addressed by absolute value index (`symbol * WIDTH + k`).
pub(crate) struct Values {
    specs: Vec<Vec<Spec>>,
    /// `(first value index, bridge values, chunk, phase, first value in spec, value count)`.
    runs: Vec<(usize, Vec<u16>, usize, usize, usize, usize)>,
}

impl Values {
    fn run_parts(&self, r: usize) -> (&[u16], &[u16]) {
        let (_, bridge, k, g, from, n) = &self.runs[r];
        let body = if *n == 0 { &[][..] } else { &self.specs[*k][*g].values[*from..from + n] };
        (&bridge[..], body)
    }

    /// Run holding value `i` and the offset within it.
    fn locate(&self, i: usize) -> (usize, usize) {
        let r = self.runs.partition_point(|s| s.0 <= i).saturating_sub(1);
        (r, i - self.runs[r].0)
    }

    pub fn get(&self, i: usize) -> u16 {
        let (r, off) = self.locate(i);
        let (bridge, body) = self.run_parts(r);
        if off < bridge.len() { bridge[off] } else { body[off - bridge.len()] }
    }

    /// Copy values `start .. start + dst.len()` into `dst`.
    pub fn copy_to(&self, start: usize, dst: &mut [u16]) {
        let (mut r, mut off) = self.locate(start);
        let mut done = 0;
        while done < dst.len() && r < self.runs.len() {
            let (bridge, body) = self.run_parts(r);
            for part in [bridge, body] {
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
            r += 1;
            off = 0;
        }
    }
}

/// Chunk size for a stream of `bytes` with the current thread count, or `None` when
/// the stream is too small to be worth splitting.
pub(crate) fn default_chunk(bytes: usize) -> Option<usize> {
    let tasks = par::threads() * 4;
    if tasks <= 4 || bytes < 2 * MIN_CHUNK_BYTES {
        return None;
    }
    Some(bytes.div_ceil(tasks).max(MIN_CHUNK_BYTES))
}

/// Decode the first `symbols` symbols of `s` on all threads (`chunk` bytes per task).
pub(crate) fn decode<S: SymbolStream>(s: &S, symbols: usize, chunk: usize) -> Option<Values> {
    let chunk_bits = chunk.max(1) as u64 * 8;
    let span = s.span_bits().min(s.total_bits());
    let nchunks = span.div_ceil(chunk_bits).max(1) as usize;
    let period = s.period().max(1);
    let specs: Vec<Vec<Spec>> = par::map((0..nchunks).collect(), true, |k| {
        let start = k as u64 * chunk_bits;
        let limit = if k + 1 == nchunks && span == s.total_bits() {
            None
        } else {
            Some(((k as u64 + 1) * chunk_bits).min(span))
        };
        let first = speculate(s, 0, start, limit, symbols, None);
        let mut all = Vec::with_capacity(period);
        let rest: Vec<Spec> =
            (1..period).map(|g| speculate(s, g, start, limit, symbols, Some((&first.states, period)))).collect();
        all.push(first);
        all.extend(rest);
        all
    });
    let runs = resolve(s, &specs, symbols)?;
    Some(Values { specs, runs })
}

fn speculate<S: SymbolStream>(
    s: &S,
    phase: usize,
    start: u64,
    limit: Option<u64>,
    cap: usize,
    mut merge_into: Option<(&[u64], usize)>,
) -> Spec {
    let w = S::WIDTH;
    let cycle = s.cycle().max(1);
    let total_bits = s.total_bits();
    let span = limit.unwrap_or(total_bits).saturating_sub(start);
    let mut values: Vec<u16> = Vec::with_capacity(((span / 6) as usize * w).min(cap * w));
    let mut states = Vec::with_capacity(SYNC_STATES.min(span as usize));
    let mut errors = Vec::new();
    let mut pump = s.pump_at(start);
    let mut pos = start;
    let mut c = phase % cycle;
    let mut merge = None;
    let mut out = [0u16; 4];
    let mut n = 0usize;
    // The last chunk runs until its end can no longer be accepted (or `cap` symbols).
    let mut valid = 0usize;
    loop {
        match limit {
            Some(l) if pos >= l => break,
            None if s.end_check(pos) != EndCheck::Valid && pos > total_bits => break,
            _ => {}
        }
        if n > cap {
            break;
        }
        if limit.is_none() && s.end_check(pos) == EndCheck::Valid {
            valid = n;
        }
        if let Some((reference, p)) = merge_into {
            if let Ok(j0) = reference.binary_search(&pos) {
                if j0 % p == (phase + n) % p {
                    states.push(pos);
                    merge = Some((n, j0));
                    break;
                }
            }
            if n >= SYNC_STATES {
                merge_into = None;
            }
        }
        if states.len() < SYNC_STATES {
            states.push(pos);
        }
        if !s.decode(&mut pump, c, &mut out[..w]) {
            errors.push(n);
        }
        values.extend_from_slice(&out[..w]);
        pos = S::position(&pump);
        n += 1;
        c += 1;
        if c == cycle {
            c = 0;
        }
    }
    if limit.is_some() || merge.is_some() {
        valid = n;
    }
    Spec { phase, merge, states, values, errors, end: pos, valid, limited_by_end: limit.is_none() }
}

type Runs = Vec<(usize, Vec<u16>, usize, usize, usize, usize)>;

fn resolve<S: SymbolStream>(s: &S, specs: &[Vec<Spec>], total: usize) -> Option<Runs> {
    let w = S::WIDTH;
    let cycle = s.cycle().max(1);
    let period = s.period().max(1);
    let mut out: Runs = Vec::with_capacity(specs.len());
    let mut n = 0usize;
    let mut pos = 0u64;
    let mut bridge: Vec<u16> = Vec::new();
    let mut pump: Option<S::Pump<'_>> = None;
    let mut buf = [0u16; 4];
    let mut k = 0usize;
    while n < total {
        let Some(phases) = specs.get(k) else { break };
        // The final chunk runs to the end of the data unless the span stopped short of it.
        let last = k + 1 == specs.len() && phases[0].limited_by_end;
        let hit = phases.iter().enumerate().find_map(|(g, sp)| {
            let j = sp.states.binary_search(&pos).ok()?;
            ((sp.phase + j) % period == n % period).then_some((g, j))
        });
        if let Some((g, j)) = hit {
            let sp = &phases[g];
            if let Some((jm, j0)) = sp.merge {
                // Follow this speculation to where it joins phase 0's path.
                if sp.errors.iter().any(|&e| e >= j && e < jm) {
                    return None;
                }
                let take = (jm - j).min(total - n);
                bridge.extend_from_slice(&sp.values[j * w..(j + take) * w]);
                n += take;
                pos = if n == total { sp.states[j + take] } else { phases[0].states[j0] };
                pump = None;
                continue;
            }
            let avail = if last { sp.valid } else { sp.symbols(w) };
            let count = avail.checked_sub(j)?.min(total - n);
            if last && count < total - n {
                return None; // truncated, or an end only the sequential decoder can judge
            }
            if sp.errors.iter().any(|&e| e >= j && e < j + count) {
                return None; // the true path holds an invalid code
            }
            out.push((n * w - bridge.len(), std::mem::take(&mut bridge), k, g, j * w, count * w));
            n += count;
            if n == total {
                // The last chunk's `valid` already applied the end rules; an earlier
                // chunk ends well inside the data.
                return Some(out);
            }
            pos = sp.end;
            pump = None;
            k += 1;
            continue;
        }
        let full = || phases.iter().filter(|sp| sp.merge.is_none());
        if !last && k + 1 < specs.len() && full().all(|sp| pos >= sp.end) {
            // The true path walked past this whole (short) chunk; try the next one.
            k += 1;
            continue;
        }
        if full().all(|sp| sp.states.len() == SYNC_STATES && sp.states.last().is_some_and(|&l| pos > l)) {
            return None; // no synchronisation within the recorded boundaries
        }
        // Advance the true path by one symbol.
        let p = pump.get_or_insert_with(|| s.pump_at(pos));
        if !s.decode(p, n % cycle, &mut buf[..w]) {
            return None;
        }
        bridge.extend_from_slice(&buf[..w]);
        pos = S::position(p);
        n += 1;
    }
    if n < total || s.end_check(pos) != EndCheck::Valid {
        return None;
    }
    if !bridge.is_empty() {
        out.push((n * w - bridge.len(), bridge, 0, 0, 0, 0));
    }
    Some(out)
}
