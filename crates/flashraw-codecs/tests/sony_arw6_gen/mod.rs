//! Synthetic ARW6 strip generator shared by the ARW6 tests (deterministic for a seed).
#![allow(dead_code)]

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// Shape of the coefficient payload bytes.
#[derive(Clone, Copy)]
pub enum Fill {
    Uniform,
    Sparse,
    Dense,
    Small,
}

pub struct Options {
    /// Packets per group beyond the ones the decoder uses (0..=6).
    pub extra_packets: usize,
    /// Probability (percent) that a stream gets one random corruption.
    pub flaw: usize,
    pub fill: Fill,
    /// Largest record payload in bytes.
    pub max_record: usize,
    /// Allow selectors up to 15 (otherwise 0..=3).
    pub wide_selectors: bool,
}

fn fill_bytes(rng: &mut Rng, n: usize, fill: Fill) -> Vec<u8> {
    (0..n)
        .map(|_| match fill {
            Fill::Uniform => rng.next() as u8,
            Fill::Sparse => {
                if rng.chance(85) {
                    0
                } else {
                    rng.next() as u8
                }
            }
            Fill::Dense => (rng.next() as u8) | 0x88,
            Fill::Small => [0x00, 0x01, 0x80, 0x40, 0x20, 0x10, 0xff, 0x0f][rng.below(8)],
        })
        .collect()
}

fn put24(b: &mut [u8], at: usize, v: usize) {
    b[at] = (v >> 16) as u8;
    b[at + 1] = (v >> 8) as u8;
    b[at + 2] = v as u8;
}

/// One packet: `kind` components per record, `nrec` records.
fn packet(rng: &mut Rng, kind: usize, nrec: usize, o: &Options) -> Vec<u8> {
    let mut lens = Vec::new();
    let mut sels = Vec::new();
    let mut payload = Vec::new();
    for _ in 0..nrec {
        let len = if rng.chance(5) { 0 } else { rng.below(o.max_record + 1) };
        lens.push(len);
        let mut s = [0usize; 3];
        for v in s.iter_mut().take(kind) {
            *v = if o.wide_selectors { rng.below(16) } else { rng.below(4) };
        }
        sels.push(s);
        payload.extend(fill_bytes(rng, len, o.fill));
    }
    let mut bits = Vec::new();
    for (l, s) in lens.iter().zip(&sels) {
        for i in (0..16).rev() {
            bits.push(((l >> i) & 1) as u8);
        }
        for v in s.iter().take(kind) {
            for i in (0..4).rev() {
                bits.push(((v >> i) & 1) as u8);
            }
        }
    }
    let mut table = vec![0u8; bits.len().div_ceil(8)];
    for (i, b) in bits.iter().enumerate() {
        table[i / 8] |= b << (7 - i % 8);
    }
    let control = (16 + table.len()).div_ceil(16) * 16 + 16 * rng.below(2);
    payload.resize(payload.len().div_ceil(16) * 16 + 16 * rng.below(2), 0);
    let mut p = vec![0u8; control];
    let ctl = control / 16 - 1;
    p[0] = (ctl >> 8) as u8;
    p[1] = ctl as u8;
    put24(&mut p, 2, payload.len() / 16);
    p[5] = 0x40;
    p[6] = (kind as u8) << 6;
    p[7] = (nrec >> 8) as u8;
    p[8] = nrec as u8;
    p[9] = 0x10;
    p[16..16 + table.len()].copy_from_slice(&table);
    p.extend(payload);
    p
}

