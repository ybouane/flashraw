// Noise reduction of developed linear RGB (CPU reference: `flashraw::denoise`, the method in
// docs/design/noise-reduction.md). A standalone module: every kernel reads and writes planes of
// f32 samples.
//
// A plane (one pyramid level of Y, B - Y or R - Y, or a guided-filter intermediate) is stored in
// row chunks: a chunk holds the rows `[first, first + stored)` of the plane, `w` samples per row,
// and the kernel that writes it computes the rows `[rows0, rows1)` (its interior); the rows
// around the interior (a halo of up to 16 rows) are copied from the neighbouring chunks by the
// host. Each binding of a dispatch is one chunk, addressed through its `*_first` row. Most
// frames are one chunk per plane.
//
// The DCT passes sum the blocks of each block row per column and inverse-transform the columns
// (`dn_rows`), then every sample gathers the block rows that cover it (`dn_gather`): WGSL has no
// floating-point atomics, and the sums run in a fixed order.

struct DnParams {
    w: u32,          // plane width and height (level coordinates)
    h: u32,
    rows0: u32,      // rows computed by this dispatch
    rows1: u32,
    a_first: u32,    // plane row of the first stored row of each binding
    b_first: u32,
    c_first: u32,
    d_first: u32,
    o_first: u32,
    o2_first: u32,
    cw: u32,         // size of the other level (finer for dn_down, coarser for dn_add_up and dn_gf_apply)
    ch: u32,
    step: u32,       // DCT passes: block step; stats: step of the measured grid (in blocks)
    nbx: u32,        // blocks per block row (padded coordinates)
    nreg_x: u32,     // blocks at multiples of `step`; block `nreg_x` (if any) sits at `last_x`
    last_x: u32,
    nreg_y: u32,
    last_y: u32,
    i0: u32,         // block rows of this sub-band: [i0, i1)
    i1: u32,
    mode: u32,       // variant (see each kernel)
    r: u32,          // box radius
    off_model: u32,  // noise model in `T`: [bins, means x 12, pad x 3] then 12 x 64 deviations
    off_var: u32,    // pixel variance in `T`: [bins, means x 12, pad x 3, variances x 12]
    scols: u32,      // stats: measured blocks per row, first measured block row of this dispatch
    sg0: u32,
    a_off: u32,      // dn_split: word offset of the first stored row inside the `A` binding
    sh: u32,         // DCT passes: log2 of `step` (a power of two)
    kf0: f32,        // hard threshold scale (blocks), keep factor (blend), 1 / n (box)
    kf1: f32,        // Wiener noise scale (gain^2)
    kf2: f32,
    kf3: f32,
}

@group(0) @binding(0) var<uniform> Q: DnParams;
@group(0) @binding(1) var<storage, read> A: array<f32>;
@group(0) @binding(2) var<storage, read> B: array<f32>;
@group(0) @binding(3) var<storage, read> C: array<f32>;
@group(0) @binding(4) var<storage, read> D: array<f32>;
@group(0) @binding(5) var<storage, read> T: array<f32>;
@group(0) @binding(6) var<storage, read_write> O: array<f32>;
@group(0) @binding(7) var<storage, read_write> O2: array<f32>;

// Rec. 709 luminance weights (`flashraw::denoise::{KR, KG, KB}`).
const KR: f32 = 0.2126;
const KG: f32 = 0.7152;
const KB: f32 = 0.0722;

fn ia(x: u32, y: u32) -> u32 { return (y - Q.a_first) * Q.w + x; }
fn ib(x: u32, y: u32) -> u32 { return (y - Q.b_first) * Q.w + x; }
fn ic(x: u32, y: u32) -> u32 { return (y - Q.c_first) * Q.w + x; }
fn id(x: u32, y: u32) -> u32 { return (y - Q.d_first) * Q.w + x; }
fn io(x: u32, y: u32) -> u32 { return (y - Q.o_first) * Q.w + x; }
fn io2(x: u32, y: u32) -> u32 { return (y - Q.o2_first) * Q.w + x; }

// Pixel (x, y) of a 2-D dispatch over the rows [rows0, rows1); false outside.
fn pixel(g: vec3<u32>) -> vec2<u32> {
    return vec2<u32>(g.x, Q.rows0 + g.y);
}
fn inside(p: vec2<u32>) -> bool {
    return p.x < Q.w && p.y < Q.rows1;
}

