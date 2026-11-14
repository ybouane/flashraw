//! Golden tests of `dng_opcodes`: parsing of random and mutated opcode lists (entries and
//! error text), and application of random opcodes to random windows through `apply`,
//! `apply_u16`, `u16_exact` and `RowOps` (samples bit for bit, skipped names), reduced to
//! FNV-1a digests over fixed pseudo-random cases.
//!
//! The digests were recorded from a run in which the previous implementation and the current
//! one were compared case by case on exactly these inputs (same generators, same seeds) and
//! agreed on every result: equal `Debug` output or error text for every list, equal samples
//! (NaN payloads ignored) and names for every application.

use flashraw_codecs::dng_opcodes as new;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
    fn u32_below(&mut self, n: u32) -> u32 {
        self.below(n as usize) as u32
    }
    fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
    /// Uniform in `[lo, hi)`.
    fn f64_in(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.next() >> 11) as f64 / (1u64 << 53) as f64 * (hi - lo)
    }
    /// A double, sometimes a special value.
    fn f64_odd(&mut self, lo: f64, hi: f64) -> f64 {
        match self.below(40) {
            0 => f64::NAN,
            1 => f64::INFINITY,
            2 => -0.0,
            3 => 0.0,
            4 => -self.f64_in(lo, hi),
            _ => self.f64_in(lo, hi),
        }
    }
}

// ---------------------------------------------------------------- opcodes as plain data

#[derive(Clone, Debug)]
struct Reg([u32; 8]);

#[derive(Clone, Debug)]
enum Spec {
    Warp(Vec<([f64; 4], [f64; 2])>, f64, f64),
    Fisheye,
    Vignette([f64; 5], f64, f64),
    BadConst(u32, u32),
    BadList(u32, Vec<(u32, u32)>, Vec<(u32, u32, u32, u32)>),
    Trim([u32; 4]),
    Table(Reg, Vec<u16>),
    Poly(Reg, Vec<f64>),
    Gain(Reg, u32, u32, [f64; 4], u32, Vec<f32>),
    PerLine(u32, Reg, Vec<f32>),
    Other(u32, bool),
}

macro_rules! region {
    ($m:ident, $r:expr) => {{
        let r = &$r.0;
        $m::Region {
            top: r[0],
            left: r[1],
            bottom: r[2],
            right: r[3],
            plane: r[4],
            planes: r[5],
            row_pitch: r[6],
            col_pitch: r[7],
        }
    }};
}

macro_rules! build {
    ($m:ident, $s:expr) => {{
        use $m::Opcode as O;
        match $s.clone() {
            Spec::Warp(planes, cx, cy) => O::WarpRectilinear { planes, cx, cy },
            Spec::Fisheye => O::WarpFisheye,
            Spec::Vignette(k, cx, cy) => O::FixVignetteRadial { k, cx, cy },
            Spec::BadConst(constant, bayer_phase) => O::FixBadPixelsConstant { constant, bayer_phase },
            Spec::BadList(bayer_phase, points, rects) => O::FixBadPixelsList { bayer_phase, points, rects },
            Spec::Trim([top, left, bottom, right]) => O::TrimBounds { top, left, bottom, right },
            Spec::Table(r, table) => O::MapTable { region: region!($m, r), table },
            Spec::Poly(r, coeffs) => O::MapPolynomial { region: region!($m, r), coeffs },
            Spec::Gain(r, points_v, points_h, [spacing_v, spacing_h, origin_v, origin_h], map_planes, gains) => {
                O::GainMap {
                    region: region!($m, r),
                    points_v,
                    points_h,
                    spacing_v,
                    spacing_h,
                    origin_v,
                    origin_h,
                    map_planes,
                    gains,
                }
            }
            Spec::PerLine(10, r, v) => O::DeltaPerRow { region: region!($m, r), deltas: v },
            Spec::PerLine(11, r, v) => O::DeltaPerColumn { region: region!($m, r), deltas: v },
            Spec::PerLine(12, r, v) => O::ScalePerRow { region: region!($m, r), scales: v },
            Spec::PerLine(_, r, v) => O::ScalePerColumn { region: region!($m, r), scales: v },
            Spec::Other(id, optional) => O::Other { id, optional },
        }
    }};
}

