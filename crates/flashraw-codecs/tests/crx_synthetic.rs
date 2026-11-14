//! Property and golden-value tests of the CRX codec on synthetic samples.
//!
//! Samples are generated from the format description (`CMP1` header, per-sample header with tile / plane / subband
//! records, random bit streams in several statistical modes, optional corruption). The decoder must never panic,
//! must fail with one of the documented error kinds, must leave the output untouched on failure and must not depend
//! on the `parallel` flag. The digest over the first 3000 cases (header fields, error kinds and every output sample)
//! was recorded from a differential run against the previous implementation, which agreed on 250 000 cases.

use flashraw_codecs::canon_crx::{CrxHeader, decode};

const SENTINEL: u16 = 0xA5A5;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { (self.next() >> 11) % n }
    }
    fn range(&mut self, a: u64, b: u64) -> u64 {
        a + self.below(b - a + 1)
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

struct Params {
    version: u16,
    planes: u8,
    bits: u8,
    cfa: u8,
    enc: u8,
    levels: u8,
    pw: usize,
    ph: usize,
    tw: usize,
    th: usize,
}

fn gen_params(r: &mut Rng) -> Params {
    let planes = if r.chance(15) { 1 } else { 4 };
    let enc = if planes == 1 {
        0
    } else {
        match r.below(100) {
            0..=69 => 0,
            70..=84 => 1,
            _ => 3,
        }
    };
    let bits = if planes == 1 {
        8
    } else if enc == 1 {
        [1, 4, 7, 9, 10, 11, 12, 13, 14, 15][r.below(10) as usize]
    } else {
        r.range(9, 14) as u8
    };
    let levels = match r.below(100) {
        0..=34 => 0,
        35..=44 => 1,
        45..=59 => 2,
        _ => 3,
    };
    let tw = r.range(22, 60) as usize;
    let th = r.range(22, 50) as usize;
    let cols = r.range(1, 3) as usize;
    let rows = r.range(1, 3) as usize;
    let pw = tw * (cols - 1) + r.range(22, tw as u64) as usize;
    let ph = th * (rows - 1) + r.range(22, th as u64) as usize;
    // A single tile column/row covers the whole plane (tile size must not exceed the image size).
    let tw = if cols == 1 { pw } else { tw };
    let th = if rows == 1 { ph } else { th };
    Params {
        version: if r.chance(50) { 0x100 } else { 0x200 },
        planes,
        bits,
        cfa: if planes == 1 { 0 } else { r.below(4) as u8 },
        enc,
        levels,
        pw,
        ph,
        tw,
        th,
    }
}

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

/// `n` stream bytes in one of several statistical modes.
fn stream_bytes(r: &mut Rng, n: usize, mode: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            let b = r.next() as u8;
            match mode {
                0 => b,
                1 => b & (r.next() as u8) & (r.next() as u8), // zero heavy: long prefixes, escapes
                2 => b | (r.next() as u8) | (r.next() as u8), // one heavy: short codes, many run bits
                _ => {
                    if r.chance(70) {
                        0
                    } else {
                        b
                    }
                }
            }
        })
        .collect()
}