// Y, B - Y or R - Y (mode 0, 1, 2) of interleaved linear RGB (`A`, three words per pixel).
@compute @workgroup_size(16, 16)
fn dn_split(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    let o = Q.a_off + ((p.y - Q.a_first) * Q.w + p.x) * 3u;
    let r = A[o];
    let gg = A[o + 1u];
    let b = A[o + 2u];
    let l = KR * r + KG * gg + KB * b;
    var v = l;
    if (Q.mode == 1u) { v = b - l; } else if (Q.mode == 2u) { v = r - l; }
    O[io(p.x, p.y)] = v;
}

// 2x2 average of the finer level `A` (`cw` x `ch`, the last row or column repeated on odd
// sides); mode 1: `B - average` (the coarse result minus the low frequencies of a level).
@compute @workgroup_size(16, 16)
fn dn_down(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    let x0 = 2u * p.x;
    let x1 = min(2u * p.x + 1u, Q.cw - 1u);
    let y0 = 2u * p.y;
    let y1 = min(2u * p.y + 1u, Q.ch - 1u);
    let fa = (y0 - Q.a_first) * Q.cw;
    let fb = (y1 - Q.a_first) * Q.cw;
    let v = 0.25 * (A[fa + x0] + A[fa + x1] + A[fb + x0] + A[fb + x1]);
    if (Q.mode == 1u) {
        O[io(p.x, p.y)] = B[ib(p.x, p.y)] - v;
    } else {
        O[io(p.x, p.y)] = v;
    }
}

// The two taps and the weight of the second of bilinear upsampling to position `i` from a
// level of side `n` (pixel centres aligned; `flashraw::denoise::tap`).
fn tap(i: u32, n: u32) -> vec3<f32> {
    let s = max((f32(i) + 0.5) * 0.5 - 0.5, 0.0);
    let a = min(u32(s), n - 1u);
    return vec3<f32>(f32(a), f32(min(a + 1u, n - 1u)), s - f32(a));
}

// Bilinear sample of the coarser level in `A` (`cw` x `ch`) at fine position (x, y).
fn up_a(x: u32, y: u32) -> f32 {
    let tx = tap(x, Q.cw);
    let ty = tap(y, Q.ch);
    let r0 = (u32(ty.x) - Q.a_first) * Q.cw;
    let r1 = (u32(ty.y) - Q.a_first) * Q.cw;
    let a = A[r0 + u32(tx.x)] + (A[r0 + u32(tx.y)] - A[r0 + u32(tx.x)]) * tx.z;
    let b = A[r1 + u32(tx.x)] + (A[r1 + u32(tx.y)] - A[r1 + u32(tx.x)]) * tx.z;
    return a + (b - a) * ty.z;
}

// Bilinear sample of the coarser level in `C` (`cw` x `ch`) at fine position (x, y).
fn up_c(x: u32, y: u32) -> f32 {
    let tx = tap(x, Q.cw);
    let ty = tap(y, Q.ch);
    let r0 = (u32(ty.x) - Q.c_first) * Q.cw;
    let r1 = (u32(ty.y) - Q.c_first) * Q.cw;
    let a = C[r0 + u32(tx.x)] + (C[r0 + u32(tx.y)] - C[r0 + u32(tx.x)]) * tx.z;
    let b = C[r1 + u32(tx.x)] + (C[r1 + u32(tx.y)] - C[r1 + u32(tx.x)]) * tx.z;
    return a + (b - a) * ty.z;
}

// `O += up(A)`: the Laplacian recomposition (`A` holds the coarse result minus the 2x2
// average of `O`).
@compute @workgroup_size(16, 16)
fn dn_add_up(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    let k = io(p.x, p.y);
    O[k] = O[k] + up_a(p.x, p.y);
}

// Pointwise: mode 0 `O += kf0 (A - O)` (strength blend), mode 1 `O = max(O - A^2, 0)` (window
// variance), mode 2 `O += eps(A)` (the guided filter's regularisation at the window mean), mode 3
// `O = A` (copy).
@compute @workgroup_size(16, 16)
fn dn_point(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    let k = io(p.x, p.y);
    let a = A[ia(p.x, p.y)];
    switch Q.mode {
        case 0u: { O[k] = O[k] + Q.kf0 * (a - O[k]); }
        case 1u: { O[k] = max(O[k] - a * a, 0.0); }
        case 2u: { O[k] = O[k] + max(pixel_var(a), 1e-9); }
        default: { O[k] = a; }
    }
}

