// Black subtraction + white normalisation to f32, keeping the sensor layout
// (CPU reference: `flashraw::process::normalize`). Output: `width * height * cpp` f32.

@compute @workgroup_size(256)
fn normalize(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let i = linear_index(wg, nwg, li);
    if (i >= P.total) {
        return;
    }
    let rl = P.width * P.cpp;
    let y = i / rl;
    let r = i - y * rl;
    let x = r / P.cpp;
    let c = r - x * P.cpp;
    dst[i] = bitcast<u32>(max(sample_norm(x, y, c), 0.0));
}