/// A tile stream for a `w x (2 * hh)` tile.
pub fn stream(rng: &mut Rng, w: usize, hh: usize, o: &Options) -> Vec<u8> {
    let h = 2 * hh;
    let n16 = h.div_ceil(16);
    let padded = h % 16 != 0;
    let mut groups: Vec<Vec<Vec<u8>>> = Vec::new();
    for g in 0..5 {
        let (kind, rm) = [(1, 1), (3, 1), (3, 2), (3, 4), (1, 8)][g];
        let used = if g < 4 { 3 } else { 1 };
        let mut pk = Vec::new();
        for _ in 0..used {
            let nrec = if padded && rng.chance(50) { n16 } else { n16 + 1 };
            let _ = rm;
            pk.push(packet(rng, kind, nrec, o));
        }
        for _ in 0..o.extra_packets.min(9 - used) {
            let k = if rng.chance(50) { 1 } else { 3 };
            let nr = 1 + rng.below(4);
            pk.push(packet(rng, k, nr, o));
        }
        groups.push(pk);
    }
    let mut dir = Vec::new();
    let mut body = Vec::new();
    let mut sizes = [0usize; 5];
    for (g, pk) in groups.iter().enumerate() {
        let n = pk.len();
        let mut rec = vec![0u8; if n >= 5 { 32 } else { 16 }];
        rec[0] = n as u8 | ((rng.below(16) as u8) << 4);
        let mut total = 0;
        for (i, p) in pk.iter().enumerate() {
            let at = if i < 5 { 1 + 3 * i } else { 0x11 + 3 * (i - 5) };
            put24(&mut rec, at, p.len() / 16);
            total += p.len();
            body.extend(p);
        }
        let slack = 16 * rng.below(3);
        body.extend(vec![0u8; slack]);
        sizes[g] = total + slack;
        dir.extend(rec);
    }
    let mut s = vec![0u8; 0x80];
    s[..4].copy_from_slice(if rng.chance(90) { b"A000" } else { b"0000" });
    s[8] = (w >> 8) as u8;
    s[9] = w as u8;
    s[0x0a] = (hh >> 8) as u8;
    s[0x0b] = hh as u8;
    s[0x0c] = 0x3d;
    s[0x0e] = 0x6e;
    for (g, sz) in sizes.iter().enumerate() {
        put24(&mut s, 0x10 + 3 * g, sz / 16);
    }
    let dlen = dir.len().min(0x50);
    s[0x30..0x30 + dlen].copy_from_slice(&dir[..dlen]);
    s.extend(body);
    if dir.len() > 0x50 {
        // A long directory record runs on into the packet bytes.
        let extra = dir.len() - 0x50;
        s[0x80..0x80 + extra].copy_from_slice(&dir[0x50..]);
    }
    if rng.chance(o.flaw) && s.len() > 0x30 {
        let at = rng.below(s.len().min(0x200));
        s[at] ^= 1 << rng.below(8);
    }
    s
}

pub struct Case {
    pub strip: Vec<u8>,
    pub raw_w: usize,
    pub raw_h: usize,
}

/// A whole strip with a tile table of one to three tiles.
pub fn case(seed: u64) -> Case {
    let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) ^ 0x51);
    let fills = [Fill::Uniform, Fill::Sparse, Fill::Dense, Fill::Small];
    let o = Options {
        extra_packets: if rng.chance(25) { rng.below(7) } else { 0 },
        flaw: 10,
        fill: fills[rng.below(4)],
        max_record: [8, 40, 120, 400][rng.below(4)],
        wide_selectors: rng.chance(50),
    };
    let tiles = if rng.chance(70) { 1 } else { 2 + rng.below(2) };
    let mut strip = vec![0u8; 0x200];
    let mut entries = Vec::new();
    let (mut fw, mut fh) = (0, 0);
    let mut at = 0x200;
    for t in 0..tiles {
        let w = 16 * (1 + rng.below(6));
        let hh = if rng.chance(10) { 1 + rng.below(8) } else { 9 + rng.below(16) };
        let (x, y) = if tiles == 1 { (0, 0) } else { (16 * rng.below(5), 2 * rng.below(8)) };
        let s = stream(&mut rng, w, hh, &o);
        let gap = if t == 0 && rng.chance(60) { 0 } else { 16 * rng.below(6) };
        at += gap;
        strip.resize(at, 0);
        let hint = if t == 0 && rng.chance(50) { 0 } else { at - 16 * rng.below(((at - 0x200) / 16).min(4) + 1) };
        let (tw, th) = (if rng.chance(50) { 0 } else { w }, if rng.chance(50) { 0 } else { 2 * hh });
        entries.push([hint, 0, x, y, tw, th]);
        at += s.len();
        strip.extend(s);
        fw = fw.max(x + w);
        fh = fh.max(y + 2 * hh);
    }
    if tiles > 1 || rng.chance(50) {
        strip[..4].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        for (i, e) in entries.iter().enumerate() {
            for (k, v) in e.iter().enumerate() {
                let p = 8 + 24 * i + 4 * k;
                strip[p..p + 4].copy_from_slice(&(*v as u32).to_le_bytes());
            }
        }
    }
    let slack = 16 * rng.below(4);
    strip.extend(fill_bytes(&mut rng, slack, Fill::Uniform));
    let mut raw_w = fw.div_ceil(16) * 16;
    let mut raw_h = fh.div_ceil(2) * 2;
    if rng.chance(8) {
        raw_w += 16;
    }
    if rng.chance(8) {
        raw_h += 2;
    }
    Case { strip, raw_w, raw_h }
}
