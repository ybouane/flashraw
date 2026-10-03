//! Kernel sources, the uniform parameter block, and host-side table builders.
//!
//! The tables reproduce the CPU reference (`flashraw::process`) exactly: the same
//! calibration pattern size, the same per-position black/scale values, the same
//! 3x3 same-colour neighbour sets and the same sRGB quantisation table.

use bytemuck::{Pod, Zeroable};
use flashraw_core::{FrameInfo, PackOrder};

/// `common.wgsl` + kernel file (WGSL has no `#include`).
pub(crate) const UNPACK_WGSL: &str =
    concat!(include_str!("../../../shaders/common.wgsl"), "\n", include_str!("../../../shaders/unpack.wgsl"));
pub(crate) const NORMALIZE_WGSL: &str =
    concat!(include_str!("../../../shaders/common.wgsl"), "\n", include_str!("../../../shaders/normalize.wgsl"));
pub(crate) const DEVELOP_WGSL: &str =
    concat!(include_str!("../../../shaders/common.wgsl"), "\n", include_str!("../../../shaders/develop.wgsl"));
pub(crate) const TO_TEXTURE_WGSL: &str = include_str!("../../../shaders/to_texture.wgsl");

/// WGSL sources of every kernel module, for embedding in other runtimes (e.g. a
/// hand-written WebGPU pipeline in JavaScript). Each entry is `(name, source)`.
pub fn wgsl_sources() -> [(&'static str, &'static str); 4] {
    [("unpack", UNPACK_WGSL), ("normalize", NORMALIZE_WGSL), ("develop", DEVELOP_WGSL), ("to_texture", TO_TEXTURE_WGSL)]
}

/// Mirrors `struct Params` in `shaders/common.wgsl` (std140-compatible: 40 u32 then 4 vec4).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable, Default)]
pub(crate) struct Params {
    pub width: u32,
    pub height: u32,
    pub cpp: u32,
    pub bits: u32,
    pub src_stride: u32,
    pub src_offset: u32,
    pub src_len: u32,
    pub row_bytes: u32,
    pub src_x: u32,
    pub src_y: u32,
    pub origin_x: u32,
    pub origin_y: u32,
    pub pat_w: u32,
    pub pat_h: u32,
    pub cfa_w: u32,
    pub cfa_h: u32,
    pub n_rowd: u32,
    pub n_cold: u32,
    pub off_scale: u32,
    pub off_rowd: u32,
    pub off_cold: u32,
    pub lut_len: u32,
    pub off_srgb: u32,
    pub off_cfa: u32,
    pub crop_x: u32,
    pub crop_y: u32,
    pub dev_w: u32,
    pub dev_h: u32,
    pub out_w: u32,
    pub out_h: u32,
    pub orientation: u32,
    pub total: u32,
    pub band_y: u32,
    pub band_end: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    pub dst_stride: u32,
    pub dst_off: u32,
    pub pad0: u32,
    pub pad1: u32,
    pub wb: [f32; 4],
    pub m0: [f32; 4],
    pub m1: [f32; 4],
    pub m2: [f32; 4],
}

pub(crate) fn order_index(o: PackOrder) -> u32 {
    match o {
        PackOrder::MsbBytes => 0,
        PackOrder::LsbBytes => 1,
        PackOrder::Msb16 => 2,
        PackOrder::Msb32 => 3,
    }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}
fn lcm(a: usize, b: usize) -> usize {
    a / gcd(a, b) * b
}

/// Calibration table (`tab` binding) and the matching `Params` fields.
pub(crate) struct CalibTable {
    pub tab: Vec<f32>,
    pub pat_w: u32,
    pub pat_h: u32,
    pub off_scale: u32,
    pub off_rowd: u32,
    pub off_cold: u32,
    pub n_rowd: u32,
    pub n_cold: u32,
}