// The luma noise variance per pixel at luma mean `m` (`flashraw::denoise::Variance::at`).
fn pixel_var(m: f32) -> f32 {
    let n = u32(T[Q.off_var]);
    var i = 0u;
    for (; i < n; i++) {
        if (!(T[Q.off_var + 1u + i] < m)) { break; }
    }
    let v = Q.off_var + 16u;
    if (i == 0u) { return T[v]; }
    if (i >= n) { return T[v + n - 1u]; }
    let a = T[Q.off_var + i];
    let b = T[Q.off_var + 1u + i];
    var t = 0.0;
    if (b > a) { t = (m - a) / (b - a); }
    return T[v + i - 1u] + (T[v + i] - T[v + i - 1u]) * t;
}

// Box sums of radius r, edges repeated. Vertical (mode 0: of `A`, 1: of `A * B`) into `O`,
// then horizontal (`dn_box_h`) of `A` divided by kf0 ((2r + 1)^2).
@compute @workgroup_size(16, 16)
fn dn_box_v(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    var s = 0.0;
    let r = i32(Q.r);
    for (var d = -r; d <= r; d++) {
        let y = u32(clamp(i32(p.y) + d, 0, i32(Q.h) - 1));
        var v = A[ia(p.x, y)];
        if (Q.mode == 1u) { v = v * B[ib(p.x, y)]; }
        s += v;
    }
    O[io(p.x, p.y)] = s;
}

@compute @workgroup_size(16, 16)
fn dn_box_h(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    var s = 0.0;
    let r = i32(Q.r);
    let row = (p.y - Q.a_first) * Q.w;
    for (var d = -r; d <= r; d++) {
        s += A[row + u32(clamp(i32(p.x) + d, 0, i32(Q.w) - 1))];
    }
    O[io(p.x, p.y)] = s / Q.kf0;
}

// The guided filter's linear coefficients: `a = (cov - mean mp) / den` into `O`, `b = mp - a mean`
// into `O2` (`A` cov, `B` mp, `C` the guide's window mean, `D` its variance plus the
// regularisation).
@compute @workgroup_size(16, 16)
fn dn_gf_coef(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    let mi = C[ic(p.x, p.y)];
    let mp = B[ib(p.x, p.y)];
    let a = (A[ia(p.x, p.y)] - mi * mp) / D[id(p.x, p.y)];
    O[io(p.x, p.y)] = a;
    O2[io2(p.x, p.y)] = mp - a * mi;
}

// The guided filter's output at full resolution: `up(A) * B + up(C)` (`A`, `C` the box means of
// the coefficients at half resolution, `B` the luma plane).
@compute @workgroup_size(16, 16)
fn dn_gf_apply(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    O[io(p.x, p.y)] = up_c(p.x, p.y) + up_a(p.x, p.y) * B[ib(p.x, p.y)];
}

// ---- DCT --------------------------------------------------------------------------------------

// The orthonormal 8-point DCT-II matrix entries the even/odd factorisation uses
// (`flashraw::denoise::Dct`, rounded to f32 the same way).
const M00: f32 = 0.353553385;
const M20: f32 = 0.461939752;
const M21: f32 = 0.191341713;
const M60: f32 = 0.191341713;
const M61: f32 = -0.461939752;
// Odd rows 1, 3, 5, 7, columns 0..3.
const M1: vec4<f32> = vec4<f32>(0.490392625, 0.415734798, 0.277785122, 0.0975451618);
const M3: vec4<f32> = vec4<f32>(0.415734798, -0.0975451618, -0.490392625, -0.277785122);
const M5: vec4<f32> = vec4<f32>(0.277785122, -0.490392625, 0.0975451618, 0.415734798);
const M7: vec4<f32> = vec4<f32>(0.0975451618, -0.277785122, 0.415734798, -0.490392625);

