// The output of the noise reduction (`denoise.wgsl`): Y, B - Y and R - Y back to linear RGB
// (`flashraw::denoise::merge`), then the tone, encoding and orientation of the develop kernels
// (`store_rgb`). This module is `common.wgsl` + `develop.wgsl` + this file; a dispatch covers the
// developed rows [band_y, band_end) and bind group 1 holds the chunks of the three planes with
// those rows.

struct EncParams {
    y_first: u32, // plane row of the first stored row of each plane binding
    u_first: u32,
    v_first: u32,
    w: u32,       // plane width
}

@group(1) @binding(0) var<uniform> EQ: EncParams;
@group(1) @binding(1) var<storage, read> PY: array<f32>;
@group(1) @binding(2) var<storage, read> PU: array<f32>;
@group(1) @binding(3) var<storage, read> PV: array<f32>;

const DN_KR: f32 = 0.2126;
const DN_KG: f32 = 0.7152;
const DN_KB: f32 = 0.0722;

@compute @workgroup_size(16, 16)
fn denoise_encode(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dx = gid.x;
    let dy = P.band_y + gid.y;
    if (dx >= P.dev_w || dy >= P.band_end) {
        return;
    }
    let y = PY[(dy - EQ.y_first) * EQ.w + dx];
    let u = PU[(dy - EQ.u_first) * EQ.w + dx];
    let v = PV[(dy - EQ.v_first) * EQ.w + dx];
    let r = v + y;
    let b = u + y;
    store_rgb(r, (y - DN_KR * r - DN_KB * b) / DN_KG, b, dx, dy);
}
