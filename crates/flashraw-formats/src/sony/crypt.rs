//! Sony's key-stream XOR "encryption" of SR2Private and SRF data.
//!
//! The key stream is a lagged generator over 32-bit words: from a 127-word seed state `x`,
//! every further word is `x[i] = x[i - 127] ^ x[i - 63]`, and stream word `n` is `x[127 + n]`.
//! The seed comes from a linear congruential sequence (multiplier 5^11, increment 1) of the
//! key, with a shift recurrence filling the rest of the state. Data is XORed in complete
//! 4-byte groups with the stream words written big-endian; a trailing partial group is left
//! alone. Because a word depends only on words at least 63 positions back, 63 words can be
//! produced at a time without a dependency between them.

/// Words of generator state.
const STATE: usize = 127;
/// Short feedback distance, also the number of words produced per block.
const BLOCK: usize = 63;
/// Long feedback distance minus the short one: `x[i] = x[i - 127] ^ x[i - 63]`.
const SHORT_LAG: usize = STATE - BLOCK;

/// The first 127 words (the seed state) for `key`.
fn seed_state(key: u32) -> [u32; STATE] {
    let mut x = [0u32; STATE];
    let mut k = key;
    for w in x.iter_mut().take(4) {
        k = k.wrapping_mul(48_828_125).wrapping_add(1);
        *w = k;
    }
    x[3] = (x[3] << 1) | ((x[0] ^ x[2]) >> 31);
    for p in 4..STATE {
        x[p] = ((x[p - 4] ^ x[p - 2]) << 1) | ((x[p - 3] ^ x[p - 1]) >> 31);
    }
    x
}

/// Key-stream generator; consecutive `apply` calls continue one stream.
pub struct SonyCipher {
    /// The last 127 generated words (the seed before the first block).
    state: [u32; STATE],
    /// The current block of stream words and how many are used.
    block: [u32; BLOCK],
    used: usize,
}

impl SonyCipher {
    /// A generator for `key`, positioned at the first stream word.
    pub fn new(key: u32) -> Self {
        SonyCipher { state: seed_state(key), block: [0; BLOCK], used: BLOCK }
    }

    fn refill(&mut self) {
        for t in 0..BLOCK {
            self.block[t] = self.state[t] ^ self.state[t + SHORT_LAG];
        }
        self.state.copy_within(BLOCK.., 0);
        self.state[STATE - BLOCK..].copy_from_slice(&self.block);
        self.used = 0;
    }

    /// XOR the stream into `data`, one word per complete 4-byte group (a trailing partial group
    /// is left untouched).
    pub fn apply(&mut self, data: &mut [u8]) {
        let mut groups = data.chunks_exact_mut(4);
        while groups.len() > 0 {
            if self.used == BLOCK {
                self.refill();
            }
            let take = groups.len().min(BLOCK - self.used);
            let words = &self.block[self.used..self.used + take];
            for (g, &z) in groups.by_ref().take(take).zip(words) {
                let v = u32::from_be_bytes([g[0], g[1], g[2], g[3]]) ^ z;
                g.copy_from_slice(&v.to_be_bytes());
            }
            self.used += take;
        }
    }
}

/// The first `n` key-stream words for `key`.
pub fn keystream(key: u32, n: usize) -> Vec<u32> {
    let mut out = vec![0u32; n];
    let x = seed_state(key);
    // Words 0..127 mix the seed state and the first words already produced.
    let head = n.min(STATE);
    for m in 0..head {
        let a = x[m];
        let b = if m + 64 < STATE { x[m + 64] } else { out[m + 64 - STATE] };
        out[m] = a ^ b;
    }
    let mut m = STATE;
    while m < n {
        let len = BLOCK.min(n - m);
        let (done, rest) = out.split_at_mut(m);
        let far = &done[m - STATE..m - STATE + len];
        let near = &done[m - BLOCK..m - BLOCK + len];
        for ((o, &a), &b) in rest[..len].iter_mut().zip(far).zip(near) {
            *o = a ^ b;
        }
        m += len;
    }
    out
}

/// Decrypt (or encrypt: the operation is its own inverse) `data` in place with a fresh stream.
pub fn decrypt(data: &mut [u8], key: u32) {
    SonyCipher::new(key).apply(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn involution() {
        let orig: Vec<u8> = (0..1003u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut d = orig.clone();
        decrypt(&mut d, 0xdead_beef);
        assert_ne!(d, orig);
        decrypt(&mut d, 0xdead_beef);
        assert_eq!(d, orig);
    }

    #[test]
    fn split_equals_whole() {
        let orig: Vec<u8> = (0..4000u32).map(|i| (i * 31 + 5) as u8).collect();
        for key in [0u32, 1, 0x1234_5678, u32::MAX] {
            let mut whole = orig.clone();
            decrypt(&mut whole, key);
            for cut in [0usize, 4, 8, 252, 256, 1000, 3996, 4000] {
                let mut p = orig.clone();
                let mut c = SonyCipher::new(key);
                let (a, b) = p.split_at_mut(cut);
                c.apply(a);
                c.apply(b);
                assert_eq!(p, whole, "key {key} cut {cut}");
            }
        }
    }

    #[test]
    fn bulk_equals_streaming() {
        for key in [0u32, 7, 0xffff_ffff] {
            for n in [0usize, 1, 62, 63, 64, 126, 127, 128, 200, 1000] {
                let ks = keystream(key, n);
                assert_eq!(ks.len(), n);
                let mut zeros = vec![0u8; n * 4 + 3];
                decrypt(&mut zeros, key);
                for (i, w) in ks.iter().enumerate() {
                    assert_eq!(zeros[4 * i..4 * i + 4], w.to_be_bytes(), "key {key} n {n} word {i}");
                }
                assert_eq!(&zeros[n * 4..], &[0, 0, 0]);
            }
        }
    }

    #[test]
    fn recurrence_holds() {
        let ks = keystream(42, 500);
        for m in 127..500 {
            assert_eq!(ks[m], ks[m - 127] ^ ks[m - 63]);
        }
    }
}