fn dot4(c: vec4<f32>, x: vec4<f32>) -> f32 {
    return (c.x * x.x + c.y * x.y) + (c.z * x.z + c.w * x.w);
}

// `out[k] = sum_i m[k][i] x[i]` (`Dct::fwd1`, the same operation order).
fn fdct(x: array<f32, 8>) -> array<f32, 8> {
    let s = vec4<f32>(x[0] + x[7], x[1] + x[6], x[2] + x[5], x[3] + x[4]);
    let d = vec4<f32>(x[0] - x[7], x[1] - x[6], x[2] - x[5], x[3] - x[4]);
    let e0 = s.x + s.w;
    let e1 = s.y + s.z;
    let e2 = s.x - s.w;
    let e3 = s.y - s.z;
    return array<f32, 8>(
        M00 * (e0 + e1), dot4(M1, d), M20 * e2 + M21 * e3, dot4(M3, d),
        M00 * (e0 - e1), dot4(M5, d), M60 * e2 + M61 * e3, dot4(M7, d),
    );
}

// `out[i] = sum_k m[k][i] x[k]` (`Dct::inv1`).
fn idct(x: array<f32, 8>) -> array<f32, 8> {
    let p = M00 * (x[0] + x[4]);
    let q = M00 * (x[0] - x[4]);
    let u = M20 * x[2] + M60 * x[6];
    let v = M21 * x[2] + M61 * x[6];
    let e = vec4<f32>(p + u, q + v, q - v, p - u);
    let xo = vec4<f32>(x[1], x[3], x[5], x[7]);
    let o = vec4<f32>(
        dot4(vec4<f32>(M1.x, M3.x, M5.x, M7.x), xo),
        dot4(vec4<f32>(M1.y, M3.y, M5.y, M7.y), xo),
        dot4(vec4<f32>(M1.z, M3.z, M5.z, M7.z), xo),
        dot4(vec4<f32>(M1.w, M3.w, M5.w, M7.w), xo),
    );
    return array<f32, 8>(e.x + o.x, e.y + o.y, e.z + o.z, e.w + o.w, e.w - o.w, e.z - o.z, e.y - o.y, e.x - o.x);
}

// Coefficients of a block given its rows (`b[i * 8 + j]`): entry `l * 8 + k` is horizontal
// frequency `l`, vertical frequency `k` (vertical transform first, like the CPU).
fn dct2(b: ptr<function, array<f32, 64>>) {
    var v: array<f32, 64>; // v[k * 8 + j]
    for (var j = 0u; j < 8u; j++) {
        let c = fdct(array<f32, 8>((*b)[j], (*b)[8u + j], (*b)[16u + j], (*b)[24u + j], (*b)[32u + j], (*b)[40u + j], (*b)[48u + j], (*b)[56u + j]));
        for (var k = 0u; k < 8u; k++) { v[k * 8u + j] = c[k]; }
    }
    for (var k = 0u; k < 8u; k++) {
        let r = fdct(array<f32, 8>(v[k * 8u], v[k * 8u + 1u], v[k * 8u + 2u], v[k * 8u + 3u], v[k * 8u + 4u], v[k * 8u + 5u], v[k * 8u + 6u], v[k * 8u + 7u]));
        for (var l = 0u; l < 8u; l++) { (*b)[l * 8u + k] = r[l]; }
    }
}

// Inverse of `dct2`: back to rows.
fn idct2(b: ptr<function, array<f32, 64>>) {
    var t: array<f32, 64>; // t[j * 8 + k]: column j, vertical frequency k
    for (var k = 0u; k < 8u; k++) {
        let r = idct(array<f32, 8>((*b)[k], (*b)[8u + k], (*b)[16u + k], (*b)[24u + k], (*b)[32u + k], (*b)[40u + k], (*b)[48u + k], (*b)[56u + k]));
        for (var j = 0u; j < 8u; j++) { t[j * 8u + k] = r[j]; }
    }
    for (var j = 0u; j < 8u; j++) {
        let c = idct(array<f32, 8>(t[j * 8u], t[j * 8u + 1u], t[j * 8u + 2u], t[j * 8u + 3u], t[j * 8u + 4u], t[j * 8u + 5u], t[j * 8u + 6u], t[j * 8u + 7u]));
        for (var i = 0u; i < 8u; i++) { (*b)[i * 8u + j] = c[i]; }
    }
}

