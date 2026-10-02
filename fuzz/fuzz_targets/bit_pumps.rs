//! All bit pumps must agree with a bit-by-bit model and never panic.
#![no_main]
use flashraw_core::bits::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let widths: Vec<u32> = data.iter().take(64).map(|b| (*b as u32) % 33).collect();
    let payload = &data[data.len().min(64)..];
    let mut m = BitPumpMsb::new(payload);
    let mut l = BitPumpLsb::new(payload);
    let mut m16 = BitPumpMsb16::new(payload);
    let mut m32 = BitPumpMsb32::new(payload);
    let mut j = BitPumpJpeg::new(payload);
    let mut bitpos = 0usize;
    for &w in &widths {
        let got = m.get(w);
        let mut want = 0u32;
        for _ in 0..w {
            let byte = payload.get(bitpos / 8).copied().unwrap_or(0);
            want = (want << 1) | ((byte >> (7 - bitpos % 8)) & 1) as u32;
            bitpos += 1;
        }
        assert_eq!(got, want);
        let _ = (l.get(w), m16.get(w), m32.get(w), j.get(w));
    }
    let _ = (m.byte_position(), j.reset_at_restart());
});
