//! Tests of the speculative engine on small prefix codes whose answers can be computed directly.

use super::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn bit(data: &[u8], pos: u64) -> u32 {
    data.get((pos / 8) as usize).map_or(0, |b| ((b >> (7 - pos % 8)) & 1) as u32)
}

/// Phase 0 reads a unary code (`0`, `10`, `110`, `1110`; `1111` is invalid), phase 1 a 2-bit code
/// (`11` invalid); a symbol's value is its code number plus 16 times its table.
struct TwoTables<'a> {
    data: &'a [u8],
}

impl TwoTables<'_> {
    fn symbol(&self, pos: u64, table: usize) -> (u64, u16, bool) {
        let (len, value, ok) = if table == 0 {
            let mut n = 0u32;
            while n < 4 && bit(self.data, pos + n as u64) == 1 {
                n += 1;
            }
            if n == 4 { (4, 0, false) } else { (n as u64 + 1, n, true) }
        } else {
            let v = bit(self.data, pos) * 2 + bit(self.data, pos + 1);
            (2, v, v != 3)
        };
        (pos + len, (value + 16 * table as u32) as u16, ok)
    }
}

impl SymbolStream for TwoTables<'_> {
    type Pump<'p>
        = u64
    where
        Self: 'p;
    const WIDTH: usize = 1;
    fn cycle(&self) -> usize {
        2
    }
    fn period(&self) -> usize {
        2
    }
    fn total_bits(&self) -> u64 {
        self.data.len() as u64 * 8
    }
    fn end_check(&self, pos: u64) -> EndCheck {
        if pos <= self.total_bits() + 8 { EndCheck::Valid } else { EndCheck::Invalid }
    }
    fn pump_at(&self, bit: u64) -> u64 {
        bit
    }
    fn position(p: &u64) -> u64 {
        *p
    }
    fn decode(&self, p: &mut u64, phase: usize, out: &mut [u16]) -> bool {
        let (np, v, ok) = self.symbol(*p, phase % 2);
        *p = np;
        out[0] = v;
        ok
    }
}

/// The sequential answer: the first `symbols` values, or `None` when the decoder would reject.
fn reference(s: &TwoTables<'_>, symbols: usize) -> Option<Vec<u16>> {
    let (mut pos, mut out) = (0u64, Vec::new());
    for j in 0..symbols {
        let (np, v, ok) = s.symbol(pos, j % 2);
        if !ok {
            return None;
        }
        out.push(v);
        pos = np;
    }
    (s.end_check(pos) == EndCheck::Valid).then_some(out)
}

fn read_all(v: &Values, n: usize) -> Vec<u16> {
    let mut out = vec![0u16; n];
    v.copy_to(0, &mut out);
    for (i, &x) in out.iter().enumerate() {
        assert_eq!(v.get(i), x);
    }
    out
}

#[test]
fn engine_matches_sequential_decoding_with_tiny_chunks() {
    let mut r = Rng(0x5EED);
    let mut accepted = 0;
    for case in 0..300 {
        let len = 1 + r.below(400) as usize;
        // Mostly zero bits give short unary codes, so most streams stay decodable.
        let data: Vec<u8> = (0..len).map(|_| (r.next() as u8) & (r.next() as u8) & (r.next() as u8 | 0x11)).collect();
        let s = TwoTables { data: &data };
        let symbols = [0, 1, 7, len, len * 2, len * 4][r.below(6) as usize];
        let want = reference(&s, symbols);
        for chunk in [1usize, 2, 3, 5, 13, 64, 1 << 20] {
            let got = decode(&s, symbols, chunk).map(|v| read_all(&v, symbols));
            if let Some(got) = got {
                assert_eq!(Some(&got), want.as_ref(), "case {case} chunk {chunk}");
                accepted += 1;
            } else {
                assert!(want.is_none(), "case {case} chunk {chunk}: declined a decodable stream");
            }
        }
    }
    assert!(accepted > 200, "{accepted}");
}

#[test]
fn a_stream_cut_short_is_declined() {
    let data = vec![0u8; 100];
    let s = TwoTables { data: &data };
    // Every symbol of an all-zero stream is one bit in phase 0 and two bits in phase 1.
    assert!(decode(&s, 200, 7).is_some());
    assert!(decode(&s, 600, 7).is_none());
}

#[test]
fn invalid_codes_on_the_true_path_are_declined() {
    let mut data = vec![0u8; 64];
    data[30] = 0xFF;
    let s = TwoTables { data: &data };
    assert!(reference(&s, 400).is_none());
    for chunk in [1, 4, 16, 1000] {
        assert!(decode(&s, 400, chunk).is_none(), "chunk {chunk}");
    }
}

#[test]
fn values_read_across_pieces() {
    let v = Values {
        bufs: vec![vec![1, 2, 3, 4], vec![5, 6, 7]],
        runs: vec![Run { start: 0, buf: 0, off: 1, len: 3 }, Run { start: 3, buf: 1, off: 0, len: 3 }],
    };
    let mut out = [0u16; 6];
    v.copy_to(0, &mut out);
    assert_eq!(out, [2, 3, 4, 5, 6, 7]);
    assert_eq!(v.get(4), 6);
    assert_eq!(v.slice(1, 2), Some(&[3u16, 4][..]));
    assert_eq!(v.slice(2, 2), None);
}

#[test]
fn default_chunk_rule() {
    assert_eq!(default_chunk(1000), None);
    assert_eq!(par::sequential(|| default_chunk(100_000_000)), None);
    if par::threads() > 1 {
        let c = default_chunk(100_000_000).unwrap();
        assert!((1..100_000_000).contains(&c));
    }
    assert!(default_chunk(usize::MAX / 2).is_none() || par::threads() > 1);
}
