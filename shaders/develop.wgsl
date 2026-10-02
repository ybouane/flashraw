// Fused develop: fetch (packed bits / u16 / f32) -> linearise -> black/white
// normalisation -> demosaic -> white balance -> clip -> camera->sRGB matrix ->
// (RGBA8: sRGB transfer + EXIF orientation) -> store. One pass, no full-size
// intermediates (CPU reference: `flashraw::process::{develop, to_rgba8}`).

// 0 = single-plane without CFA (grey), 1 = CFA mosaic (cpp 1), 2 = RGB(+) planes (cpp >= 3),
// 3 = Bayer mosaic (as 1, with the CPU Bayer fast-path summation order).
override LAYOUT: u32 = 1u;

fn is_cfa() -> bool {
    return LAYOUT == 1u || LAYOUT == 3u;
}

// 3x3 neighbour indices ((dy + 1) * 3 + dx + 1) in Bayer summation order.
const BAYER_ORDER = array<u32, 9>(3u, 5u, 1u, 7u, 0u, 2u, 6u, 8u, 4u);
// 0 = RGBA8 (sRGB, orientation applied, one u32 per pixel), 1 = linear RGB f32 (3 words per pixel).
override OUT_MODE: u32 = 0u;

const TILE: u32 = 16u;
const APRON: u32 = 18u; // TILE + 2
var<workgroup> tile: array<f32, 324>; // APRON * APRON normalised samples

fn srgb8(v: f32) -> u32 {
    // Same quantisation as the CPU LUT: index = trunc(clamp(v, 0, 1) * 4096).
    let i = u32(clamp(v, 0.0, 1.0) * 4096.0);
    return lut[P.off_srgb + min(i, 4096u)];
}

// White balance, clip at sensor saturation, convert to linear sRGB, store.
fn finish(rgb_in: vec3<f32>, dx: u32, dy: u32) {
    let rgb = clamp(rgb_in * P.wb.xyz, vec3<f32>(0.0), vec3<f32>(1.0));
    let r = P.m0.x * rgb.x + P.m0.y * rgb.y + P.m0.z * rgb.z;
    let g = P.m1.x * rgb.x + P.m1.y * rgb.y + P.m1.z * rgb.z;
    let b = P.m2.x * rgb.x + P.m2.y * rgb.y + P.m2.z * rgb.z;
    if (OUT_MODE == 1u) {
        let o = (dy * P.dev_w + dx) * 3u;
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
    dst[oy * P.out_w + ox] = srgb8(r) | (srgb8(g) << 8u) | (srgb8(b) << 16u) | (255u << 24u);
}

@compute @workgroup_size(16, 16)
fn develop(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) l: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let bx = wg.x * TILE;
    let by = wg.y * TILE;
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
    if (dx >= P.dev_w || dy >= P.dev_h) {
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
        for (var c = 0u; c < 3u; c++) {
            let mask = (site >> (2u + 9u * c)) & 0x1ffu;
            var s = 0.0;
            var n = 0u;
            for (var j = 0u; j < 9u; j++) {
                var k = j;
                if (LAYOUT == 3u) {
                    k = BAYER_ORDER[j];
                }
                if ((mask & (1u << k)) != 0u) {
                    let ky = k / 3u;
                    let kx = k - ky * 3u;
                    let xx = i32(x + kx) - 1;
                    let yy = i32(y + ky) - 1;
                    if (xx >= 0 && yy >= 0 && u32(xx) < P.width && u32(yy) < P.height) {
                        s += tile[(l.y + ky) * APRON + l.x + kx];
                        n += 1u;
                    }
                }
            }
            if (n > 0u) {
                if (interior) {
                    rgb[c] = s * tab[P.off_cold + P.n_cold + n];
                } else {
                    rgb[c] = s / f32(n);
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
    let dy = gid.y;
    if (dx >= P.dev_w || dy >= P.dev_h) {
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
            if (is_cfa()) {
                let c = cfa_site(x, y) & 3u;
                acc[c] += sample_norm(x, y, 0u);
                n[c] += 1.0;
            } else if (LAYOUT == 2u) {
                for (var c = 0u; c < 3u; c++) {
                    acc[c] += sample_norm(x, y, c);
                    n[c] += 1.0;
                }
            } else {
                acc[0] += sample_norm(x, y, 0u);
                n[0] += 1.0;
            }
        }
    }
    var rgb = vec3<f32>(0.0);
    for (var c = 0u; c < 3u; c++) {
        if (n[c] > 0.0) {
            rgb[c] = acc[c] / n[c];
        }
    }
    if (LAYOUT == 0u) {
        rgb = vec3<f32>(rgb.x);
    }
    finish(rgb, dx, dy);
}
