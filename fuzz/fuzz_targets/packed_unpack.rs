//! Every packing fast path must equal the scalar reference and never over-read.
#![no_main]
use flashraw_codecs::packed::*;
use flashraw_core::PackOrder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let bits = (data[0] % 16) as u32 + 1;
    let order = [PackOrder::MsbBytes, PackOrder::LsbBytes, PackOrder::Msb16, PackOrder::Msb32][(data[1] % 4) as usize];
    let width = (data[2] as usize) * 3 + 1;
    let src = &data[3..];
    let mut a = vec![0u16; width];
    let mut b = vec![0u16; width];
    unpack_row_scalar(src, &mut a, bits, order);
    unpack_row(src, &mut b, bits, order, true);
    assert_eq!(a, b);
    let layout = PackedLayout { width: width.min(64), height: 4, bits, order, row_stride: (data[1] as usize) + 1 };
    let mut dst = vec![0u16; 64 * 4];
    let _ = unpack(src, layout, &mut dst, 64, false, true, data[2] & 1 == 0);
});