// ---- Noise estimate ---------------------------------------------------------------------------

// The measured blocks of one plane level: block (bx, by) = (gx step, gy step) of the grid of 8x8
// blocks, `A` the plane, `B` the luma guide; per block 64 words into `O`: the guide mean, then the
// squared coefficients `c[k]^2` (k >= 1).
@compute @workgroup_size(64)
fn dn_stats(@builtin(global_invocation_id) g: vec3<u32>) {
    let gx = g.x;
    let gy = Q.sg0 + g.y;
    if (gx >= Q.scols || gy >= Q.rows1) { return; }
    let x0 = gx * Q.step * 8u;
    let y0 = gy * Q.step * 8u;
    var b: array<f32, 64>;
    var mean = 0.0;
    for (var i = 0u; i < 8u; i++) {
        for (var j = 0u; j < 8u; j++) {
            mean += B[ib(x0 + j, y0 + i)];
            b[i * 8u + j] = A[ia(x0 + j, y0 + i)];
        }
    }
    dct2(&b);
    let o = (gy * Q.scols + gx) * 64u;
    O[o] = mean / 64.0;
    for (var k = 1u; k < 64u; k++) { O[o + k] = b[k] * b[k]; }
}

// ---- DCT passes ------------------------------------------------------------------------------

// Source row or column of padded position `i` (symmetric padding of 8, `flashraw::denoise::pad_map`).
fn refl(i: i32, n: u32) -> u32 {
    var r = i - 8;
    if (r < 0) { r = -r - 1; }
    if (r >= i32(n)) { r = 2 * i32(n) - r - 1; }
    return u32(clamp(r, 0, i32(n) - 1));
}

fn pos_x(b: u32) -> u32 { return select(b << Q.sh, Q.last_x, b >= Q.nreg_x); }
fn pos_y(b: u32) -> u32 { return select(b << Q.sh, Q.last_y, b >= Q.nreg_y); }

// Bin index and interpolation weight of the noise model at luma mean `m`
// (`flashraw::denoise::Model::sigmas`): the deviations are `s[ia] + (s[ib] - s[ia]) t`.
fn model_at(m: f32) -> vec3<f32> {
    let n = u32(T[Q.off_model]);
    var i = 0u;
    for (; i < n; i++) {
        if (!(T[Q.off_model + 1u + i] < m)) { break; }
    }
    if (i == 0u || n == 1u) { return vec3<f32>(0.0, 0.0, 0.0); }
    if (i >= n) { return vec3<f32>(f32(n - 1u), f32(n - 1u), 0.0); }
    let a = T[Q.off_model + i];
    let b = T[Q.off_model + 1u + i];
    var t = 0.0;
    if (b > a) { t = (m - a) / (b - a); }
    return vec3<f32>(f32(i - 1u), f32(i), t);
}

// One DCT pass over one block row (mode 0: hard threshold of `A`; mode 1: empirical Wiener filter
// of `A` with the pilot `C`), `B` the luma guide, for the output columns [x0, x0 + SEG) of a
// workgroup. Like the CPU, the blocks of a row are summed per column before one vertical inverse
// transform per column: the result is 8 rows of weighted samples per block row into `O`
// (`((block row - i0) * 8 + i) * w + x`) and the weights into `O2` (`(block row - i0) * w + x`).
// Loops over the eight values of a transform are written out: Metal keeps the bounded loops wgpu
// emits, and an array indexed in a loop lives in slow memory.
const SEG: u32 = 48u;   // output columns per workgroup
const MAXB: u32 = 32u;  // blocks that cover them, at most ((SEG + 6) / 2 + 2)
const MAXC: u32 = 64u;  // padded columns those blocks read, at most (SEG + 14)
var<workgroup> ws: array<f32, 512>;   // 8 rows x MAXC: samples, then their vertical transform, then the column sums
var<workgroup> wp: array<f32, 512>;   // the pilot's
var<workgroup> wt: array<f32, 2048>;  // the guide's samples, then per block its 64 inverse-transformed values
var<workgroup> wred: array<f32, 256>; // per block and vertical frequency: kept coefficients / Wiener sums
var<workgroup> wwt: array<f32, 32>;   // per block: the weight
var<workgroup> wmd: array<vec3<f32>, 32>; // per block: the noise model at its luma mean

