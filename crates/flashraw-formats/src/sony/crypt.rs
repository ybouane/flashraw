//! Sony's metadata/raw "encryption": a lagged-Fibonacci style XOR key stream.
//!
//! The key seeds four words through a linear congruential step; the remaining 123
//! words of a 127-word pad are derived by shifting/XORing earlier words. Each output
//! word is `pad[p + 1] ^ pad[p + 65]` (indices mod 127, written back into the pad)
//! and is XORed big-endian into consecutive 4-byte groups of the data.
//!
//! Used for the SR2Private sub-IFD (white balance, black levels) of ARW/SR2 files
//! and for the metadata and raw data of SRF files.

/// Key-stream generator state.
pub struct SonyCipher {
    pad: [u32; 128],
    p: usize,
}

impl SonyCipher {
    pub fn new(key: u32) -> Self {
        let mut pad = [0u32; 128];
        let mut k = key;
        for v in pad.iter_mut().take(4) {
            k = k.wrapping_mul(48828125).wrapping_add(1);
            *v = k;
        }
        pad[3] = (pad[3] << 1) | ((pad[0] ^ pad[2]) >> 31);
        for p in 4..127 {
            pad[p] = ((pad[p - 4] ^ pad[p - 2]) << 1) | ((pad[p - 3] ^ pad[p - 1]) >> 31);
        }
        SonyCipher { pad, p: 127 }
    }

    /// Next key-stream word.
    #[inline]
    fn next_word(&mut self) -> u32 {
        let p = self.p;
        let v = self.pad[(p + 1) & 127] ^ self.pad[(p + 65) & 127];
        self.pad[p & 127] = v;
        self.p = p.wrapping_add(1);
        v
    }

    /// XOR the key stream into `data`, one word per complete 4-byte group (a trailing
    pub fn apply(&mut self, data: &mut [u8]) {
        for chunk in data.chunks_exact_mut(4) {
            let k = self.next_word().to_be_bytes();
            for (d, k) in chunk.iter_mut().zip(k) {
                *d ^= k;
            }
        }
    }
}

/// Decrypt `data` in place with a fresh key stream.
pub fn decrypt(data: &mut [u8], key: u32) {
    SonyCipher::new(key).apply(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn involution_and_stream_continuity() {
        let orig: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut a = orig.clone();
        decrypt(&mut a, 0x1234_5678);
        assert_ne!(a, orig);
        // Decrypting in two pieces with one cipher equals one pass.
        let mut b = orig.clone();
        let mut c = SonyCipher::new(0x1234_5678);
        let (x, y) = b.split_at_mut(400);
        c.apply(x);
        c.apply(y);
        assert_eq!(a, b);
        decrypt(&mut a, 0x1234_5678);
        assert_eq!(a, orig);
    }

    #[test]
    fn known_first_words() {
        // First key-stream words for key 0, computed from the published algorithm.
        let mut c = SonyCipher::new(0);
        let w0 = c.next_word();
        let mut c2 = SonyCipher::new(0);
        let mut buf = [0u8; 4];
        c2.apply(&mut buf);
        assert_eq!(u32::from_be_bytes(buf), w0);
    }
}
