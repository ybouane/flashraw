//! Region and tile decode: arbitrary rectangles (including empty, out-of-range and
//! overflowing ones) and tile indices. A region must equal the same window of the full
//! decode when both succeed cleanly. Rectangle and tile come from the last 10 input bytes.
#![no_main]
use flashraw::{CorruptionPolicy, DecodeOptions, FlashRaw, Limits, OutputFormat, Rect};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((file, c)) = data.split_last_chunk::<10>().map(|(f, c)| (f, *c)) else { return };
    let limits = Limits::default().with_max_samples(8 << 20).with_max_dimension(8192);
    let Ok(raw) = FlashRaw::open_with(file, limits) else { return };
    let opts = |output| DecodeOptions {
        limits,
        output,
        threads: Some(1 + (c[9] & 1) as usize),
        apply_opcodes: false,
        corruption_policy: CorruptionPolicy::Strict,
        ..Default::default()
    };
    let word = |i: usize| u16::from_le_bytes([c[i], c[i + 1]]) as u32;
    // Mostly small coordinates, sometimes huge ones.
    let scale = |v: u32, big: bool| if big { v.wrapping_mul(0x0001_0001) } else { v };
    let rect = Rect::new(
        scale(word(0), c[8] & 1 != 0),
        scale(word(2), c[8] & 2 != 0),
        scale(word(4), c[8] & 4 != 0),
        scale(word(6), c[8] & 8 != 0),
    );
    let _ = raw.decode_region(rect, opts(OutputFormat::Rgba8));
    let units = raw.units();
    if !units.is_empty() {
        let _ = raw.decode_tile(word(0) as usize % (units.len() + 1), opts(OutputFormat::RawU16));
    }
    let _ = raw.decode_tile(usize::MAX, opts(OutputFormat::RawU16));

    let info = raw.frame_info();
    let (fw, fh) = (info.width, info.height);
    let (Ok(full), Ok(part)) =
        (raw.decode(opts(OutputFormat::RawU16)), raw.decode_region(rect, opts(OutputFormat::RawU16)))
    else {
        return;
    };
    let (Some(full_px), Some(part_px)) = (full.as_u16(), part.as_u16()) else { return };
    if (full.width(), full.height()) != (fw, fh) || !part.warnings().is_empty() || !full.warnings().is_empty() {
        return;
    }
    let (w, h) = (part.width() as usize, part.height() as usize);
    let (fwu, fhu) = (fw as usize, fh as usize);
    if w == 0 || h == 0 || full_px.len() % (fwu * fhu) != 0 {
        return;
    }
    let cpp = full_px.len() / (fwu * fhu);
    assert_eq!(part_px.len(), w * h * cpp, "region sample count");
    // The returned window sits where the (clamped) request asked.
    let (x0, y0) = (rect.x as usize, rect.y as usize);
    if x0 + w <= fwu && y0 + h <= fhu {
        for y in 0..h {
            let a = &full_px[((y0 + y) * fwu + x0) * cpp..((y0 + y) * fwu + x0 + w) * cpp];
            let b = &part_px[y * w * cpp..(y + 1) * w * cpp];
            assert_eq!(a, b, "region row {y} differs from the full decode");
        }
    }
});