/// Per-position black and `1 / (white - black)` for each component, laid out as
/// `black[c][pat]`, `scale[c][pat]`, then row and column deltas. Same pattern
/// size and values as the CPU `Calib::new`.
pub(crate) fn calib_table(info: &FrameInfo, cpp: usize) -> CalibTable {
    let (cw, ch) = info.cfa.map(|p| (p.width(), p.height())).unwrap_or((1, 1));
    let b = &info.black_level;
    let (rows, cols, samples) = (b.rows.max(1) as usize, b.cols.max(1) as usize, b.samples.max(1) as usize);
    let pw = lcm(cw, cols).min(64);
    let ph = lcm(ch, rows).min(64);
    let cpp = cpp.max(1);
    let mut black = Vec::with_capacity(pw * ph * cpp);
    let mut scale = Vec::with_capacity(pw * ph * cpp);
    for c in 0..cpp {
        for y in 0..ph {
            for x in 0..pw {
                let bl = b.values.get(((y % rows) * cols + x % cols) * samples + c % samples).copied().unwrap_or(0.0);
                let color = info.cfa.map(|p| p.color_at(x, y) as usize).unwrap_or(c);
                let wl = &info.white_level;
                let white = wl.at(if wl.values.len() > 1 { color.min(wl.values.len() - 1) } else { 0 }) as f32;
                black.push(bl);
                scale.push(if white > bl { 1.0 / (white - bl) } else { 1.0 });
            }
        }
    }
    let off_scale = black.len();
    let mut tab = black;
    tab.extend_from_slice(&scale);
    let off_rowd = tab.len();
    tab.extend_from_slice(&b.row_deltas);
    let off_cold = tab.len();
    tab.extend_from_slice(&b.col_deltas);
    // Reciprocals 1/n (n = 0..=9) at off_cold + n_cold, computed like the CPU's `1.0 / len as f32`.
    tab.extend((0..=9).map(|n| if n == 0 { 0.0 } else { 1.0 / n as f32 }));
    CalibTable {
        tab,
        pat_w: pw as u32,
        pat_h: ph as u32,
        off_scale: off_scale as u32,
        off_rowd: off_rowd as u32,
        off_cold: off_cold as u32,
        n_rowd: b.row_deltas.len() as u32,
        n_cold: b.col_deltas.len() as u32,
    }
}

/// CFA site descriptors (see `cfa_site` in common.wgsl): colour (clamped to 3) and,
/// for R/G/B, the mask of 3x3 neighbours of that colour. 1x1 green site without CFA.
pub(crate) fn cfa_sites(info: &FrameInfo) -> (u32, u32, Vec<u32>) {
    let Some(p) = info.cfa else { return (1, 1, vec![1]) };
    let (pw, ph) = (p.width(), p.height());
    let mut out = Vec::with_capacity(pw * ph);
    for py in 0..ph {
        for px in 0..pw {
            let own = (p.color_at(px, py) as u32).min(3);
            let mut d = own;
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let c = (p.color_at(
                        (px as isize + dx + pw as isize * 4) as usize,
                        (py as isize + dy + ph as isize * 4) as usize,
                    ) as u32)
                        .min(3);
                    // A site's own colour is measured (centre only), not interpolated.
                    if c < 3 && (c != own || (dx == 0 && dy == 0)) {
                        let bit = ((dy + 1) * 3 + (dx + 1)) as u32;
                        d |= 1 << (2 + 9 * c + bit);
                    }
                }
            }
            out.push(d);
        }
    }
    (pw as u32, ph as u32, out)
}

/// Linear → sRGB 8-bit table with 4097 entries over [0, 1] (identical to the CPU LUT).
pub(crate) fn srgb_table() -> Vec<u32> {
    (0..=4096)
        .map(|i| {
            let x = i as f32 / 4096.0;
            let s = if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
            (s * 255.0 + 0.5).clamp(0.0, 255.0) as u8 as u32
        })
        .collect()
}

/// Scalar models of the GPU kernels' addressing, written to mirror the WGSL line by
/// line. They let the bit-level semantics be tested on machines without a GPU
/// (against `flashraw_codecs::packed::unpack_row_scalar`), and serve as executable
/// documentation of the shader behaviour.
pub mod reference {
    use flashraw_core::PackOrder;

    /// Description of a packed raster as the `unpack` kernel sees it.
    #[derive(Debug, Clone, Copy)]
    pub struct PackedRaster {
        pub bits: u32,
        pub order: PackOrder,
        /// Bytes from one row start to the next.
        pub row_stride: usize,
        /// Bytes of sample data in one row (bytes past this read as 0).
        pub row_bytes: usize,
    }

