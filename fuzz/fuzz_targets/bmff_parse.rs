#![no_main]
use flashraw_core::bmff::{boxes, BmffBox};
use libfuzzer_sys::fuzz_target;

fn walk(b: &BmffBox, depth: u32) {
    if depth > 8 {
        return;
    }
    for skip in [0usize, 4, 8] {
        for c in b.children(skip).flatten() {
            walk(&c, depth + 1);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    for b in boxes(data).flatten() {
        walk(&b, 0);
    }
});
