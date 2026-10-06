// Fused develop: fetch (packed bits / u16 / f32) -> linearise -> black/white
// normalisation -> demosaic -> white balance -> clip -> camera->sRGB matrix ->
// (RGBA8: sRGB transfer + EXIF orientation) -> store. One pass, no full-size
// intermediates (CPU reference: `flashraw::process::{develop, to_rgba8}`).
//
// Bands: a dispatch develops rows [band_y, band_end) of the developed image. Frame
// coordinates stay global (CFA phase, black pattern and borders are unchanged); only
// the source fetch is relative to the uploaded rows (`src_y`) and the store relative
// to the bound output rectangle (`dst_x`, `dst_y`, `dst_stride`, `dst_off`).

// 0 = single-plane without CFA (grey), 1 = CFA mosaic (cpp 1), 2 = RGB(+) planes (cpp >= 3),
// 3 = Bayer mosaic (as 1, with the CPU Bayer fast-path summation order).
override LAYOUT: u32 = 1u;

fn is_cfa() -> bool {
    return LAYOUT == 1u || LAYOUT == 3u;
}

// 0 = RGBA8 (sRGB, orientation applied, one u32 per pixel), 1 = linear RGB f32 (3 words per pixel),
// 16-bit RGBA, two words per pixel (R | G << 16, B | A << 16): 2 = sRGB (orientation applied),
// 3 = linear integers, 4 = linear half floats. The host drops the fourth sample on download.
override OUT_MODE: u32 = 0u;

const TILE: u32 = 16u;
const APRON: u32 = 18u; // TILE + 2
var<workgroup> tile: array<f32, 324>; // APRON * APRON normalised samples

fn srgb8(v: f32) -> u32 {
    // Same quantisation as the CPU LUT: index = trunc(clamp(v, 0, 1) * 4096).
    let i = u32(clamp(v, 0.0, 1.0) * 4096.0);
    return lut[P.off_srgb + min(i, 4096u)];
}

// Linear -> sRGB 16-bit: the CPU `srgb16` interpolation of the same table.
fn srgb16(v: f32) -> u32 {
    let x = clamp(v, 0.0, 1.0) * 16384.0;
    let i = min(u32(x), 16383u);
    let t = x - f32(i);
    let a = tab[P.off_srgb16 + i];
    let b = tab[P.off_srgb16 + i + 1u];
    return u32(a + (b - a) * t + 0.5);
}

fn lin16(v: f32) -> u32 {
    return u32(clamp(v, 0.0, 1.0) * 65535.0 + 0.5);
}

// f32 -> IEEE half bits, round to nearest even, saturating at +-65504: the CPU
// `f32_to_f16` (integer operations, so the result is the same bit for bit; the
// `pack2x16float` builtin needs an optional shader capability and leaves rounding open).
fn f16_bits(x: f32) -> u32 {
    let bits = bitcast<u32>(x);
    let sign = (bits >> 16u) & 0x8000u;
    let a = bits & 0x7fffffffu;
    let normal = (a + 0xc8000fffu + ((a >> 13u) & 1u)) >> 13u;
    // Below the smallest normal half: the float addition rounds into the subnormal range.
    let sub = bitcast<u32>(bitcast<f32>(a) + 0.5) - 0x3f000000u;
    var h = select(normal, sub, a < 0x38800000u);
    h = select(h, select(0x7bffu, 0x7e00u, a > 0x7f800000u), a >= 0x477ff000u);
    return sign | h;
}