fn ld_s(o: u32, s: u32) -> array<f32, 8> {
    return array<f32, 8>(ws[o], ws[o + s], ws[o + 2u * s], ws[o + 3u * s], ws[o + 4u * s], ws[o + 5u * s], ws[o + 6u * s], ws[o + 7u * s]);
}
fn ld_p(o: u32, s: u32) -> array<f32, 8> {
    return array<f32, 8>(wp[o], wp[o + s], wp[o + 2u * s], wp[o + 3u * s], wp[o + 4u * s], wp[o + 5u * s], wp[o + 6u * s], wp[o + 7u * s]);
}
fn st_s(o: u32, s: u32, v: array<f32, 8>) {
    ws[o] = v[0]; ws[o + s] = v[1]; ws[o + 2u * s] = v[2]; ws[o + 3u * s] = v[3];
    ws[o + 4u * s] = v[4]; ws[o + 5u * s] = v[5]; ws[o + 6u * s] = v[6]; ws[o + 7u * s] = v[7];
}
fn st_p(o: u32, s: u32, v: array<f32, 8>) {
    wp[o] = v[0]; wp[o + s] = v[1]; wp[o + 2u * s] = v[2]; wp[o + 3u * s] = v[3];
    wp[o + 4u * s] = v[4]; wp[o + 5u * s] = v[5]; wp[o + 6u * s] = v[6]; wp[o + 7u * s] = v[7];
}
fn st_t(o: u32, v: array<f32, 8>) {
    wt[o] = v[0]; wt[o + 1u] = v[1]; wt[o + 2u] = v[2]; wt[o + 3u] = v[3];
    wt[o + 4u] = v[4]; wt[o + 5u] = v[5]; wt[o + 6u] = v[6]; wt[o + 7u] = v[7];
}
fn sum_t8(o: u32) -> f32 {
    return wt[o] + wt[o + 1u] + wt[o + 2u] + wt[o + 3u] + wt[o + 4u] + wt[o + 5u] + wt[o + 6u] + wt[o + 7u];
}
fn scale8(v: array<f32, 8>, s: f32) -> array<f32, 8> {
    return array<f32, 8>(v[0] * s, v[1] * s, v[2] * s, v[3] * s, v[4] * s, v[5] * s, v[6] * s, v[7] * s);
}

// Deviation of coefficient `q` at the block's model `mt` (`sa`, `sb`: its bins in `T`).
fn sigma(q: u32, sa: u32, sb: u32, t: f32) -> f32 {
    return T[sa + q] + (T[sb + q] - T[sa + q]) * t;
}

// Hard threshold of coefficient `c` (number `q`): the kept value and 1 if kept.
fn hard1(c: f32, q: u32, sa: u32, sb: u32, t: f32) -> vec2<f32> {
    let keep = abs(c) >= Q.kf0 * sigma(q, sa, sb, t);
    return vec2<f32>(select(0.0, c, keep), select(0.0, 1.0, keep));
}

// Empirical Wiener factor of coefficient `c` with the pilot's `p`: the filtered value and its
// residual variance.
fn wien1(c: f32, p: f32, q: u32, sa: u32, sb: u32, t: f32) -> vec2<f32> {
    let sg = sigma(q, sa, sb, t);
    let n2 = sg * sg * Q.kf1;
    let p2 = p * p;
    let s = p2 / (p2 + n2 + 1e-30);
    return vec2<f32>(c * s, s * s * n2);
}

