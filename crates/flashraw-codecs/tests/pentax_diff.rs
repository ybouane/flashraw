//! Tests of the Pentax table builder and wrapper: random complete tables in tag form decode random
//! streams to results pinned by a hash (computed once against the pre-rewrite implementation),
//! the default table equals the K-r table, hostile tags only must not panic.

use flashraw_codecs::nikon_huffman;
use flashraw_core::bytes::ByteOrder;

use flashraw_codecs::pentax_huffman as new;

fn fnv(h: &mut u64, out: &[u16], dead: Option<usize>) {
    for v in out.iter().copied().chain(dead.map(|d| d as u16 + 1)) {
        for b in v.to_le_bytes() {
            *h = (*h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    }
}

struct Rng(u64);
impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.step() % n as u64) as usize
    }
}

/// Serialise a tag 0x0220 value: n word, 12 padding bytes, codes (12-bit left aligned), lengths.
fn tag(codes: &[(u16, u8)], order: ByteOrder, pad: u8) -> Vec<u8> {
    let n = (codes.len() + 4) & 15; // dep = (n + 12) & 15
    let put = |v: u16| if order == ByteOrder::Big { v.to_be_bytes() } else { v.to_le_bytes() };
    let mut t = put(n as u16).to_vec();
    t.extend(std::iter::repeat_n(pad, 12));
    for &(c, _) in codes {
        t.extend(put(c));
    }
    t.extend(codes.iter().map(|c| c.1));
    t
}

/// A random complete prefix code with 12..=15 entries and lengths <= 12, entries in random order.
fn random_table(r: &mut Rng) -> Vec<(u16, u8)> {
    {
        let want = 12 + r.below(4);
        let mut leaves: Vec<(u32, u32)> = vec![(0, 0)]; // (code, len) of the open prefixes
        while leaves.len() < want {
            let i = r.below(leaves.len());
            let (c, l) = leaves[i];
            if l >= 12 {
                continue;
            }
            leaves[i] = (c << 1, l + 1);
            leaves.push((c << 1 | 1, l + 1));
        }
        let mut t: Vec<(u16, u8)> = leaves.iter().map(|&(c, l)| ((c << (12 - l)) as u16, l as u8)).collect();
        for i in (1..t.len()).rev() {
            t.swap(i, r.below(i + 1));
        }
        t
    }
}

fn streams(r: &mut Rng) -> Vec<u8> {
    let n = 200 + r.below(3000);
    (0..n).map(|_| r.step() as u8).collect()
}

#[test]
fn tables_decode_alike() {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut r = Rng(0xfeed_beef_1234_5678);
    for case in 0..300 {
        let order = if r.below(2) == 0 { ByteOrder::Big } else { ByteOrder::Little };
        let codes = random_table(&mut r);
        let meta = tag(&codes, order, r.below(256) as u8);
        let b = new::table_from_makernote(&meta, order).unwrap();
        let (w, h) = (2 * (1 + r.below(40)), 1 + r.below(30));
        let src = streams(&mut r);
        let mut ob = vec![0u16; w * h];
        let rb = new::decode(&src, &b, w, h, &mut ob, w, false, 0).unwrap();
        fnv(&mut hash, &ob, rb);
        let mut oc = vec![0u16; w * h];
        let rc = new::decode(&src, &b, w, h, &mut oc, w, true, src.len()).unwrap();
        assert_eq!((rb, &ob), (rc, &oc), "case {case} parallel");
    }
    assert_eq!(hash, 0xb0c4_43cb_6597_b15d);
}

#[test]
fn default_equals_k_r_table() {
    let mut r = Rng(5);
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let codes: Vec<(u16, u8)> =
        [0xf00u16, 0xc00, 0x800, 0x000, 0x400, 0xa00, 0xe00, 0xf80, 0xfc0, 0xfe0, 0xff0, 0xff8, 0xffc]
            .into_iter()
            .zip([5u8, 3, 3, 2, 2, 3, 4, 6, 7, 8, 9, 10, 10])
            .collect();
    let meta = tag(&codes, ByteOrder::Big, 0);
    let a = new::table_from_makernote(&meta, ByteOrder::Big).unwrap();
    let b = new::default_table().unwrap();
    for _ in 0..50 {
        let (w, h) = (64, 40);
        let src = streams(&mut r);
        let mut oa = vec![0u16; w * h];
        let mut ob = vec![0u16; w * h];
        let ra = new::decode(&src, &a, w, h, &mut oa, w, false, 0).unwrap();
        let rb = new::decode(&src, &b, w, h, &mut ob, w, false, 0).unwrap();
        assert_eq!((ra, &oa), (rb, &ob));
        fnv(&mut hash, &ob, rb);
    }
    assert_eq!(hash, 0xdede_a445_234b_e47d);
}

#[test]
fn truncated_or_hostile_tags_do_not_panic() {
    let mut r = Rng(11);
    for _ in 0..500 {
        let n = r.below(80);
        let meta: Vec<u8> = (0..n).map(|_| r.step() as u8).collect();
        for order in [ByteOrder::Big, ByteOrder::Little] {
            if let Ok(t) = new::table_from_makernote(&meta, order) {
                let src = streams(&mut r);
                let mut out = vec![0u16; 16 * 8];
                let _ = new::decode(&src, &t, 16, 8, &mut out, 16, false, 0);
            }
        }
    }
    let _ = nikon_huffman::DirectHuff::from_counts(&[0; 3]);
}
