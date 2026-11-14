use super::fixtures::{self, Decoders, Rng, background};
use super::*;

#[test]
fn word_reader_fetches_lazily() {
    let data: Vec<u8> = (0..12).collect();
    let mut b = Bits::at(&data, 0);
    assert_eq!(b.get(8), 0x03);
    assert_eq!(b.p, 4);
    assert_eq!(b.get(24), 0x020100);
    assert_eq!(b.p, 4, "exactly 32 bits consumed: nothing fetched ahead");
    assert_eq!(b.get(1), 0);
    assert_eq!(b.p, 8);
    b.restart(8);
    assert_eq!(b.get(32), 0x0B0A0908);
    assert!(!b.over);
    // A word that is not completely inside the input is all ones.
    let mut b = Bits::at(&data[..10], 8);
    assert_eq!(b.get(32), u32::MAX);
    assert!(b.over);
}

#[test]
fn v1_code_tiles_the_window() {
    let sum: usize = V1_CODE.iter().map(|&(len, _)| 1usize << (10 - len)).sum();
    assert_eq!(sum, 1024);
}

#[test]
fn v0_zero_stream_is_constant_prediction() {
    // Every block: direction 0, no length change, lengths 4: residuals read as zeros, so the
    // first block of a row is 128 and later blocks repeat their left neighbour.
    let (w, h) = (48usize, 6usize);
    let src = vec![0u8; 4096];
    let offs = vec![0u32; h];
    let mut out = vec![9u16; w * h];
    decode_v0_with(&src, &offs, w, h, &mut out, false).unwrap();
    assert!(out.iter().all(|&v| v == 128), "{out:?}");
}

/// Argument errors and degenerate sizes.
#[test]
fn v0_arguments() {
    let src = [0u8; 16];
    let mut out = [7u16; 8];
    assert!(matches!(decode_v0_with(&src, &[0; 2], 4, 2, &mut out[..7], false), Err(Error::InvalidArgument(_))));
    assert!(matches!(decode_v0_with(&src, &[0; 1], 4, 2, &mut out, false), Err(Error::InvalidArgument(_))));
    assert!(decode_v0_with(&src, &[], 0, 0, &mut [], true).is_ok());
    assert_eq!(out, [7; 8]);
}

/// The two-pass decoder (rows >= 2 reconstructed in place) equals the flat-buffer model run
/// over every row, for any geometry, offsets and data.
#[test]
fn v0_matches_flat_model() {
    let mut r = Rng(42);
    for it in 0..1500 {
        let w = match it % 4 {
            0 => 1 + r.below(15),
            1 => 16 + r.below(60),
            2 => 16 * (1 + r.below(8)),
            _ => 530 + r.below(40),
        };
        let h = 1 + r.below(if w > 500 { 30 } else { 10 });
        let len = 16 + r.below(w * h * 2);
        let bias = r.below(4);
        let src = r.bytes(len, bias);
        let offs: Vec<u32> = (0..h).map(|_| r.below(len + 16) as u32).collect();
        let mut want = background(w * h, it as u64);
        for row in 0..h {
            flat_row(&src, offs[row] as usize, row, w, &mut want);
        }
        transpose_pairs(&mut want, w, false);
        for par in [false, true] {
            let mut got = background(w * h, it as u64);
            decode_v0_with(&src, &offs, w, h, &mut got, par).unwrap();
            assert!(got == want, "{w}x{h} par={par}");
        }
    }
}

#[test]
fn v2_rejects_small_and_garbage() {
    let mut out = vec![0u16; 646 * 436];
    assert!(matches!(decode_v2(&[0; 64], 15, 436, &mut out), Err(Error::CorruptFile { .. })));
    assert!(matches!(decode_v2(&[0; 64], 646, 0, &mut out), Err(Error::CorruptFile { .. })));
    assert!(matches!(decode_v2(&[0; 8], 646, 436, &mut out[..10]), Err(Error::InvalidArgument(_))));
    for fill in [0u8, 0xFF, 0x5A] {
        assert!(decode_v2(&vec![fill; 100_000], 646, 436, &mut out).is_err());
    }
    assert!(decode_v2(&[], 646, 436, &mut out).is_err());
}

#[test]
fn v2_generated_stream_decodes_and_half_of_it_does_not() {
    let mut r = Rng(7);
    let s = fixtures::gen_v2(&mut r, 646, 436, 14, 5, 128, false);
    let mut out = vec![0u16; 646 * 436];
    decode_v2(&s, 646, 436, &mut out).unwrap();
    assert!(out.iter().all(|&v| v < 1 << 14));
    assert!(decode_v2(&s[..s.len() / 2], 646, 436, &mut out).is_err());
}

#[test]
fn golden_digests() {
    let d = Decoders { v0: decode_v0_with, v1: decode_v1, v2: decode_v2 };
    let got = fixtures::digests(&d);
    if std::env::var_os("SAMSUNG_PRINT_DIGESTS").is_some() {
        for l in &got {
            println!("{l}");
        }
    }
    assert_eq!(got.len(), fixtures::GOLDEN.len());
    for (g, w) in got.iter().zip(fixtures::GOLDEN) {
        assert_eq!(g, w);
    }
}
