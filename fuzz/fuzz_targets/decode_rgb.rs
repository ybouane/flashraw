//! RGB develop path: demosaic, colour matrices, colour spaces, crops, orientation, exposure
//! and the output conversions, with the output format, backend, demosaic and colour space
//! chosen from the last four input bytes (the rest is the file, so its magic survives
//! mutation). A successful frame must have exactly the sample count its geometry implies.
#![no_main]
use flashraw::{Backend, ColorSpace, DecodeOptions, Demosaic, FlashRaw, Limits, OutputFormat};
use libfuzzer_sys::fuzz_target;

const OUTPUTS: [OutputFormat; 6] = [
    OutputFormat::Rgba8,
    OutputFormat::Rgb16,
    OutputFormat::LinearRgbF32,
    OutputFormat::LinearRgb16,
    OutputFormat::LinearRgbF16,
    OutputFormat::NormalizedF32,
];
const SPACES: [ColorSpace; 5] =
    [ColorSpace::Srgb, ColorSpace::DisplayP3, ColorSpace::AdobeRgb, ColorSpace::ProPhoto, ColorSpace::Rec2020];

fuzz_target!(|data: &[u8]| {
    let Some((file, c)) = data.split_last_chunk::<4>().map(|(f, c)| (f, *c)) else { return };
    let limits = Limits::default().with_max_samples(8 << 20).with_max_dimension(8192);
    let Ok(raw) = FlashRaw::open_with(file, limits) else { return };
    let output = OUTPUTS[c[0] as usize % OUTPUTS.len()];
    let opts = DecodeOptions {
        limits,
        output,
        backend: if c[1] & 1 == 0 { Backend::Cpu } else { Backend::CpuSimd },
        demosaic: if c[1] & 2 == 0 { Demosaic::Bilinear } else { Demosaic::HalfSize },
        crop: c[1] & 4 != 0,
        apply_opcodes: c[1] & 8 != 0,
        preview_fallback: c[1] & 16 != 0,
        color_space: SPACES[c[2] as usize % SPACES.len()],
        exposure: [0.0, 1.0, -3.0, 8.0][(c[2] >> 4) as usize & 3],
        threads: Some(1 + (c[3] & 1) as usize),
        ..Default::default()
    };
    if let Ok(f) = raw.decode(opts) {
        let channels = match output {
            OutputFormat::Rgba8 => 4,
            OutputFormat::NormalizedF32 => f.data.len() / (f.width() as usize * f.height() as usize).max(1),
            _ => 3,
        };
        assert_eq!(f.data.len(), f.width() as usize * f.height() as usize * channels, "{output:?}");
    }
});
