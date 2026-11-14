#![no_main]
use flashraw_core::bits::BitPumpMsb;
use flashraw_core::huffman::HuffTable;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 17 {
        return;
    }
    let counts = &data[..16];
    let total: usize = counts.iter().map(|&c| c as usize).sum();
    let rest = &data[16..];
    let (vals, stream) = rest.split_at(total.min(rest.len()));
    for diffs in [true, false] {
        if let Ok(mut t) = HuffTable::from_counts(counts, vals, diffs) {
            t.dng_bug16 = data[0] & 1 == 0;
            let mut p = BitPumpMsb::new(stream);
            for _ in 0..512 {
                if diffs {
                    if t.decode_diff(&mut p).is_err() {
                        break;
                    }
                } else if t.decode_symbol(&mut p).is_err() {
                    break;
                }
            }
        }
    }
});
