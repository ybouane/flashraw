//! Minimal lossless JPEG encoder for synthetic fixtures (tests, fuzz seeds, malformed-input suites). Not optimised;
//! it is test support, not part of the decoder.

/// Encode `samples` (`height` rows of `width * comps` interleaved values) as a
/// lossless JPEG with predictor `psv` (1..=7) and a fixed Huffman table.
pub fn encode(width: usize, height: usize, comps: usize, bits: u32, psv: u32, samples: &[u16]) -> Vec<u8> {
    encode_with_restarts(width, height, comps, bits, psv, samples, 0)
}

/// [`encode`] with a restart interval of `restart` MCUs (0: none). Intervals must
/// start at row starts; each one resets the first-column predictors, and other
/// predictors keep reading the row above (as the decoder does).
pub fn encode_with_restarts(
    width: usize,
    height: usize,
    comps: usize,
    bits: u32,
    psv: u32,
    samples: &[u16],
    restart: usize,
) -> Vec<u8> {
    // Huffman table: lengths 0..=16 with simple code lengths.
    let counts: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0];
    let values: Vec<u8> = (0..=16).collect();
    let mut codes = [(0u32, 0u32); 17];
    let mut code = 0u32;
    let mut k = 0;
    for len in 1..=16u32 {
        for _ in 0..counts[len as usize - 1] {
            codes[values[k] as usize] = (code, len);
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    let mut out = vec![0xFF, 0xD8];
    // SOF3
    out.extend([0xFF, 0xC3]);
    out.extend(((8 + 3 * comps) as u16).to_be_bytes());
    out.push(bits as u8);
    out.extend((height as u16).to_be_bytes());
    out.extend((width as u16).to_be_bytes());
    out.push(comps as u8);
    for c in 0..comps {
        out.extend([c as u8 + 1, 0x11, 0]);
    }
    // DHT
    out.extend([0xFF, 0xC4]);
    out.extend(((2 + 17 + values.len()) as u16).to_be_bytes());
    out.push(0x00);
    out.extend(counts);
    out.extend(&values);
    if restart > 0 {
        out.extend([0xFF, 0xDD, 0, 4]);
        out.extend((restart as u16).to_be_bytes());
    }
    // SOS
    out.extend([0xFF, 0xDA]);
    out.extend(((6 + 2 * comps) as u16).to_be_bytes());
    out.push(comps as u8);
    for c in 0..comps {
        out.extend([c as u8 + 1, 0x00]);
    }
    out.extend([psv as u8, 0, 0]);
    // Entropy data
    // Bit accumulator and its fill.
    let mut st = (0u64, 0u32);
    let mut ent = vec![];
    let put = |v: u32, len: u32, ent: &mut Vec<u8>, (acc, n): &mut (u64, u32)| {
        for i in (0..len).rev() {
            *acc = (*acc << 1) | ((v >> i) & 1) as u64;
            *n += 1;
            if *n == 8 {
                ent.push(*acc as u8);
                if *acc as u8 == 0xFF {
                    ent.push(0);
                }
                *acc = 0;
                *n = 0;
            }
        }
    };
    let rl = width * comps;
    let s = |r: usize, i: usize| samples[r * rl + i] as i32;
    let mut markers = 0u8;
    for r in 0..height {
        let restarted = restart > 0 && (r * width) % restart == 0;
        if restarted && r > 0 {
            // Pad the partial byte with 1-bits and emit RSTn.
            if st.1 > 0 {
                put(0x7F, 8 - st.1, &mut ent, &mut st);
            }
            ent.extend([0xFF, 0xD0 + markers % 8]);
            markers += 1;
        }
        for col in 0..width {
            for c in 0..comps {
                let i = col * comps + c;
                let pred = if (r == 0 || restarted) && col == 0 {
                    1 << (bits - 1)
                } else if r == 0 {
                    s(r, i - comps)
                } else if col == 0 {
                    s(r - 1, i)
                } else {
                    let (ra, rb, rc) = (s(r, i - comps), s(r - 1, i), s(r - 1, i - comps));
                    match psv {
                        1 => ra,
                        2 => rb,
                        3 => rc,
                        4 => ra + rb - rc,
                        5 => ra + ((rb - rc) >> 1),
                        6 => rb + ((ra - rc) >> 1),
                        _ => (ra + rb) >> 1,
                    }
                };
                let mut diff = (s(r, i) - pred) as i16 as i32;
                if diff == -32768 {
                    let (c16, l16) = codes[16];
                    put(c16, l16, &mut ent, &mut st);
                    continue;
                }
                let len = if diff == 0 { 0 } else { 32 - diff.unsigned_abs().leading_zeros() };
                let (cd, l) = codes[len as usize];
                put(cd, l, &mut ent, &mut st);
                if len > 0 {
                    if diff < 0 {
                        diff -= 1;
                    }
                    put(diff as u32 & ((1 << len) - 1), len, &mut ent, &mut st);
                }
            }
        }
    }
    // Pad the final partial byte with 1-bits (JPEG convention).
    put(0x7F, 7, &mut ent, &mut st);
    out.extend(ent);
    out.extend([0xFF, 0xD9]);
    out
}
