//! Shared body of the Phase One golden test: deterministic streams, folded into one hash.

use flashraw_core::bits::{BitPumpMsb, BitPumpMsb32, BitRead};

pub trait Codec {
    fn l<B: BitRead>(&self, b: &mut B, out: &mut [u16], shift: u32, curve: bool) -> Result<(), String>;
    fn s<B: BitRead>(&self, b: &mut B, out: &mut [u16]) -> Result<(), String>;
    fn unscramble(&self, s: &mut [u16], a: u16, b: u16, f: u32);
}

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

pub struct Fnv(pub u64);
impl Fnv {
    pub fn new() -> Self {
        Fnv(0xcbf29ce484222325)
    }
    pub fn add(&mut self, b: u8) {
        self.0 = (self.0 ^ b as u64).wrapping_mul(0x100000001b3);
    }
    pub fn add_u16s(&mut self, v: &[u16]) {
        for x in v {
            self.add(*x as u8);
            self.add((*x >> 8) as u8);
        }
    }
    pub fn add_str(&mut self, s: &str) {
        for b in s.bytes() {
            self.add(b);
        }
        self.add(0xff);
    }
}

/// Random bytes with a density bias so that short codes, repeats and escapes all occur.
fn stream(r: &mut Rng, n: usize) -> Vec<u8> {
    let mode = r.below(6);
    (0..n)
        .map(|_| {
            let v = r.next();
            match mode {
                0 => v as u8,
                1 => (v & v >> 8) as u8,
                2 => (v | v >> 8) as u8,
                3 => (v & v >> 8 & v >> 16) as u8,
                4 => (v | v >> 8 | v >> 16) as u8,
                _ => [0x55, 0xaa, 0x00, 0xff, 0x80, 0x01][(v % 6) as usize],
            }
        })
        .collect()
}

fn fold(h: &mut Fnv, res: Result<(), String>, out: &[u16]) {
    match res {
        Ok(()) => {
            h.add(1);
            h.add_u16s(out);
        }
        Err(e) => {
            h.add(0);
            h.add_str(&e);
        }
    }
}

fn l_case<C: Codec>(c: &C, h: &mut Fnv, data: &[u8], width: usize, shift: u32, curve: bool, be: bool) {
    let mut out = vec![0u16; width];
    let res = if be {
        c.l(&mut BitPumpMsb::new(data), &mut out, shift, curve)
    } else {
        c.l(&mut BitPumpMsb32::new(data), &mut out, shift, curve)
    };
    fold(h, res, &out);
}

fn s_case<C: Codec>(c: &C, h: &mut Fnv, data: &[u8], width: usize) {
    let mut out = vec![0u16; width];
    let res = c.s(&mut BitPumpMsb32::new(data), &mut out);
    fold(h, res, &out);
}

pub fn digest<C: Codec>(c: &C) -> u64 {
    let mut h = Fnv::new();
    let mut r = Rng::new(0x9e3779b97f4a7c15);
    let variants = [(2, false), (2, true), (0, false), (0, true)];
    for i in 0..6000 {
        let width = r.below(300) as usize;
        let n = r.below(width as u64 * 2 + 40) as usize;
        let data = stream(&mut r, n);
        let (shift, curve) = variants[i % 4];
        l_case(c, &mut h, &data, width, shift, curve, i % 8 >= 4);
    }
    for _ in 0..8000 {
        let width = r.below(300) as usize;
        let n = r.below(width as u64 * 2 + 40) as usize;
        let data = stream(&mut r, n);
        s_case(c, &mut h, &data, width);
    }
    // Truncation at every byte length (decision at the end-of-data threshold).
    for k in 0..24 {
        let data = stream(&mut r, 160);
        for cut in 0..=data.len() {
            let (shift, curve) = variants[k % 4];
            l_case(c, &mut h, &data[..cut], 64 + (k % 3) * 5, shift, curve, k % 2 == 1);
            s_case(c, &mut h, &data[..cut], 64 + (k % 3) * 5);
        }
    }
    // Arithmetic wrap cases of S: levels driven below zero, high shifts, long runs of one bit value.
    for fill in [0x00u8, 0xff, 0x55, 0xaa, 0x49, 0x92, 0x24] {
        for init_byte in 0u8..8 {
            let mut data = vec![fill; 200];
            data[1] = init_byte; // low byte of the 16-bit prologue (little-endian word: byte 1 is bits 8..15 of the stream)
            data[0] = init_byte;
            s_case(c, &mut h, &data, 96);
            s_case(c, &mut h, &data, 99);
        }
    }
    for _ in 0..300 {
        let mut data = stream(&mut r, 120);
        data[0] = 0;
        data[1] = 0;
        data[2] = 0;
        data[3] = r.below(8) as u8;
        s_case(c, &mut h, &data, 8 * (1 + r.below(10) as usize));
    }
    for i in 0..400 {
        let len = r.below(40) as usize;
        let mut s: Vec<u16> = (0..len).map(|_| r.next() as u16).collect();
        c.unscramble(&mut s, r.next() as u16, r.next() as u16, [1, 2, 0, 3][i % 4]);
        h.add_u16s(&s);
    }
    h.0
}