    /// Sample `i` of row `y` exactly as `packed_sample` in common.wgsl computes it.
    pub fn packed_sample(data: &[u8], r: &PackedRaster, i: usize, y: usize) -> u16 {
        let row = y * r.row_stride;
        let bitpos = i * r.bits as usize;
        let (k, sh) = (bitpos >> 3, (bitpos & 7) as u32);
        let byte = |k: usize| -> u32 {
            let p = match r.order {
                PackOrder::Msb16 => k ^ 1,
                PackOrder::Msb32 => k ^ 3,
                _ => k,
            };
            if p >= r.row_bytes {
                return 0;
            }
            data.get(row + p).copied().unwrap_or(0) as u32
        };
        let (b0, b1, b2) = (byte(k), byte(k + 1), byte(k + 2));
        let mask = (1u32 << r.bits) - 1;
        let v = if r.order == PackOrder::LsbBytes {
            ((b0 | (b1 << 8) | (b2 << 16)) >> sh) & mask
        } else {
            (((b0 << 16) | (b1 << 8) | b2) >> (24 - sh - r.bits)) & mask
        };
        v as u16
    }

    /// Whole-raster unpack (what the `unpack` kernel produces for a full frame).
    pub fn unpack(data: &[u8], r: &PackedRaster, width: usize, height: usize) -> Vec<u16> {
        let mut out = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                out.push(packed_sample(data, r, x, y));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_layout_matches_wgsl() {
        // 40 scalars + 4 vec4<f32>: 224 bytes, 16-byte multiple (uniform buffer rules).
        assert_eq!(std::mem::size_of::<Params>(), 224);
        assert_eq!(std::mem::offset_of!(Params, wb), 160);
        // Every WGSL field name appears in the host struct in the same order.
        let src = include_str!("../../../shaders/common.wgsl");
        let start = src.find("struct Params {").unwrap();
        let body = &src[start..start + src[start..].find('}').unwrap()];
        let names: Vec<&str> = body
            .lines()
            .skip(1)
            .filter_map(|l| l.trim().split(':').next())
            .filter(|n| !n.is_empty() && !n.starts_with("//"))
            .collect();
        let expected = [
            "width",
            "height",
            "cpp",
            "bits",
            "src_stride",
            "src_offset",
            "src_len",
            "row_bytes",
            "src_x",
            "src_y",
            "origin_x",
            "origin_y",
            "pat_w",
            "pat_h",
            "cfa_w",
            "cfa_h",
            "n_rowd",
            "n_cold",
            "off_scale",
            "off_rowd",
            "off_cold",
            "lut_len",
            "off_srgb",
            "off_cfa",
            "crop_x",
            "crop_y",
            "dev_w",
            "dev_h",
            "out_w",
            "out_h",
            "orientation",
            "total",
            "band_y",
            "band_end",
            "dst_x",
            "dst_y",
            "dst_stride",
            "dst_off",
            "pad0",
            "pad1",
            "wb",
            "m0",
            "m1",
            "m2",
        ];
        assert_eq!(names, expected);
    }

    #[test]
    fn wgsl_modules_validate() {
        for (name, src) in wgsl_sources() {
            let module =
                naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)));
            naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }

    #[test]
    fn bayer_neighbour_masks() {
        let mut info = FrameInfo::new(4, 4, 12);
        info.cfa = Some(flashraw_core::CfaPattern::RGGB);
        let (w, h, sites) = cfa_sites(&info);
        assert_eq!((w, h), (2, 2));
        // R site: own colour R only at the centre (bit 4), G at the cross, B at the corners.
        let r = sites[0];
        assert_eq!(r & 3, 0);
        assert_eq!((r >> 2) & 0x1ff, 1 << 4);
        assert_eq!((r >> 11) & 0x1ff, (1 << 1) | (1 << 3) | (1 << 5) | (1 << 7));
        assert_eq!((r >> 20) & 0x1ff, (1 << 0) | (1 << 2) | (1 << 6) | (1 << 8));
        // G site (top right): G only at the centre, not the diagonals.
        assert_eq!((sites[1] >> 11) & 0x1ff, 1 << 4);
    }

    #[test]
    fn srgb_table_endpoints() {
        let t = srgb_table();
        assert_eq!(t.len(), 4097);
        assert_eq!(t[0], 0);
        assert_eq!(t[4096], 255);
        assert!(t.windows(2).all(|w| w[0] <= w[1]));
    }
}