fn gen_case(r: &mut Rng) -> (Vec<u8>, Vec<u8>, Vec<String>) {
    let p = gen_params(r);
    let mut feat: Vec<String> = vec![
        format!("version {:#x}", p.version),
        format!("enc {} planes {}", p.enc, p.planes),
        format!("levels {}", p.levels),
        format!("enc {} levels {}", p.enc, p.levels),
        format!("cfa {}", p.cfa),
    ];
    {
        let (c, rw) = (p.pw.div_ceil(p.tw), p.ph.div_ceil(p.th));
        feat.push(format!("tiles {}x{}", c.min(3), rw.min(3)));
        if c > 1 && rw > 1 {
            feat.push(format!("levels {} with tiles on all sides", p.levels));
        }
        let w_last = p.pw - p.tw * (c - 1);
        let h_last = p.ph - p.th * (rw - 1);
        for (name, v) in [("tile width", p.tw), ("tile height", p.th), ("last width", w_last), ("last height", h_last)]
        {
            feat.push(format!("{name} mod 8 = {} levels {}", v % 8, p.levels));
        }
    }
    let div = if p.planes == 4 { 2 } else { 1 };
    let cols = p.pw.div_ceil(p.tw);
    let rows = p.ph.div_ceil(p.th);
    let nb = 3 * p.levels as usize + 1;
    let mode = [0, 0, 0, 0, 0, 2, 2, 3, 3, 1][r.below(10) as usize];
    let density = [0.05f64, 0.8, 1.5, 3.0, 3.0][r.below(5) as usize];
    let bad_rounded = r.chance(2);
    let mut hdr = Vec::new();
    let mut bodies: Vec<Vec<u8>> = Vec::new();
    // Defer tile records: they need the tile size, so build bodies first and records afterwards.
    let mut tile_records: Vec<Vec<u8>> = Vec::new();
    for t in 0..cols * rows {
        let (c, rr) = (t % cols, t / cols);
        let w = if c + 1 == cols { p.pw - p.tw * (cols - 1) } else { p.tw };
        let h = if rr + 1 == rows { p.ph - p.th * (rows - 1) } else { p.th };
        let has_qp = p.version == 0x200 && (p.levels > 0 || r.chance(3)) && r.chance(85);
        if has_qp {
            feat.push(format!("qp map levels {}", p.levels));
        }
        let mut body = Vec::new();
        let mut qp_size = 0usize;
        let mut extra = 0usize;
        if has_qp {
            qp_size =
                ((w.div_ceil(8) * h.div_ceil(2)) as f64 * [0.1, 2.0, 3.0, 4.0, 4.0][r.below(5) as usize]) as usize;
            body.extend(stream_bytes(r, qp_size, mode));
            extra = r.below(6) as usize;
            body.extend(stream_bytes(r, extra, 0));
        }
        let mut recs = Vec::new();
        let mut plane_bodies = Vec::new();
        for pi in 0..p.planes as usize {
            let mut prec = Vec::new();
            let predictive = r.chance(75);
            let mut rounded = 0u8;
            if p.levels == 0 && predictive && r.chance(25) {
                rounded = r.range(1, 3) as u8;
            }
            if bad_rounded {
                rounded = r.range(1, 3) as u8; // occasionally invalid combination (levels > 0 or not predictive)
            }
            if rounded != 0 {
                feat.push(format!("rounded_bits {rounded}"));
            }
            if predictive && p.levels > 0 {
                feat.push("predictive LL band".into());
            }
            let mut band_recs = Vec::new();
            let mut pbody = Vec::new();
            for i in 0..nb {
                let ds = if r.chance(4) {
                    0
                } else {
                    ((w * h) as f64 * density * (0.3 + r.below(100) as f64 / 100.0)) as usize
                };
                let tail = if r.chance(70) { 0 } else { r.below(5) as usize };
                let band_size = ds + tail;
                if p.version == 0x200 && r.chance(90) {
                    put16(&mut band_recs, 0xFF13);
                    put16(&mut band_recs, 16);
                    put32(&mut band_recs, band_size as u32);
                    put16(&mut band_recs, (i as u16) << 12);
                    put16(&mut band_recs, r.below(400) as u16); // step_mult
                    put32(&mut band_recs, if r.chance(20) { r.next() as u32 } else { r.below(50) as u32 }); // step_base
                    put16(&mut band_recs, tail as u16);
                    put16(&mut band_recs, 0);
                } else {
                    put16(&mut band_recs, 0xFF03);
                    put16(&mut band_recs, 8);
                    put32(&mut band_recs, band_size as u32);
                    let q = if r.chance(80) { r.range(20, 60) } else { r.below(256) } as u32;
                    let pq = r.chance(10) as u32;
                    if pq == 1 && p.version == 0x100 && p.levels > 0 {
                        feat.push("v1 per-line QP deltas".into());
                    }
                    put32(&mut band_recs, ((i as u32) << 28) | (pq << 27) | (q << 19) | tail as u32);
                }
                pbody.extend(stream_bytes(r, ds, mode));
                pbody.extend(stream_bytes(r, tail, 0));
            }
            put16(&mut prec, 0xFF02);
            put16(&mut prec, 8);
            put32(&mut prec, pbody.len() as u32);
            prec.push(((pi as u8) << 4) | (predictive as u8) << 3 | rounded << 1);
            prec.extend_from_slice(&[0, 0, 0]);
            recs.extend(prec);
            recs.extend(band_recs);
            plane_bodies.push(pbody);
        }
        for pb in plane_bodies {
            body.extend(pb);
        }
        let mut trec = Vec::new();
        if has_qp {
            put16(&mut trec, 0xFF11);
            put16(&mut trec, 16);
            put32(&mut trec, body.len() as u32);
            put16(&mut trec, t as u16);
            put16(&mut trec, 0x4000);
            put32(&mut trec, qp_size as u32);
            put16(&mut trec, extra as u16);
            put16(&mut trec, 0);
        } else {
            put16(&mut trec, if r.chance(10) { 0xFF11 } else { 0xFF01 });
            put16(&mut trec, 8);
            put32(&mut trec, body.len() as u32);
            put16(&mut trec, t as u16);
            put16(&mut trec, 0);
        }
        trec.extend(recs);
        tile_records.push(trec);
        bodies.push(body);
    }
    for t in &tile_records {
        hdr.extend_from_slice(t);
    }
    let mut cmp = vec![0u8; if p.enc == 3 && r.chance(50) { 90 } else { 52 }];
    put_be16(&mut cmp, 4, p.version);
    put_be32(&mut cmp, 8, (p.pw * div) as u32);
    put_be32(&mut cmp, 12, (p.ph * div) as u32);
    put_be32(&mut cmp, 16, (p.tw * div) as u32);
    put_be32(&mut cmp, 20, (p.th * div) as u32);
    cmp[24] = p.bits;
    cmp[25] = (p.planes << 4) | p.cfa;
    cmp[26] = (p.enc << 4) | p.levels;
    put_be32(&mut cmp, 28, hdr.len() as u32);
    if cmp.len() > 84 {
        cmp[32] = 0x80;
        cmp[56] = 0x40;
        cmp[84] = r.range(8, 16) as u8;
    }
    let mut data = hdr;
    for b in bodies {
        data.extend(b);
    }
    let tailn = r.below(40) as usize;
    data.extend(stream_bytes(r, tailn, 0));
    // Corruption.
    let corruption = r.below(100);
    if corruption > 20 {
        feat.push("clean".into());
    } else {
        feat.clear();
    }
    match corruption {
        0..=7 => {
            // Truncate the data.
            let n = r.below(data.len() as u64 + 1) as usize;
            data.truncate(n);
        }
        8..=13 => {
            // Flip a byte inside the per-sample header.
            let hs = u32::from_be_bytes(cmp[28..32].try_into().unwrap()) as usize;
            if hs > 0 && hs <= data.len() {
                let i = r.below(hs as u64) as usize;
                data[i] = r.next() as u8;
            }
        }
        14..=17 => {
            let i = r.range(4, 31) as usize;
            cmp[i] = r.next() as u8;
        }
        18..=20 => {
            let hs = r.below(data.len() as u64 + 1) as u32;
            put_be32(&mut cmp, 28, hs);
        }
        _ => {}
    }
    (cmp, data, feat)
}

