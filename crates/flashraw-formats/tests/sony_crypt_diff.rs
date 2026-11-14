//! Sony key-stream cipher: key streams and decrypted buffers over many keys and lengths are pinned
//! by a hash computed once against the pre-rewrite implementation; split application must equal
//! one call.

use flashraw_formats::sony::crypt as new;

#[test]
fn streams_match() {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut fnv = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    };
    let mut keys = vec![0u32, 1, 0xffff_ffff, 0x8000_0000, 0xdead_beef, 0x1234_5678];
    for _ in 0..20 {
        keys.push(next() as u32);
    }
    for key in keys {
        for n in [0usize, 1, 3, 4, 63, 64, 126, 127, 128, 129, 190, 254, 1000, 70_000] {
            let ks = new::keystream(key, n);
            assert_eq!(ks.len(), n, "key {key:#x} n {n}");
            ks.iter().for_each(|w| fnv(&w.to_be_bytes()));
        }
        for len in [0usize, 1, 3, 4, 5, 7, 8, 100, 1023, 4096] {
            let data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let mut b = data.clone();
            new::decrypt(&mut b, key);
            fnv(&b);
            // split application continues one stream
            let mut c = data.clone();
            let mut cipher = new::SonyCipher::new(key);
            let cut = (len / 8) * 4;
            let (p, q) = c.split_at_mut(cut);
            cipher.apply(p);
            cipher.apply(q);
            assert_eq!(b, c, "split key {key:#x} len {len}");
        }
    }
    assert_eq!(h, 0xc3de_5c81_6848_e3a0);
}
