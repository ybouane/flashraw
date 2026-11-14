//! Self-contained checks of the Phase One IIQ coders: structural properties plus one
//! golden digest over deterministic streams (random, biased, truncated at every byte
//! length, arithmetic wrap cases). The digest was produced by the previous
//! implementation of this module; it pins every output sample and error message.

mod phaseone_support;

use flashraw_codecs::phaseone::{IiqLParams, decode_l_row, decode_s_row, unscramble_pairs};
use flashraw_core::bits::{BitPumpMsb32, BitRead};
use phaseone_support::{Codec, digest};

struct New;
impl Codec for New {
    fn l<B: BitRead>(&self, b: &mut B, out: &mut [u16], shift: u32, low_curve: bool) -> Result<(), String> {
        decode_l_row(b, out, IiqLParams { shift, low_curve }).map_err(|e| e.to_string())
    }
    fn s<B: BitRead>(&self, b: &mut B, out: &mut [u16]) -> Result<(), String> {
        decode_s_row(b, out).map_err(|e| e.to_string())
    }
    fn unscramble(&self, s: &mut [u16], a: u16, b: u16, f: u32) {
        unscramble_pairs(s, a, b, f)
    }
}

const GOLDEN: u64 = 0x99552cf03fbe0ab6;

#[test]
fn golden_digest() {
    let d = digest(&New);
    println!("DIGEST {d:#018x}");
    assert_eq!(d, GOLDEN);
}

#[test]
fn unscramble_twice_with_zero_keys_is_identity() {
    let orig: Vec<u16> = (0..101u32).map(|i| (i.wrapping_mul(2654435761u32) >> 7) as u16).collect();
    for f in [1, 2] {
        let mut v = orig.clone();
        unscramble_pairs(&mut v, 0, 0, f);
        assert_ne!(v, orig);
        unscramble_pairs(&mut v, 0, 0, f);
        assert_eq!(v, orig);
    }
}

#[test]
fn l_literal_row_and_curve() {
    // Eight columns: both length codes `000001 0` style escape (z=5, e=0 -> width 14), then 16-bit literals.
    // Stream (MSB first): 000000 000000 then 8 x 16 bits.
    let vals: [u16; 8] = [0, 5, 255, 256, 1000, 40000, 65535, 3];
    let mut bits = String::from("000000000000");
    for v in vals {
        bits.push_str(&format!("{v:016b}"));
    }
    while bits.len() % 32 != 0 {
        bits.push('0');
    }
    // Words are little-endian, each read MSB first.
    let mut bytes = Vec::new();
    for w in bits.as_bytes().chunks(32) {
        let x = u32::from_str_radix(std::str::from_utf8(w).unwrap(), 2).unwrap();
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    let mut out = [0u16; 8];
    decode_l_row(&mut BitPumpMsb32::new(&bytes), &mut out, IiqLParams { shift: 0, low_curve: false }).unwrap();
    assert_eq!(out, vals);
    decode_l_row(&mut BitPumpMsb32::new(&bytes), &mut out, IiqLParams { shift: 0, low_curve: true }).unwrap();
    assert_eq!((out[0], out[1], out[2], out[3]), (0, 6, 16383, 256));
    decode_l_row(&mut BitPumpMsb32::new(&bytes), &mut out, IiqLParams { shift: 2, low_curve: false }).unwrap();
    assert_eq!(out[4], 4000);
    assert_eq!(out[5], 40000u16.wrapping_shl(2));
}

#[test]
fn l_first_repeat_code_is_an_error() {
    let bytes = [0xffu8; 64];
    let mut out = [0u16; 16];
    let e = decode_l_row(&mut BitPumpMsb32::new(&bytes), &mut out, IiqLParams { shift: 2, low_curve: false });
    assert!(e.unwrap_err().to_string().contains("block without difference width"));
}

#[test]
fn empty_and_short_rows() {
    let mut none: [u16; 0] = [];
    let data = [0u8; 16];
    assert!(decode_l_row(&mut BitPumpMsb32::new(&data), &mut none, IiqLParams { shift: 2, low_curve: false }).is_ok());
    assert!(decode_s_row(&mut BitPumpMsb32::new(&data), &mut none).is_ok());
    let mut out = [1u16; 3];
    decode_s_row(&mut BitPumpMsb32::new(&data), &mut out).unwrap();
    assert_eq!(out, [0, 0, 0]);
    let err = decode_s_row(&mut BitPumpMsb32::new(&[]), &mut out).unwrap_err();
    assert!(err.to_string().contains("IIQ S: truncated row data"));
}