// White balance, clip at sensor saturation, convert to linear sRGB, levels, store.
fn finish(rgb_in: vec3<f32>, dx: u32, dy: u32) {
    let rgb = clamp(rgb_in * P.wb.xyz, vec3<f32>(0.0), vec3<f32>(1.0));
    var r = P.m0.x * rgb.x + P.m0.y * rgb.y + P.m0.z * rgb.z;
    var g = P.m1.x * rgb.x + P.m1.y * rgb.y + P.m1.z * rgb.z;
    var b = P.m2.x * rgb.x + P.m2.y * rgb.y + P.m2.z * rgb.z;
    if (P.levels.z != 0.0) {
        r = r * P.levels.x - P.levels.y;
        g = g * P.levels.x - P.levels.y;
        b = b * P.levels.x - P.levels.y;
    }
    if (OUT_MODE == 1u) {
        let o = ((dy - P.dst_y) * P.dst_stride + dx - P.dst_x) * 3u + P.dst_off;
        dst[o] = bitcast<u32>(r);
        dst[o + 1u] = bitcast<u32>(g);
        dst[o + 2u] = bitcast<u32>(b);
        return;
    }
    // Destination of developed pixel (dx, dy) under the EXIF orientation (inverse of
    // the output->source mapping used by the CPU `to_rgba8`).
    let w = P.dev_w;
    let h = P.dev_h;
    var ox = dx;
    var oy = dy;
    switch P.orientation {
        case 2u: { ox = w - 1u - dx; }
        case 3u: { ox = w - 1u - dx; oy = h - 1u - dy; }
        case 4u: { oy = h - 1u - dy; }
        case 5u: { ox = dy; oy = dx; }
        case 6u: { ox = h - 1u - dy; oy = dx; }
        case 7u: { ox = h - 1u - dy; oy = w - 1u - dx; }
        case 8u: { ox = dy; oy = w - 1u - dx; }
        default: {}
    }
    if (OUT_MODE == 0u) {
        dst[(oy - P.dst_y) * P.dst_stride + ox - P.dst_x + P.dst_off] = srgb8(r) | (srgb8(g) << 8u) | (srgb8(b) << 16u) | (255u << 24u);
        return;
    }
    var lo: u32;
    var hi: u32;
    if (OUT_MODE == 2u) {
        lo = srgb16(r) | (srgb16(g) << 16u);
        hi = srgb16(b) | (0xffffu << 16u);
    } else if (OUT_MODE == 3u) {
        lo = lin16(r) | (lin16(g) << 16u);
        hi = lin16(b) | (0xffffu << 16u);
    } else {
        lo = f16_bits(r) | (f16_bits(g) << 16u);
        hi = f16_bits(b) | (0x3c00u << 16u);
    }
    let o = ((oy - P.dst_y) * P.dst_stride + ox - P.dst_x) * 2u + P.dst_off;
    dst[o] = lo;
    dst[o + 1u] = hi;
}

struct Taps {
    s: vec3<f32>,
    n: vec3<f32>,
}

// Accumulate 3x3 tap k ((dy + 1) * 3 + dx + 1) into the colours whose neighbour mask has it.
fn tap(acc: ptr<function, Taps>, site: u32, k: u32, base: u32, x: u32, y: u32, interior: bool) {
    let ky = k / 3u;
    let kx = k - ky * 3u;
    var sel = vec3<f32>(vec3<u32>((site >> (2u + k)) & 1u, (site >> (11u + k)) & 1u, (site >> (20u + k)) & 1u));
    if (!interior) {
        let xx = i32(x + kx) - 1;
        let yy = i32(y + ky) - 1;
        if (xx < 0 || yy < 0 || u32(xx) >= P.width || u32(yy) >= P.height) {
            sel = vec3<f32>(0.0);
        }
    }
    let v = tile[base + ky * APRON + kx];
    (*acc).s += v * sel;
    (*acc).n += sel;
}

