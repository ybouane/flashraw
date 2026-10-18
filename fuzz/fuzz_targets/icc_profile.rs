#![no_main]
//! Untrusted ICC output profiles: any bytes handed to `DecodeOptions::output_profile` must
//! come back as `Ok` or an `Error`, never a panic, hang or runaway allocation.
use flashraw::{Backend, DecodeOptions, FlashRaw, OutputFormat, OutputProfile, RenderingIntent};
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

/// A 16x8 16-bit uncompressed DNG (one RGGB strip).
fn dng() -> Vec<u8> {
    let e: [(u16, u16, u32, u32); 12] = [
        (0x0100, 4, 1, 16),
        (0x0101, 4, 1, 8),
        (0x0102, 3, 1, 16),
        (0x0103, 3, 1, 1),
        (0x0106, 3, 1, 32803),
        (0x0111, 4, 1, 8 + 2 + 12 * 12 + 4),
        (0x0115, 3, 1, 1),
        (0x0116, 4, 1, 8),
        (0x0117, 4, 1, 256),
        (0x828D, 3, 2, 2 | (2 << 16)),
        (0x828E, 1, 4, u32::from_le_bytes([0, 1, 1, 2])),
        (0xC612, 1, 4, u32::from_le_bytes([1, 4, 0, 0])),
    ];
    let mut out = b"II*\0\x08\0\0\0".to_vec();
    out.extend((e.len() as u16).to_le_bytes());
    for (tag, typ, count, v) in e {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(v.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes());
    out.extend((0..128u16).flat_map(|i| (i * 31 + 1).to_le_bytes()));
    out
}

fuzz_target!(|data: &[u8]| {
    static RAW: OnceLock<FlashRaw<'static>> = OnceLock::new();
    let raw = RAW.get_or_init(|| FlashRaw::open(dng()).expect("synthetic DNG"));
    // The first byte picks the intent, black point compensation and output; the rest is the profile.
    let Some((&sel, icc)) = data.split_first() else { return };
    let intent = [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
        RenderingIntent::AbsoluteColorimetric,
    ][(sel & 3) as usize];
    let output = if sel & 8 == 0 { OutputFormat::Rgba8 } else { OutputFormat::Rgb16 };
    let opts = DecodeOptions {
        backend: Backend::Cpu,
        threads: Some(1),
        output,
        output_profile: Some(
            OutputProfile::new(icc.to_vec()).with_intent(intent).with_black_point_compensation(sel & 4 != 0),
        ),
        ..Default::default()
    };
    let _ = raw.decode(opts);
});
