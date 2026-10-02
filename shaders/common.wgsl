// FlashRaw GPU kernels: shared declarations.
//
// Every kernel module is `common.wgsl` + one kernel file, concatenated on the host
// (WGSL has no include). The same sources are used by native wgpu (Vulkan/Metal/DX12)
// and by WebGPU in the browser.
//
// Sample addressing:
//   * frame coordinates (x, y): position inside the decoded frame (0..width, 0..height);
//   * source raster: frame (x, y) lives at (x + src_x, y + src_y) of the uploaded raster
//     (non-zero for region decodes straight from a packed payload);
//   * sensor coordinates: (x + origin_x, y + origin_y); CFA and black-level patterns
//     are anchored there, exactly like the CPU reference (`flashraw::process`).

struct Params {
    width: u32,       // frame width in pixels
    height: u32,      // frame height in pixels
    cpp: u32,         // samples per pixel
    bits: u32,        // packed: bits per sample
    src_stride: u32,  // packed: bytes per row; u16/f32: samples per row
    src_offset: u32,  // packed: byte offset of the payload inside `src`
    src_len: u32,     // packed: payload length in bytes (bytes beyond read as 0)
    row_bytes: u32,   // packed: bytes of one row (bytes beyond read as 0 for that row)
    src_x: u32,
    src_y: u32,
    origin_x: u32,
    origin_y: u32,
    pat_w: u32,       // calibration pattern (lcm of CFA and black pattern)
    pat_h: u32,
    cfa_w: u32,       // CFA repeat (1x1 without CFA)
    cfa_h: u32,
    n_rowd: u32,      // black row deltas (sensor rows)
    n_cold: u32,      // black column deltas (sensor columns)
    off_scale: u32,   // offsets into `tab`
    off_rowd: u32,
    off_cold: u32,
    lut_len: u32,     // linearisation LUT entries at lut[0..lut_len)
    off_srgb: u32,    // 4097-entry linear -> sRGB8 table in `lut`
    off_cfa: u32,     // CFA site descriptors in `lut`
    crop_x: u32,      // develop: crop origin (frame coordinates)
    crop_y: u32,
    dev_w: u32,       // develop: developed (pre-orientation) size
    dev_h: u32,
    out_w: u32,       // output size (after orientation)
    out_h: u32,
    orientation: u32, // EXIF orientation 1..8
    total: u32,       // 1-D kernels: number of output elements
    wb: vec4<f32>,
    m0: vec4<f32>,    // camera -> linear sRGB matrix rows
    m1: vec4<f32>,
    m2: vec4<f32>,
}

@group(0) @binding(0) var<uniform> P: Params;
// Input raster as 32-bit words: packed bytes (little-endian), u16 pairs, or f32 bits.
@group(0) @binding(1) var<storage, read> src: array<u32>;
// Calibration tables: black[cpp][pat], scale[cpp][pat], row deltas, column deltas.
@group(0) @binding(2) var<storage, read> tab: array<f32>;
// Integer tables: linearisation LUT, sRGB LUT, CFA site descriptors.
@group(0) @binding(3) var<storage, read> lut: array<u32>;
// Output (u16 pairs, packed RGBA8, or f32 bits).
@group(0) @binding(4) var<storage, read_write> dst: array<u32>;

// 0 = u16 samples (two per word), 1 = f32 samples, 2 = bit-packed payload.
override INPUT_MODE: u32 = 0u;
// Packed bit order: 0 = MSB-first bytes, 1 = LSB-first bytes, 2 = MSB in LE 16-bit words,
// 3 = MSB in LE 32-bit words (`flashraw_core::PackOrder`).
override ORDER: u32 = 0u;
// Apply the linearisation LUT to integer samples.
override USE_LUT: bool = false;

// Byte `i` of the packed payload (0 beyond its end).
fn payload_byte(i: u32) -> u32 {
    if (i >= P.src_len) {
        return 0u;
    }
    let a = i + P.src_offset;
    return (src[a >> 2u] >> ((a & 3u) * 8u)) & 0xffu;
}

// Logical byte `k` of the row starting at byte `row`: word-swapped orders read the
// physical byte `k ^ 1` / `k ^ 3`; bytes past the row's data read as 0, like the
// CPU bit pumps reading a row slice.
fn row_byte(row: u32, k: u32) -> u32 {
    var p = k;
    if (ORDER == 2u) {
        p = k ^ 1u;
    } else if (ORDER == 3u) {
        p = k ^ 3u;
    }
    if (p >= P.row_bytes) {
        return 0u;
    }
    return payload_byte(row + p);
}