@compute @workgroup_size(16, 16)
fn develop(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) l: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let bx = wg.x * TILE;
    let by = P.band_y + wg.y * TILE;
    if (is_cfa()) {
        // Stage the normalised (TILE + 2)^2 neighbourhood once per workgroup.
        for (var t = li; t < APRON * APRON; t += 256u) {
            let ty = t / APRON;
            let tx = t - ty * APRON;
            let fx = i32(P.crop_x + bx + tx) - 1;
            let fy = i32(P.crop_y + by + ty) - 1;
            var v = 0.0;
            if (fx >= 0 && fy >= 0 && u32(fx) < P.width && u32(fy) < P.height) {
                v = sample_norm(u32(fx), u32(fy), 0u);
            }
            tile[t] = v;
        }
        workgroupBarrier();
    }
    let dx = bx + l.x;
    let dy = by + l.y;
    if (dx >= P.dev_w || dy >= P.band_end) {
        return;
    }
    let x = P.crop_x + dx;
    let y = P.crop_y + dy;
    var rgb = vec3<f32>(0.0);
    if (is_cfa()) {
        // Own colour: the centre sample. Other colours: mean of the same-colour samples
        // of the 3x3 neighbourhood inside the frame. Interior sites multiply by the
        // host-computed 1/count (like the CPU fast paths); borders divide by the
        // in-bounds count. Bayer sums in the CPU fast-path order (left, right, up,
        // down, then diagonals); other CFAs row by row.
        let site = cfa_site(x, y);
        let interior = x > 0u && y > 0u && x + 1u < P.width && y + 1u < P.height;
        var acc = Taps(vec3<f32>(0.0), vec3<f32>(0.0));
        let base = l.y * APRON + l.x;
        // All three colours accumulate in one pass over the taps; a tap adds v * 0 to
        // colours it does not belong to, which leaves their sums bit-identical.
        if (LAYOUT == 3u) {
            tap(&acc, site, 3u, base, x, y, interior);
            tap(&acc, site, 5u, base, x, y, interior);
            tap(&acc, site, 1u, base, x, y, interior);
            tap(&acc, site, 7u, base, x, y, interior);
            tap(&acc, site, 0u, base, x, y, interior);
            tap(&acc, site, 2u, base, x, y, interior);
            tap(&acc, site, 6u, base, x, y, interior);
            tap(&acc, site, 8u, base, x, y, interior);
            tap(&acc, site, 4u, base, x, y, interior);
        } else {
            for (var k = 0u; k < 9u; k++) {
                tap(&acc, site, k, base, x, y, interior);
            }
        }
        let inv_base = P.off_cold + P.n_cold;
        for (var c = 0u; c < 3u; c++) {
            let n = acc.n[c];
            if (n > 0.0) {
                if (interior) {
                    rgb[c] = acc.s[c] * tab[inv_base + u32(n)];
                } else {
                    rgb[c] = acc.s[c] / n;
                }
            }
        }
    } else if (LAYOUT == 2u) {
        rgb = vec3<f32>(sample_norm(x, y, 0u), sample_norm(x, y, 1u), sample_norm(x, y, 2u));
    } else {
        rgb = vec3<f32>(sample_norm(x, y, 0u));
    }
    finish(rgb, dx, dy);
}

// Half-size 2x2 superpixel develop (CPU reference: `develop` with `Demosaic::HalfSize`).
@compute @workgroup_size(16, 16)
fn develop_half(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dx = gid.x;
    let dy = P.band_y + gid.y;
    if (dx >= P.dev_w || dy >= P.band_end) {
        return;
    }
    let x0 = P.crop_x + dx * 2u;
    let y0 = P.crop_y + dy * 2u;
    var acc = vec4<f32>(0.0);
    var n = vec4<f32>(0.0);
    for (var k = 0u; k < 4u; k++) {
        let x = x0 + (k & 1u);
        let y = y0 + (k >> 1u);
        if (x < P.width && y < P.height) {
            // Accumulate through channel masks rather than `acc[c] += ...`: some shader
            // compilers (FXC on Direct3D 12) reject dynamically indexed vector writes.
            if (is_cfa()) {
                let c = cfa_site(x, y) & 3u;
                let m = select(vec4<f32>(0.0), vec4<f32>(1.0), vec4<u32>(c) == vec4<u32>(0u, 1u, 2u, 3u));
                acc += m * sample_norm(x, y, 0u);
                n += m;
            } else if (LAYOUT == 2u) {
                acc += vec4<f32>(sample_norm(x, y, 0u), sample_norm(x, y, 1u), sample_norm(x, y, 2u), 0.0);
                n += vec4<f32>(1.0, 1.0, 1.0, 0.0);
            } else {
                acc.x += sample_norm(x, y, 0u);
                n.x += 1.0;
            }
        }
    }
    let have = n.xyz > vec3<f32>(0.0);
    let rgb_avg = acc.xyz / select(vec3<f32>(1.0), n.xyz, have);
    var rgb = select(vec3<f32>(0.0), rgb_avg, have);
    if (LAYOUT == 0u) {
        rgb = vec3<f32>(rgb.x);
    }
    finish(rgb, dx, dy);
}
