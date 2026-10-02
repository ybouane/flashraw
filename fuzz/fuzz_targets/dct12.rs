#![no_main]
use flashraw_codecs::dct12::Dct12;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(d) = Dct12::parse(data, true) {
        if (d.width as u64) * (d.height as u64) <= 1 << 22 {
            let _ = d.decode_blocks(data, |_, _, _| {});
        }
    }
});
