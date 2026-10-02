// Interop helper: copy a packed RGBA8 buffer (one u32 per pixel, as produced by
// `develop` with OUT_MODE 0) into an rgba8unorm storage texture. Standalone module
// (does not include common.wgsl).

struct TexParams {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> T: TexParams;
@group(0) @binding(1) var<storage, read> pixels: array<u32>;
@group(0) @binding(2) var out_tex: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16)
fn to_texture(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= T.width || gid.y >= T.height) {
        return;
    }
    let p = pixels[gid.y * T.width + gid.x];
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), unpack4x8unorm(p));
}