fn rand_region(g: &mut Rng, w: u32, h: u32, planes: u32) -> Reg {
    let t = g.u32_below(h + 2);
    let l = g.u32_below(w + 2);
    let b = if g.chance(1, 5) { u32::MAX } else { t + g.u32_below(h + 3) };
    let r = if g.chance(1, 5) { u32::MAX } else { l + g.u32_below(w + 3) };
    let plane = g.u32_below(planes + 1);
    let np = if g.chance(1, 8) { u32::MAX } else { g.u32_below(planes + 2) };
    // Pitch 0 is stored as 1 by the parser; constructed regions keep pitches >= 1.
    Reg([t, l, b, r, plane, np, 1 + g.u32_below(4), 1 + g.u32_below(4)])
}

fn rand_spec(g: &mut Rng, w: u32, h: u32, planes: u32, constants: &[u32]) -> Spec {
    match g.below(15) {
        0 => {
            let n = g.below(5);
            let planes = (0..n)
                .map(|_| {
                    (
                        [g.f64_odd(0.0, 2.0), g.f64_odd(0.0, 1.0), g.f64_odd(0.0, 1.0), g.f64_odd(0.0, 1.0)],
                        [g.f64_odd(0.0, 1.0), g.f64_odd(0.0, 1.0)],
                    )
                })
                .collect();
            Spec::Warp(planes, g.f64_odd(0.0, 1.0), g.f64_odd(0.0, 1.0))
        }
        1 => {
            if g.chance(1, 2) {
                Spec::Fisheye
            } else {
                Spec::Trim([g.u32_below(h), g.u32_below(w), g.u32_below(h), g.u32_below(w)])
            }
        }
        2 => {
            let k = [
                g.f64_odd(-0.5, 0.8),
                g.f64_odd(-0.3, 0.3),
                g.f64_odd(-0.1, 0.1),
                g.f64_in(-0.05, 0.05),
                g.f64_in(-0.01, 0.01),
            ];
            let c = if g.chance(1, 8) { 1.7 } else { g.f64_odd(0.0, 1.0) };
            Spec::Vignette(k, c, g.f64_odd(0.0, 1.0))
        }
        3 => Spec::BadConst(constants[g.below(constants.len())], g.u32_below(4)),
        4 => {
            let pts = (0..g.below(12)).map(|_| (g.u32_below(h + 3), g.u32_below(w + 3))).collect();
            let rects = (0..g.below(3))
                .map(|_| {
                    let (t, l) = (g.u32_below(h + 2), g.u32_below(w + 2));
                    (t, l, t + g.u32_below(4), l + g.u32_below(5))
                })
                .collect();
            Spec::BadList(g.u32_below(4), pts, rects)
        }
        5 | 6 => {
            let n = match g.below(6) {
                0 => 0,
                1 => 1,
                _ => 1 + g.below(5000),
            };
            Spec::Table(rand_region(g, w, h, planes), (0..n).map(|_| g.below(65536) as u16).collect())
        }
        7 => {
            let n = if g.chance(1, 10) { 0 } else { 1 + g.below(9) };
            let c = (0..n).map(|i| if i == 1 { g.f64_in(0.8, 1.2) } else { g.f64_odd(-0.1, 0.1) }).collect();
            Spec::Poly(rand_region(g, w, h, planes), c)
        }
        8..=10 => {
            let (pv, ph) = (g.u32_below(6), g.u32_below(6));
            let mp = g.u32_below(5);
            let mut n = (pv * ph * mp) as usize;
            if g.chance(1, 10) {
                n = n.saturating_sub(1);
            }
            let gains = (0..n).map(|_| if g.chance(1, 50) { f32::NAN } else { g.f64_in(0.5, 3.0) as f32 }).collect();
            let sp = |g: &mut Rng| match g.below(8) {
                0 => 0.0,
                1 => -0.25,
                _ => g.f64_in(0.05, 0.6),
            };
            let params = [sp(g), sp(g), g.f64_odd(-0.2, 0.3), g.f64_odd(-0.2, 0.3)];
            Spec::Gain(rand_region(g, w, h, planes), pv, ph, params, mp, gains)
        }
        11..=13 => {
            let id = 10 + g.u32_below(4);
            let n = g.below((w.max(h) + 4) as usize);
            let v = (0..n)
                .map(|_| if id < 12 { g.f64_in(-0.01, 0.01) as f32 } else { g.f64_in(0.5, 1.5) as f32 })
                .collect();
            Spec::PerLine(id, rand_region(g, w, h, planes), v)
        }
        _ => Spec::Other(14 + g.u32_below(100), g.chance(1, 2)),
    }
}