// Sample `i` (in samples from the row start) of packed row `y` (source raster).
fn packed_sample(i: u32, y: u32) -> u32 {
    let row = y * P.src_stride;
    let bitpos = i * P.bits;
    let k = bitpos >> 3u;
    let sh = bitpos & 7u;
    let mask = (1u << P.bits) - 1u;
    if (ORDER <= 1u && row + k + 3u <= P.src_len) {
        // Fast path for byte orders: the 3 bytes come from two adjacent words. Bytes
        // past the row's data only feed bits outside this sample, so no clamping needed.
        let a = row + k + P.src_offset;
        let wi = a >> 2u;
        let sb = (a & 3u) * 8u;
        var v24 = src[wi] >> sb;
        if (sb > 8u) {
            v24 |= src[wi + 1u] << (32u - sb);
        }
        if (ORDER == 1u) {
            return (v24 >> sh) & mask;
        }
        let msb = ((v24 & 0xffu) << 16u) | (v24 & 0xff00u) | ((v24 >> 16u) & 0xffu);
        return (msb >> (24u - sh - P.bits)) & mask;
    }
    let b0 = row_byte(row, k);
    let b1 = row_byte(row, k + 1u);
    let b2 = row_byte(row, k + 2u);
    if (ORDER == 1u) {
        let v = b0 | (b1 << 8u) | (b2 << 16u);
        return (v >> sh) & mask;
    }
    let v = (b0 << 16u) | (b1 << 8u) | b2;
    return (v >> (24u - sh - P.bits)) & mask;
}

fn linearize(v: u32) -> u32 {
    if (USE_LUT && P.lut_len > 0u) {
        return lut[min(v, P.lut_len - 1u)];
    }
    return v;
}

// Integer sample (u16 or packed input) at frame (x, y), component c.
fn int_sample(x: u32, y: u32, c: u32) -> u32 {
    let sy = y + P.src_y;
    let si = (x + P.src_x) * P.cpp + c;
    if (INPUT_MODE == 2u) {
        return linearize(packed_sample(si, sy));
    }
    let idx = sy * P.src_stride + si;
    return linearize((src[idx >> 1u] >> ((idx & 1u) * 16u)) & 0xffffu);
}

// Raw sample value (before calibration) at frame (x, y), component c.
fn raw_value(x: u32, y: u32, c: u32) -> f32 {
    if (INPUT_MODE == 1u) {
        let idx = (y + P.src_y) * P.src_stride + (x + P.src_x) * P.cpp + c;
        return bitcast<f32>(src[idx]);
    }
    return f32(int_sample(x, y, c));
}

// a % m for uniform m: a mask when m is a power of two (integer division is slow on GPUs).
fn umod(a: u32, m: u32) -> u32 {
    if ((m & (m - 1u)) == 0u) {
        return a & (m - 1u);
    }
    return a % m;
}

// Black-subtracted, white-normalised value (same operation order as the CPU `Calib::norm`).
fn norm(v: f32, x: u32, y: u32, c: u32) -> f32 {
    let sx = x + P.origin_x;
    let sy = y + P.origin_y;
    let i = c * P.pat_w * P.pat_h + umod(sy, P.pat_h) * P.pat_w + umod(sx, P.pat_w);
    var b = tab[i];
    if (sy < P.n_rowd) {
        b += tab[P.off_rowd + sy];
    }
    if (sx < P.n_cold) {
        b += tab[P.off_cold + sx];
    }
    return (v - b) * tab[P.off_scale + i];
}

fn sample_norm(x: u32, y: u32, c: u32) -> f32 {
    return norm(raw_value(x, y, c), x, y, c);
}

// CFA site descriptor at frame (x, y): bits 0-1 colour (R, G, B, other), then three
// 9-bit masks (bits 2, 11, 20) of the 3x3 neighbours with colour R, G, B
// (bit (dy + 1) * 3 + (dx + 1)).
fn cfa_site(x: u32, y: u32) -> u32 {
    let sx = x + P.origin_x;
    let sy = y + P.origin_y;
    return lut[P.off_cfa + umod(sy, P.cfa_h) * P.cfa_w + umod(sx, P.cfa_w)];
}

// Linear thread index for 1-D kernels dispatched as a 2-D grid of 256-wide groups.
fn linear_index(wg: vec3<u32>, nwg: vec3<u32>, li: u32) -> u32 {
    return (wg.y * nwg.x + wg.x) * 256u + li;
}
