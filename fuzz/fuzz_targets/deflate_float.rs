#![no_main]
use flashraw_codecs::deflate::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = inflate_zlib(data, 1 << 16);
    if data.len() > 8 {
        let mut row = data.to_vec();
        let mut tmp = Vec::new();
        let bps = (data[0] % 3 + 2) as usize;
        let n = row.len() / bps;
        undo_float_predictor(&mut row, &mut tmp, n, bps, (data[1] % 4 + 1) as usize);
        let mut out = vec![0f32; n];
        float_bytes_to_f32(&row, bps, &mut out);
        let mut u: Vec<u16> = data.iter().map(|&b| b as u16).collect();
        undo_horizontal_u16(&mut u, (data[2] % 4 + 1) as usize, (data[3] % 4 + 1) as usize);
    }
});