// ---------------------------------------------------------------- serialisation

fn reg_bytes(out: &mut Vec<u8>, r: &Reg, g: &mut Rng) {
    for (i, v) in r.0.iter().enumerate() {
        // Pitch 0 in the file must behave as 1.
        let v = if i >= 6 && *v == 1 && g.chance(1, 4) { 0 } else { *v };
        out.extend(v.to_be_bytes());
    }
}

fn body(s: &Spec, g: &mut Rng) -> (u32, Vec<u8>) {
    let mut b = Vec::new();
    let f64s = |b: &mut Vec<u8>, v: &[f64]| v.iter().for_each(|x| b.extend(x.to_bits().to_be_bytes()));
    let id = match s {
        Spec::Warp(planes, cx, cy) => {
            b.extend((planes.len() as u32).to_be_bytes());
            for (kr, kt) in planes {
                f64s(&mut b, kr);
                f64s(&mut b, kt);
            }
            f64s(&mut b, &[*cx, *cy]);
            1
        }
        Spec::Fisheye => 2,
        Spec::Vignette(k, cx, cy) => {
            f64s(&mut b, k);
            f64s(&mut b, &[*cx, *cy]);
            3
        }
        Spec::BadConst(c, p) => {
            b.extend(c.to_be_bytes());
            b.extend(p.to_be_bytes());
            4
        }
        Spec::BadList(p, pts, rects) => {
            b.extend(p.to_be_bytes());
            b.extend((pts.len() as u32).to_be_bytes());
            b.extend((rects.len() as u32).to_be_bytes());
            pts.iter().for_each(|&(r, c)| [r, c].iter().for_each(|v| b.extend(v.to_be_bytes())));
            rects.iter().for_each(|&(t, l, bo, r)| [t, l, bo, r].iter().for_each(|v| b.extend(v.to_be_bytes())));
            5
        }
        Spec::Trim(v) => {
            v.iter().for_each(|x| b.extend(x.to_be_bytes()));
            6
        }
        Spec::Table(r, t) => {
            reg_bytes(&mut b, r, g);
            b.extend((t.len() as u32).to_be_bytes());
            t.iter().for_each(|x| b.extend(x.to_be_bytes()));
            7
        }
        Spec::Poly(r, c) => {
            reg_bytes(&mut b, r, g);
            b.extend((c.len() as u32).wrapping_sub(1).to_be_bytes());
            f64s(&mut b, c);
            8
        }
        Spec::Gain(r, pv, ph, p, mp, gains) => {
            reg_bytes(&mut b, r, g);
            b.extend(pv.to_be_bytes());
            b.extend(ph.to_be_bytes());
            f64s(&mut b, p);
            b.extend(mp.to_be_bytes());
            gains.iter().for_each(|x| b.extend(x.to_bits().to_be_bytes()));
            9
        }
        Spec::PerLine(id, r, v) => {
            reg_bytes(&mut b, r, g);
            b.extend((v.len() as u32).to_be_bytes());
            v.iter().for_each(|x| b.extend(x.to_bits().to_be_bytes()));
            *id
        }
        Spec::Other(id, _) => {
            b.extend((0..g.below(20)).map(|_| g.next() as u8));
            *id
        }
    };
    if g.chance(1, 6) {
        b.extend((0..g.below(9)).map(|_| g.next() as u8)); // trailing bytes
    }
    (id, b)
}

fn list_bytes(specs: &[Spec], g: &mut Rng) -> Vec<u8> {
    let mut out = (specs.len() as u32).to_be_bytes().to_vec();
    for s in specs {
        let (id, b) = body(s, g);
        let flags = match s {
            Spec::Other(_, opt) => (*opt as u32) | (g.u32_below(2) << 1),
            _ => g.u32_below(4) | if g.chance(1, 10) { 0xF0 } else { 0 },
        };
        out.extend(id.to_be_bytes());
        out.extend((0x0103_0000u32 + g.u32_below(3)).to_be_bytes());
        out.extend(flags.to_be_bytes());
        out.extend((b.len() as u32).to_be_bytes());
        out.extend(b);
    }
    out
}