@compute @workgroup_size(256)
fn dn_rows(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let x0 = wg.x * SEG;
    let bi = Q.i0 + wg.y;
    let oy = pos_y(bi);
    let wiener = Q.mode == 1u;
    // The blocks covering padded columns [x0 + 8, x0 + 8 + SEG), and the columns they read.
    let px0 = x0 + 8u;
    let bj0 = first_from(px0 - 7u);
    let bj1 = end_at(px0 + SEG - 1u);
    let nb = bj1 - bj0;
    let c0 = pos_x(bj0);
    let ncols = pos_x(bj1 - 1u) + 8u - c0;
    // Load the samples of the 8 rows (and the pilot's, and the guide's).
    for (var i = li; i < 8u * MAXC; i += 256u) {
        let r = i / MAXC;
        let c = i % MAXC;
        if (c < ncols) {
            let x = refl(i32(c0 + c), Q.w);
            let y = refl(i32(oy + r), Q.h);
            ws[i] = A[ia(x, y)];
            wt[i] = B[ib(x, y)];
            if (wiener) { wp[i] = C[ic(x, y)]; }
        }
    }
    workgroupBarrier();
    // Each block's luma mean and noise model.
    if (li < nb) {
        let o = pos_x(bj0 + li) - c0;
        let m = ((sum_t8(o) + sum_t8(MAXC + o)) + (sum_t8(2u * MAXC + o) + sum_t8(3u * MAXC + o)))
            + ((sum_t8(4u * MAXC + o) + sum_t8(5u * MAXC + o)) + (sum_t8(6u * MAXC + o) + sum_t8(7u * MAXC + o)));
        wmd[li] = model_at(m / 64.0);
    }
    // Vertical transform of every column (sample r * MAXC + c -> frequency k * MAXC + c).
    if (li < ncols) {
        st_s(li, MAXC, fdct(ld_s(li, MAXC)));
    } else if (wiener && li >= MAXC && li - MAXC < ncols) {
        st_p(li - MAXC, MAXC, fdct(ld_p(li - MAXC, MAXC)));
    }
    workgroupBarrier();
    // Per block and vertical frequency k: the horizontal transform, the threshold or the
    // Wiener factor (coefficient l * 8 + k).
    let b = li / 8u;
    let k = li % 8u;
    let live = b < nb;
    var c: array<f32, 8>;
    if (live) {
        let o = k * MAXC + pos_x(bj0 + b) - c0;
        let f = fdct(ld_s(o, 1u));
        let mt = wmd[b];
        let sa = Q.off_model + 16u + u32(mt.x) * 64u;
        let sb = Q.off_model + 16u + u32(mt.y) * 64u;
        let t = mt.z;
        var r0: vec2<f32>;
        var r1: vec2<f32>;
        var r2: vec2<f32>;
        var r3: vec2<f32>;
        var r4: vec2<f32>;
        var r5: vec2<f32>;
        var r6: vec2<f32>;
        var r7: vec2<f32>;
        if (wiener) {
            let p = fdct(ld_p(o, 1u));
            r0 = wien1(f[0], p[0], k, sa, sb, t);
            r1 = wien1(f[1], p[1], 8u + k, sa, sb, t);
            r2 = wien1(f[2], p[2], 16u + k, sa, sb, t);
            r3 = wien1(f[3], p[3], 24u + k, sa, sb, t);
            r4 = wien1(f[4], p[4], 32u + k, sa, sb, t);
            r5 = wien1(f[5], p[5], 40u + k, sa, sb, t);
            r6 = wien1(f[6], p[6], 48u + k, sa, sb, t);
            r7 = wien1(f[7], p[7], 56u + k, sa, sb, t);
        } else {
            r0 = hard1(f[0], k, sa, sb, t);
            r1 = hard1(f[1], 8u + k, sa, sb, t);
            r2 = hard1(f[2], 16u + k, sa, sb, t);
            r3 = hard1(f[3], 24u + k, sa, sb, t);
            r4 = hard1(f[4], 32u + k, sa, sb, t);
            r5 = hard1(f[5], 40u + k, sa, sb, t);
            r6 = hard1(f[6], 48u + k, sa, sb, t);
            r7 = hard1(f[7], 56u + k, sa, sb, t);
        }
        // The DC coefficient is kept (and not counted).
        if (k == 0u) { r0 = vec2<f32>(f[0], 0.0); }
        c = array<f32, 8>(r0.x, r1.x, r2.x, r3.x, r4.x, r5.x, r6.x, r7.x);
        wred[li] = ((r0.y + r1.y) + (r2.y + r3.y)) + ((r4.y + r5.y) + (r6.y + r7.y));
    }
    workgroupBarrier();
    if (live && k == 0u) {
        let a = ((wred[li] + wred[li + 1u]) + (wred[li + 2u] + wred[li + 3u]))
            + ((wred[li + 4u] + wred[li + 5u]) + (wred[li + 6u] + wred[li + 7u]));
        // Hard: 1 / kept coefficients (with the DC); Wiener: inverse of the residual variance.
        wwt[b] = select(1.0 / (a + 1.0), 1.0 / (a + 1e-20), wiener);
    }
    workgroupBarrier();
    // Inverse horizontal transform: column j, vertical frequency k of block b.
    if (live) {
        st_t(b * 64u + k * 8u, idct(scale8(c, wwt[b])));
    }
    workgroupBarrier();
    // Sum the blocks per output column and frequency (blocks in order, like the CPU).
    for (var i = li; i < 8u * SEG; i += 256u) {
        let kk = i / SEG;
        let x = i % SEG;
        let p = px0 + x;
        let cv = cover(p, Q.nreg_x, Q.last_x, Q.nbx);
        var s = 0.0;
        for (var bj = max(cv.x, bj0); bj <= cv.y && bj < bj1; bj++) {
            s += wt[(bj - bj0) * 64u + kk * 8u + p - pos_x(bj)];
        }
        ws[kk * MAXC + x] = s;
    }
    workgroupBarrier();
    // Vertical inverse per column: 8 rows of weighted samples, and the column's weight.
    if (li < SEG && x0 + li < Q.w) {
        let x = x0 + li;
        let v = idct(ld_s(li, MAXC));
        let w = Q.w;
        let o = (bi - Q.i0) * 8u * w + x;
        O[o] = v[0];
        O[o + w] = v[1];
        O[o + 2u * w] = v[2];
        O[o + 3u * w] = v[3];
        O[o + 4u * w] = v[4];
        O[o + 5u * w] = v[5];
        O[o + 6u * w] = v[6];
        O[o + 7u * w] = v[7];
        let p = px0 + li;
        let cv = cover(p, Q.nreg_x, Q.last_x, Q.nbx);
        var s = 0.0;
        for (var bj = max(cv.x, bj0); bj <= cv.y && bj < bj1; bj++) { s += wwt[bj - bj0]; }
        O2[(bi - Q.i0) * w + x] = s;
    }
}

