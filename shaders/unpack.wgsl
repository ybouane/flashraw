// Unpack a bit-packed payload (INPUT_MODE 2) or re-window a u16 raster (INPUT_MODE 0)
// into u16 samples, optionally through the linearisation LUT.
//
// Output: `width * height * cpp` u16 samples, two per u32 word (little-endian halves),
// i.e. byte-identical to a `Vec<u16>` on the host. One invocation writes one word, so
// no two invocations touch the same word even when rows have an odd length.
// A band covers `total` samples starting at frame row `band_y` (an even sample index)
// and writes them from word `dst_off` of the bound output range.

@compute @workgroup_size(256)
fn unpack(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let w = linear_index(wg, nwg, li);
    let n = P.total; // samples
    let s0 = w * 2u;
    if (s0 >= n) {
        return;
    }
    let rl = P.width * P.cpp;
    var word = 0u;
    for (var j = 0u; j < 2u; j++) {
        let s = s0 + j;
        if (s < n) {
            let ly = s / rl;
            let r = s - ly * rl;
            let y = ly + P.band_y;
            let x = r / P.cpp;
            let c = r - x * P.cpp;
            word |= (int_sample(x, y, c) & 0xffffu) << (16u * j);
        }
    }
    dst[w + P.dst_off] = word;
}