fn mutate(mut b: Vec<u8>, g: &mut Rng) -> Vec<u8> {
    match g.below(6) {
        0 => {
            let n = g.below(b.len() + 1);
            b.truncate(n);
        }
        1 => {
            for _ in 0..1 + g.below(4) {
                if !b.is_empty() {
                    let i = g.below(b.len());
                    b[i] = g.next() as u8;
                }
            }
        }
        // A big count somewhere on a 4-byte boundary.
        2 if b.len() >= 4 => {
            let i = g.below(b.len() / 4) * 4;
            let v: u32 = [4096, 4097, 1 << 24, (1 << 24) + 1, u32::MAX, 0x8000_0000][g.below(6)];
            b[i..i + 4].copy_from_slice(&v.to_be_bytes());
        }
        3 => b.extend((0..g.below(40)).map(|_| g.next() as u8)),
        _ => {}
    }
    b
}

// ---------------------------------------------------------------- digests

/// FNV-1a, 64-bit.
struct Digest(u64);
impl Digest {
    fn new() -> Self {
        Digest(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
        self.bytes(&[0xff]);
    }
    /// f32 samples, every NaN made the same (NaN payloads are not part of the contract).
    fn f32s(&mut self, v: &[f32]) {
        for x in v {
            let b = if x.is_nan() { 0x7fc0_0000 } else { x.to_bits() };
            self.bytes(&b.to_le_bytes());
        }
    }
    fn u16s(&mut self, v: &[u16]) {
        for x in v {
            self.bytes(&x.to_le_bytes());
        }
    }
}

fn parse_digest(d: &mut Digest, bytes: &[u8]) {
    match new::parse_entries(bytes) {
        Ok(v) => d.str(&format!("ok {v:?}")),
        Err(e) => d.str(&format!("err {e}")),
    }
    let a = new::parse(bytes).map(|v| format!("{v:?}")).map_err(|e| e.to_string());
    let b = new::parse_entries(bytes)
        .map(|v| format!("{:?}", v.into_iter().map(|e| e.op).collect::<Vec<_>>()))
        .map_err(|e| e.to_string());
    assert_eq!(a, b, "parse and parse_entries disagree");
}

#[test]
fn parse_golden() {
    let mut g = Rng(0x9e37_79b9_7f4a_7c15);
    let mut d = Digest::new();
    for round in 0..40_000 {
        let (w, h) = (1 + g.u32_below(64), 1 + g.u32_below(64));
        let specs: Vec<Spec> = (0..g.below(5)).map(|_| rand_spec(&mut g, w, h, 4, &[0, 7, 65535])).collect();
        let bytes = list_bytes(&specs, &mut g);
        parse_digest(&mut d, &bytes);
        let m = mutate(bytes, &mut g);
        parse_digest(&mut d, &m);
        if round % 1000 == 0 {
            let junk: Vec<u8> = (0..g.below(64)).map(|_| g.next() as u8).collect();
            parse_digest(&mut d, &junk);
        }
    }
    for fixed in [&[][..], &[0, 0, 0], &[0, 0, 0, 0], &[0, 0, 0x10, 0x01], &[0, 0, 0, 1, 0, 0, 0, 9]] {
        parse_digest(&mut d, fixed);
    }
    assert_eq!(d.0, PARSE_DIGEST, "parse digest {:#018x}", d.0);
}

struct Case {
    w: usize,
    h: usize,
    p: usize,
    stride: usize,
    x0: usize,
    y0: usize,
    fw: usize,
    fh: usize,
    len: usize,
}

fn rand_case(g: &mut Rng, big: bool) -> Case {
    let (fw, fh) = if big { (200 + g.below(300), 150 + g.below(200)) } else { (1 + g.below(40), 1 + g.below(40)) };
    let p = [1, 1, 1, 2, 3, 3, 4][g.below(7)];
    let (x0, y0) = if g.chance(1, 2) { (0, 0) } else { (g.below(fw), g.below(fh)) };
    let w = if g.chance(1, 20) { 0 } else { 1 + g.below(fw - x0) };
    let h = if g.chance(1, 20) { 0 } else { 1 + g.below(fh - y0) };
    let stride = if g.chance(1, 20) { w.saturating_sub(1) } else { w + if g.chance(1, 2) { 0 } else { g.below(4) } };
    let full = (h * stride * p).max((h.saturating_sub(1) * stride + w) * p);
    let len = match g.below(10) {
        0 => full.saturating_sub(1 + g.below(5)),
        1 => full + g.below(30),
        _ => full,
    };
    // Coordinates beyond the declared whole image are allowed (callers' windows always fit).
    let (fw, fh) = if g.chance(1, 20) { (fw.saturating_sub(3), fh.saturating_sub(3)) } else { (fw, fh) };
    Case { w, h, p, stride, x0, y0, fw, fh, len }
}

macro_rules! image {
    ($m:ident, $data:expr, $c:expr) => {
        $m::PlaneImage {
            data: $data,
            width: $c.w,
            height: $c.h,
            planes: $c.p,
            stride: $c.stride,
            x0: $c.x0,
            y0: $c.y0,
            full_width: $c.fw,
            full_height: $c.fh,
        }
    };
}

#[test]
fn apply_golden() {
    let mut g = Rng(0x2545_f491_4f6c_dd1d);
    let mut d = Digest::new();
    for round in 0..20_000 {
        let c = rand_case(&mut g, round % 50 == 0);
        let scale = [1.0f32, 1.0, 65535.0, 65535.0, 65535.0 / 4095.0][g.below(5)];
        let period = [1usize, 2, 2, 2, 3, 6, 0][g.below(7)];
        let consts = [0u32, 1023, 4095, 65535];
        let base: Vec<f32> = (0..c.len)
            .map(|_| match g.below(60) {
                0 => f32::NAN,
                1 => -g.f64_in(0.0, 0.1) as f32,
                2..=6 => consts[g.below(4)] as f32 / scale,
                _ => g.f64_in(0.0, 1.1) as f32 * 65535.0 / scale,
            })
            .collect();
        let specs: Vec<Spec> =
            (0..1 + g.below(4)).map(|_| rand_spec(&mut g, c.fw as u32, c.fh as u32, c.p as u32, &consts)).collect();
        let ops: Vec<new::Opcode> = specs.iter().map(|s| build!(new, s)).collect();
        let mut first: Option<Vec<f32>> = None;
        for parallel in [false, true] {
            let mut a = base.clone();
            let names = new::apply(&ops, &mut image!(new, &mut a[..], c), period, scale, parallel);
            d.str(&format!("{names:?}"));
            d.f32s(&a);
            match &first {
                None => first = Some(a),
                Some(f) => assert!(
                    f.iter().zip(&a).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())),
                    "parallel apply differs: {specs:?}"
                ),
            }
        }
        // Row by row (only for lists RowOps accepts).
        let rows = new::RowOps::new(&ops, c.w, c.p, c.x0, c.fw, c.fh, scale);
        d.str(&format!("{} {}", rows.is_some(), rows.as_ref().is_some_and(|r| r.is_empty())));
        if let Some(rows) = rows {
            for y in 0..c.fh + 3 {
                let mut row: Vec<f32> = (0..c.w * c.p).map(|_| g.f64_in(0.0, 1.0) as f32 * 65535.0 / scale).collect();
                rows.apply(y, &mut row);
                d.f32s(&row);
            }
        }
        // Stored integers.
        d.str(&format!("{}", new::u16_exact(&ops)));
        let base16: Vec<u16> = (0..c.len)
            .map(|_| if g.chance(1, 15) { consts[g.below(4)] as u16 } else { g.below(66000).min(65535) as u16 })
            .collect();
        for parallel in [false, true] {
            let mut a = base16.clone();
            let names = new::apply_u16(&ops, &mut image!(new, &mut a[..], c), period, parallel);
            d.str(&format!("{names:?}"));
            d.u16s(&a);
        }
    }
    assert_eq!(d.0, APPLY_DIGEST, "apply digest {:#018x}", d.0);
}

#[test]
fn names_and_classes_golden() {
    let mut g = Rng(77);
    let mut d = Digest::new();
    for _ in 0..2000 {
        let s = rand_spec(&mut g, 30, 30, 3, &[0]);
        let o = build!(new, &s);
        d.str(&format!("{} {} {}", o.name(), o.is_row_local(), o.is_pixel_opcode()));
    }
    assert_eq!(d.0, NAMES_DIGEST, "names digest {:#018x}", d.0);
    let means: Vec<u16> = [0.0f32, 0.4999, 0.5, 1.5, 2.5, 65534.5, 65535.4, 70000.0, -3.0, f32::NAN]
        .iter()
        .map(|&v| <u16 as new::Sample>::from_mean(v))
        .collect();
    assert_eq!(means, [0, 0, 1, 2, 3, 65535, 65535, 65535, 0, 0]);
}

const PARSE_DIGEST: u64 = 0xf1a7_c483_3f44_ae8f;
const APPLY_DIGEST: u64 = 0x9646_0c8f_4975_4c40;
const NAMES_DIGEST: u64 = 0x928b_b00a_392d_a395;