// First block at or after padded column `p` (`Grid::first_from`).
fn first_from(p: u32) -> u32 {
    let b = (p + Q.step - 1u) >> Q.sh;
    if (b < Q.nreg_x) { return b; }
    if (Q.nbx > Q.nreg_x && Q.last_x >= p) { return Q.nreg_x; }
    return Q.nbx;
}

// One past the last block at or before padded column `q` (`Grid::end_at`).
fn end_at(q: u32) -> u32 {
    if (Q.nbx > Q.nreg_x && Q.last_x <= q) { return Q.nbx; }
    return min(q >> Q.sh, Q.nreg_x - 1u) + 1u;
}

// First and last block (inclusive; last < first: none) covering padded position `q` of a side.
fn cover(q: u32, nreg: u32, last: u32, nb: u32) -> vec2<u32> {
    let lo = max(i32(q) - 7, 0);
    var first = (u32(lo) + Q.step - 1u) >> Q.sh;
    var hi = min(q >> Q.sh, nreg - 1u);
    if (nb > nreg && last <= q && last + 7u >= q) {
        hi = nreg;
        if (first > nreg - 1u) { first = nreg; }
    }
    return vec2<u32>(first, hi);
}

// The samples of the pass: the sum over the block rows covering each sample of their weighted
// samples (`A`) and weights (`B`) from `dn_rows`, divided; the source `C` where no block covers it.
@compute @workgroup_size(16, 16)
fn dn_gather(@builtin(global_invocation_id) g: vec3<u32>) {
    let p = pixel(g);
    if (!inside(p)) { return; }
    let py = p.y + 8u;
    let cy = cover(py, Q.nreg_y, Q.last_y, Q.nreg_y + select(0u, 1u, (Q.last_y & (Q.step - 1u)) != 0u));
    var v = 0.0;
    var ws = 0.0;
    for (var bi = max(cy.x, Q.i0); bi <= cy.y && bi < Q.i1; bi++) {
        let r = bi - Q.i0;
        v += A[(r * 8u + py - pos_y(bi)) * Q.w + p.x];
        ws += B[r * Q.w + p.x];
    }
    var out = C[ic(p.x, p.y)];
    if (ws > 0.0) { out = v / ws; }
    O[io(p.x, p.y)] = out;
}