fn put_be16(v: &mut [u8], o: usize, x: u16) {
    v[o..o + 2].copy_from_slice(&x.to_be_bytes());
}
fn put_be32(v: &mut [u8], o: usize, x: u32) {
    v[o..o + 4].copy_from_slice(&x.to_be_bytes());
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn header_bytes(h: &CrxHeader) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&h.version.to_le_bytes());
    for x in [h.width, h.height, h.tile_width, h.tile_height, h.mdat_header_size] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&[h.bits, h.planes, h.cfa_layout, h.enc_type, h.levels, h.median_bits]);
    v
}

#[test]
fn synthetic_samples_match_recorded_digest() {
    let mut r = Rng(0x1234_5678_9ABC_DEF1);
    let mut digest = 0xcbf2_9ce4_8422_2325u64;
    let (mut ok, mut failed) = (0, 0);
    for i in 0..3000 {
        let (cmp, data, _) = gen_case(&mut r);
        let h = match CrxHeader::parse(&cmp) {
            Ok(h) => h,
            Err(e) => {
                assert!(matches!(e.kind(), "CorruptFile" | "UnsupportedCompression"), "case {i}: {e}");
                fnv(&mut digest, e.kind().as_bytes());
                continue;
            }
        };
        let mut v = vec![b'H'];
        v.extend(header_bytes(&h));
        fnv(&mut digest, &v);
        let n = h.width as usize * h.height as usize;
        if n > 40_000_000 {
            continue;
        }
        let mut a = vec![SENTINEL; n];
        let res = decode(&h, &data, &mut a, false);
        let mut b = vec![SENTINEL; n];
        let res_par = decode(&h, &data, &mut b, true);
        assert_eq!(res.is_ok(), res_par.is_ok(), "case {i}: the parallel flag changes the outcome");
        assert_eq!(a, b, "case {i}: the parallel flag changes the output");
        match &res {
            Ok(()) => ok += 1,
            Err(e) => {
                failed += 1;
                assert_eq!(e.kind(), "CorruptFile", "case {i}: {e}");
                assert!(a.iter().all(|&x| x == SENTINEL), "case {i}: failed decode wrote to the output");
            }
        }
        fnv(&mut digest, res.as_ref().err().map_or("ok", |e| e.kind()).as_bytes());
        for x in &a {
            fnv(&mut digest, &x.to_le_bytes());
        }
    }
    assert!(ok > 300 && failed > 1000, "ok {ok}, failed {failed}");
    assert_eq!(digest, DIGEST, "decoded output of the synthetic samples changed");
}

const DIGEST: u64 = 0x0e0a0b454a788f1a;

#[test]
fn small_output_buffer_is_an_invalid_argument() {
    let mut r = Rng(7);
    loop {
        let (cmp, data, _) = gen_case(&mut r);
        if let Ok(h) = CrxHeader::parse(&cmp) {
            let n = h.width as usize * h.height as usize;
            if n == 0 || n > 1_000_000 {
                continue;
            }
            let mut out = vec![0u16; n - 1];
            let e = decode(&h, &data, &mut out, false).unwrap_err();
            assert_eq!(e.kind(), "InvalidArgument");
            return;
        }
    }
}
