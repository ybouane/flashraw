// Edge-aware demosaic (CPU reference: `flashraw::edgeaware`, `Demosaic::EdgeAware`) for Bayer
// and X-Trans mosaics. This module is `common.wgsl` + `develop.wgsl` + this file: the final
// kernel reuses `finish_balanced`, `Taps` and the CFA site tables.
//
// Several dispatches per band of developed rows, with intermediates in one scratch buffer
// (`S`) laid out as planes of `E.n` floats over the rectangle E (frame coordinates
// `ex0.., ey0..`, `ew` x `eh`; it holds the interior pixels of the band plus `halo` pixels on
// every side):
//
//   plane 0     M   white balanced mosaic, clipped at 1                (edge_mosaic)
//   plane 1     G0  sqrt of the first green estimate                   (edge_g0)
//   planes 2-4  RGB the directionally weighted 7x7 filters             (edge_rgb)
//   planes 0-1  D1  colour differences R-G, B-G, pass 1                (edge_bayer_d1 / edge_xtrans_d1; reuses M, G0)
//   planes 5-7  V2  cleaned RGB, pass 2                                (edge_bayer_v2, both sensors)
//
// The AHD demosaic (section below) uses the same layout with more planes and the same final
// kernel.
// then `develop_edge` smooths (noisy frames), applies the matrix and encodes. Pixels closer than
// `halo` to the frame border are bilinear, like the CPU. The order of the floating-point
// operations follows the CPU code; only FMA contraction and division rounding differ.

struct EdgeParams {
    period: u32,    // 2 (Bayer) or 6 (X-Trans)
    cx: u32,        // canonical CFA column / row of frame column / row 0
    cy: u32,
    off_g0: u32,    // offsets into `ftab`: G0 filters, directional filters (canonical colours first)
    off_dir: u32,
    ex0: u32,       // rectangle E in frame coordinates
    ey0: u32,
    ew: u32,
    eh: u32,
    n: u32,         // ew * eh (0: the band has no interior pixel)
    halo: u32,      // pixels from the frame border that are bilinear
    smooth_on: u32, // 1: gated smoothing of the output
    beta: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
    wb: vec4<f32>,  // balance of the mosaic per colour
}

@group(1) @binding(0) var<uniform> E: EdgeParams;
// Canonical colours (0 R, 1 G, 2 B) of the period x period pattern, then the filters
// (the f32 tables of `crates/flashraw/src/edgeaware`; `off_g0` and `off_dir` are multiples of 4).
@group(1) @binding(1) var<storage, read> ftab: array<f32>;
// The same buffer as vec4s, for the directional filters: per position and colour, per tap, the
// coefficients of the four directions (the host transposes them).
@group(1) @binding(3) var<storage, read> ftab4: array<vec4<f32>>;
@group(1) @binding(2) var<storage, read_write> S: array<f32>;

const EDGE_EPS: f32 = 1e-3;
const FRAC_1_SQRT_2: f32 = 0.70710678118654752440;

var<workgroup> tm: array<f32, 484>; // 22 x 22: mosaic with a 3-pixel apron
var<workgroup> tg: array<f32, 400>; // 20 x 20: sqrt(G0) with a 2-pixel apron

fn e_inside(i: i32, j: i32) -> bool {
    return i >= 0 && j >= 0 && u32(i) < E.eh && u32(j) < E.ew;
}

fn e_idx(i: i32, j: i32) -> u32 {
    return u32(i) * E.ew + u32(j);
}

// Colour (0 R, 1 G, 2 B) of the sample at E-local (i, j).
fn e_colour(i: i32, j: i32) -> u32 {
    let p = E.period;
    let x = E.ex0 + u32(j);
    let y = E.ey0 + u32(i);
    return u32(ftab[((y + E.cy) % p) * p + (x + E.cx) % p]);
}

// Plane `k` of the scratch buffer at pixel index `idx`.
fn sp(k: u32, idx: u32) -> f32 {
    return S[k * E.n + idx];
}

// ---- 1. White balanced mosaic -------------------------------------------------------------

@compute @workgroup_size(16, 16)
fn edge_mosaic(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= E.ew || gid.y >= E.eh) {
        return;
    }
    let c = e_colour(i32(gid.y), i32(gid.x));
    let v = sample_norm(E.ex0 + gid.x, E.ey0 + gid.y, 0u);
    // Clipped samples end up at the white level in every channel.
    S[gid.y * E.ew + gid.x] = min(v * E.wb[c], 1.0);
}

// ---- 2. sqrt(G0) ---------------------------------------------------------------------------

@compute @workgroup_size(16, 16)
fn edge_g0(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) l: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let i0 = i32(wg.y * 16u) - 3;
    let j0 = i32(wg.x * 16u) - 3;
    for (var t = li; t < 484u; t += 256u) {
        let ii = i0 + i32(t / 22u);
        let jj = j0 + i32(t % 22u);
        var v = 0.0;
        if (e_inside(ii, jj)) {
            v = S[e_idx(ii, jj)];
        }
        tm[t] = v;
    }
    workgroupBarrier();
    let i = wg.y * 16u + l.y;
    let j = wg.x * 16u + l.x;
    if (i >= E.eh || j >= E.ew) {
        return;
    }
    let p = E.period;
    let cr = (E.ey0 + i + E.cy) % p;
    let q = (E.ex0 + j + E.cx) % p;
    let base = E.off_g0 + (cr * p + q) * 49u;
    var acc = 0.0;
    for (var ty = 0u; ty < 7u; ty++) {
        for (var tx = 0u; tx < 7u; tx++) {
            acc += ftab[base + ty * 7u + tx] * tm[(l.y + ty) * 22u + l.x + tx];
        }
    }
    S[E.n + i * E.ew + j] = sqrt(max(acc, 0.0));
}

// ---- 3. Direction weights and the directional filters -------------------------------------

fn gtl(l: vec3<u32>, a: i32, b: i32) -> f32 {
    return tg[u32(i32(l.y) + 2 + a) * 20u + u32(i32(l.x) + 2 + b)];
}

// Squared derivatives of sqrt(G0) at the pixel offset (a, b) along the four directions.
fn dsq4(l: vec3<u32>, a: i32, b: i32) -> vec4<f32> {
    let dh = gtl(l, a, b + 1) - gtl(l, a, b - 1);
    let dv = gtl(l, a + 1, b) - gtl(l, a - 1, b);
    let d1 = (gtl(l, a + 1, b + 1) - gtl(l, a - 1, b - 1)) * FRAC_1_SQRT_2;
    let d2 = (gtl(l, a + 1, b - 1) - gtl(l, a - 1, b + 1)) * FRAC_1_SQRT_2;
    return vec4<f32>(dh * dh, dv * dv, d1 * d1, d2 * d2);
}

@compute @workgroup_size(16, 16)
fn edge_rgb(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) l: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
) {
    let i0 = i32(wg.y * 16u);
    let j0 = i32(wg.x * 16u);
    for (var t = li; t < 484u; t += 256u) {
        let ii = i0 - 3 + i32(t / 22u);
        let jj = j0 - 3 + i32(t % 22u);
        var v = 0.0;
        if (e_inside(ii, jj)) {
            v = S[e_idx(ii, jj)];
        }
        tm[t] = v;
    }
    for (var t = li; t < 400u; t += 256u) {
        let ii = i0 - 2 + i32(t / 20u);
        let jj = j0 - 2 + i32(t % 20u);
        var v = 0.0;
        if (e_inside(ii, jj)) {
            v = S[E.n + e_idx(ii, jj)];
        }
        tg[t] = v;
    }
    workgroupBarrier();
    let i = wg.y * 16u + l.y;
    let j = wg.x * 16u + l.x;
    if (i >= E.eh || j >= E.ew) {
        return;
    }
    // Weights: per direction, the sum over 3x3 of the squared derivative, w = 1 / (E + eps)^2,
    // normalised.
    var hs: array<vec4<f32>, 3>;
    for (var a = 0; a < 3; a++) {
        var s = vec4<f32>(0.0);
        for (var b = 0; b < 3; b++) {
            s = s + dsq4(l, a - 1, b - 1);
        }
        hs[a] = s;
    }
    let u = ((hs[0] + hs[1]) + hs[2]) + vec4<f32>(EDGE_EPS);
    let wd = vec4<f32>(1.0) / (u * u);
    let inv = 1.0 / (((wd.x + wd.y) + wd.z) + wd.w);
    let wn = wd * inv;

    let p = E.period;
    let cr = (E.ey0 + i + E.cy) % p;
    let q = (E.ex0 + j + E.cx) % p;
    let own = u32(ftab[cr * p + q]);
    let m0 = select(0u, 1u, own == 0u);
    let m1 = select(2u, 1u, own == 2u);
    let b0 = E.off_dir / 4u + ((cr * p + q) * 3u + m0) * 49u;
    let b1 = E.off_dir / 4u + ((cr * p + q) * 3u + m1) * 49u;
    var a0 = vec4<f32>(0.0);
    var a1 = vec4<f32>(0.0);
    for (var ty = 0u; ty < 7u; ty++) {
        for (var tx = 0u; tx < 7u; tx++) {
            let tap = ty * 7u + tx;
            let v = tm[(l.y + ty) * 22u + l.x + tx];
            a0 += ftab4[b0 + tap] * v;
            a1 += ftab4[b1 + tap] * v;
        }
    }
    let idx = i * E.ew + j;
    S[(2u + m0) * E.n + idx] = wn.x * a0.x + wn.y * a0.y + wn.z * a0.z + wn.w * a0.w;
    S[(2u + m1) * E.n + idx] = wn.x * a1.x + wn.y * a1.y + wn.z * a1.z + wn.w * a1.w;
    S[(2u + own) * E.n + idx] = tm[(l.y + 3u) * 22u + l.x + 3u];
}

// ---- 4. Chroma: two passes ----------------------------------------------------------------

// Pass 1, Bayer: green plus the gated mean of the measured differences of the 3x3 window.
@compute @workgroup_size(16, 16)
fn edge_bayer_d1(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= E.ew || gid.y >= E.eh) {
        return;
    }
    let i = i32(gid.y);
    let j = i32(gid.x);
    let idx = e_idx(i, j);
    let own = e_colour(i, j);
    let gc = sp(3u, idx);
    let inv = 1.0 / (0.2 * max(gc, 0.0) + 0.015);
    for (var q = 0u; q < 2u; q++) {
        let ch = q * 2u;
        var d = 0.0;
        if (own == ch) {
            d = sp(2u + ch, idx) - gc;
        } else {
            var sw = 0.0;
            var sd = 0.0;
            var su = 0.0;
            var cnt = 0.0;
            for (var dy = -1; dy <= 1; dy++) {
                for (var dx = -1; dx <= 1; dx++) {
                    let ii = i + dy;
                    let jj = j + dx;
                    if (!e_inside(ii, jj) || e_colour(ii, jj) != ch) {
                        continue;
                    }
                    let k = e_idx(ii, jj);
                    let gk = sp(3u, k);
                    let dd = sp(2u + ch, k) - gk;
                    let e = (gk - gc) * inv;
                    let wt = 1.0 / (1.0 + e * e);
                    sw += wt;
                    sd += wt * dd;
                    su += dd;
                    cnt += 1.0;
                }
            }
            if (sw > 1e-12) {
                d = sd / sw;
            } else if (cnt > 0.0) {
                d = su / cnt;
            }
        }
        S[q * E.n + idx] = d;
    }
}

// Pass 2, Bayer and X-Trans: the same gate over the pass-1 differences of every pixel, for the
// two colours the pixel does not measure.
@compute @workgroup_size(16, 16)
fn edge_bayer_v2(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= E.ew || gid.y >= E.eh) {
        return;
    }
    let i = i32(gid.y);
    let j = i32(gid.x);
    let idx = e_idx(i, j);
    let own = e_colour(i, j);
    let gc = sp(3u, idx);
    let inv = 1.0 / (0.1 * max(gc, 0.0) + 0.007);
    // The gated means of the pass-1 differences R-G (d.x) and B-G (d.y).
    var sw = 0.0;
    var sd = vec2<f32>(0.0);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let ii = i + dy;
            let jj = j + dx;
            if (!e_inside(ii, jj)) {
                continue;
            }
            let k = e_idx(ii, jj);
            let e = (sp(3u, k) - gc) * inv;
            let wt = 1.0 / (1.0 + e * e);
            sw += wt;
            sd = sd + wt * vec2<f32>(sp(0u, k), sp(1u, k));
        }
    }
    let d = sd / sw;
    // X-Trans: green at a red or blue site moves towards the measured sample minus the cleaned
    // difference, fully on clean frames and less on noisy ones (CPU: `develop_chroma`).
    var g = gc;
    if (E.period == 6u) {
        let xg = clamp(1.0 - 2.0 * E.beta, 0.0, 1.0);
        if (own == 0u) {
            g = gc + xg * ((sp(2u, idx) - d.x) - gc);
        } else if (own == 2u) {
            g = gc + xg * ((sp(4u, idx) - d.y) - gc);
        }
    }
    var v = vec3<f32>(g + d.x, g, g + d.y);
    if (own == 0u) {
        v.x = sp(2u, idx);
    }
    if (own == 2u) {
        v.z = sp(4u, idx);
    }
    S[5u * E.n + idx] = v.x;
    S[6u * E.n + idx] = v.y;
    S[7u * E.n + idx] = v.z;
}

// ---- 5. X-Trans chroma pass 1: 3x3 median of the colour differences -----------------------

fn cas(p: ptr<function, array<f32, 9>>, a: i32, b: i32) {
    let lo = min((*p)[a], (*p)[b]);
    let hi = max((*p)[a], (*p)[b]);
    (*p)[a] = lo;
    (*p)[b] = hi;
}

// The comparison network of the CPU `median9`.
fn median9(v: array<f32, 9>) -> f32 {
    var p = v;
    cas(&p, 1, 2);
    cas(&p, 4, 5);
    cas(&p, 7, 8);
    cas(&p, 0, 1);
    cas(&p, 3, 4);
    cas(&p, 6, 7);
    cas(&p, 1, 2);
    cas(&p, 4, 5);
    cas(&p, 7, 8);
    cas(&p, 0, 3);
    cas(&p, 5, 8);
    cas(&p, 4, 7);
    cas(&p, 3, 6);
    cas(&p, 1, 4);
    cas(&p, 2, 5);
    cas(&p, 4, 7);
    cas(&p, 4, 2);
    cas(&p, 6, 4);
    cas(&p, 4, 2);
    return p[4];
}

// Pass 1, X-Trans: the measured difference, or the 3x3 median of the differences of the filtered
// planes; pass 2 is `edge_bayer_v2`.
@compute @workgroup_size(16, 16)
fn edge_xtrans_d1(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= E.ew || gid.y >= E.eh) {
        return;
    }
    let i = i32(gid.y);
    let j = i32(gid.x);
    let idx = e_idx(i, j);
    let own = e_colour(i, j);
    var rg: array<f32, 9>;
    var bg: array<f32, 9>;
    for (var k = 0; k < 9; k++) {
        let ii = i + k / 3 - 1;
        let jj = j + k % 3 - 1;
        var a = 0.0;
        var b = 0.0;
        if (e_inside(ii, jj)) {
            let n = e_idx(ii, jj);
            a = sp(2u, n) - sp(3u, n);
            b = sp(4u, n) - sp(3u, n);
        }
        rg[k] = a;
        bg[k] = b;
    }
    let g = sp(3u, idx);
    S[idx] = select(median9(rg), sp(2u, idx) - g, own == 0u);
    S[E.n + idx] = select(median9(bg), sp(4u, idx) - g, own == 2u);
}

// ---- AHD (CPU reference: `flashraw::ahd`, `Demosaic::Ahd`) on Bayer mosaics ---------------
//
// After `edge_mosaic` (plane 0, M), with `halo` = 6:
//
//   planes 1-2   GH, GV  green along rows / along columns                 (ahd_green)
//   planes 3-5   L a b of candidate H, planes 6-8 of candidate V          (ahd_lab)
//   planes 9-10  homogeneity counts of H and V                             (ahd_hom)
//   planes 5-7   the blended RGB, read by `develop_edge` (smoothing off)   (ahd_blend)
//
// The same operations in the same order as the CPU code; samples outside the scratch rectangle
// read as 0 and only affect pixels that are never output.

fn ahd_at(k: u32, i: i32, j: i32) -> f32 {
    if (e_inside(i, j)) {
        return S[k * E.n + e_idx(i, j)];
    }
    return 0.0;
}

@compute @workgroup_size(16, 16)
fn ahd_green(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= E.ew || gid.y >= E.eh) {
        return;
    }
    let i = i32(gid.y);
    let j = i32(gid.x);
    let idx = e_idx(i, j);
    let c = S[idx];
    var gh = c;
    var gv = c;
    if (e_colour(i, j) != 1u) {
        let l = ahd_at(0u, i, j - 1);
        let r = ahd_at(0u, i, j + 1);
        let u = ahd_at(0u, i - 1, j);
        let d = ahd_at(0u, i + 1, j);
        gh = min(max((l + r) * 0.5 + (2.0 * c - ahd_at(0u, i, j - 2) - ahd_at(0u, i, j + 2)) * 0.25, min(l, r)), max(l, r));
        gv = min(max((u + d) * 0.5 + (2.0 * c - ahd_at(0u, i - 2, j) - ahd_at(0u, i + 2, j)) * 0.25, min(u, d)), max(u, d));
    }
    S[E.n + idx] = gh;
    S[2u * E.n + idx] = gv;
}

// Colour difference (mosaic minus the green plane `gp`) at (i, j).
fn ahd_df(gp: u32, i: i32, j: i32) -> f32 {
    return ahd_at(0u, i, j) - ahd_at(gp, i, j);
}

// Candidate RGB at (i, j) from the green plane `gp` (1: H, 2: V): measured colours kept, the
// others from the colour differences of the 3x3 window.
fn ahd_rgb(i: i32, j: i32, gp: u32) -> vec3<f32> {
    let c = ahd_at(0u, i, j);
    let g = ahd_at(gp, i, j);
    let col = e_colour(i, j);
    let diag = g + 0.25 * (((ahd_df(gp, i - 1, j - 1) + ahd_df(gp, i - 1, j + 1)) + ahd_df(gp, i + 1, j - 1)) + ahd_df(gp, i + 1, j + 1));
    let row = g + 0.5 * (ahd_df(gp, i, j - 1) + ahd_df(gp, i, j + 1));
    let clm = g + 0.5 * (ahd_df(gp, i - 1, j) + ahd_df(gp, i + 1, j));
    // A green site with red neighbours along its row.
    let hr = col == 1u && e_colour(i, j + 1) == 0u;
    var red = diag;
    var blue = diag;
    if (col == 0u) {
        red = c;
    } else if (col == 1u) {
        red = select(clm, row, hr);
    }
    if (col == 2u) {
        blue = c;
    } else if (col == 1u) {
        blue = select(row, clm, hr);
    }
    return vec3<f32>(red, g, blue);
}

@compute @workgroup_size(16, 16)
fn ahd_lab(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= E.ew || gid.y >= E.eh) {
        return;
    }
    let i = i32(gid.y);
    let j = i32(gid.x);
    let idx = e_idx(i, j);
    for (var d = 0u; d < 2u; d++) {
        let v = ahd_rgb(i, j, 1u + d);
        let sr = sqrt(max(v.x, 0.0));
        let sg = sqrt(max(v.y, 0.0));
        let sb = sqrt(max(v.z, 0.0));
        S[(3u + 3u * d) * E.n + idx] = ((sr + 2.0 * sg) + sb) * 0.25;
        S[(4u + 3u * d) * E.n + idx] = sr - sg;
        S[(5u + 3u * d) * E.n + idx] = sb - sg;
    }
}

// Lightness step and squared chroma distance from (i, j) to (i + di, j + dj) in candidate `d`.
fn ahd_step(d: u32, i: i32, j: i32, di: i32, dj: i32) -> vec2<f32> {
    let b = 3u + 3u * d;
    let l = ahd_at(b, i, j);
    let a = ahd_at(b + 1u, i, j);
    let c = ahd_at(b + 2u, i, j);
    let ql = ahd_at(b, i + di, j + dj);
    let qa = ahd_at(b + 1u, i + di, j + dj);
    let qc = ahd_at(b + 2u, i + di, j + dj);
    return vec2<f32>(abs(ql - l), (qa - a) * (qa - a) + (qc - c) * (qc - c));
}

@compute @workgroup_size(16, 16)
fn ahd_hom(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= E.ew || gid.y >= E.eh) {
        return;
    }
    let i = i32(gid.y);
    let j = i32(gid.x);
    let idx = e_idx(i, j);
    // Thresholds: the smaller of the largest steps along the row in H and along the column in V.
    let h0 = ahd_step(0u, i, j, 0, -1);
    let h1 = ahd_step(0u, i, j, 0, 1);
    let v0 = ahd_step(1u, i, j, -1, 0);
    let v1 = ahd_step(1u, i, j, 1, 0);
    let el = min(max(h0.x, h1.x), max(v0.x, v1.x));
    let ec = min(max(h0.y, h1.y), max(v0.y, v1.y));
    for (var d = 0u; d < 2u; d++) {
        var cnt = 0.0;
        for (var di = -1; di <= 1; di++) {
            for (var dj = -1; dj <= 1; dj++) {
                if (di == 0 && dj == 0) {
                    continue;
                }
                let s = ahd_step(d, i, j, di, dj);
                if (s.x <= el && s.y <= ec) {
                    cnt += 1.0;
                }
            }
        }
        S[(9u + d) * E.n + idx] = cnt;
    }
}

@compute @workgroup_size(16, 16)
fn ahd_blend(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= E.ew || gid.y >= E.eh) {
        return;
    }
    let i = i32(gid.y);
    let j = i32(gid.x);
    let idx = e_idx(i, j);
    // Homogeneity summed over the 3x3 window (small integers: exact in any order).
    var sh = 0.0;
    var sv = 0.0;
    for (var di = -1; di <= 1; di++) {
        for (var dj = -1; dj <= 1; dj++) {
            sh += ahd_at(9u, i + di, j + dj);
            sv += ahd_at(10u, i + di, j + dj);
        }
    }
    var w = 0.5;
    if (sh + sv > 0.0) {
        w = sh / (sh + sv);
    }
    let a = ahd_rgb(i, j, 1u);
    let b = ahd_rgb(i, j, 2u);
    S[5u * E.n + idx] = w * a.x + (1.0 - w) * b.x;
    S[6u * E.n + idx] = w * a.y + (1.0 - w) * b.y;
    S[7u * E.n + idx] = w * a.z + (1.0 - w) * b.z;
}

// ---- 6. Smoothing, border pixels, colour conversion, encoding ------------------------------

fn v2px(idx: u32) -> vec3<f32> {
    return vec3<f32>(S[5u * E.n + idx], S[6u * E.n + idx], S[7u * E.n + idx]);
}

// The cleaned pixel at frame (x, y), moved by `beta` towards the gated mean of its 3x3 window
// on noisy frames.
fn edge_pixel(x: u32, y: u32) -> vec3<f32> {
    let i = y - E.ey0;
    let j = x - E.ex0;
    let p = v2px(i * E.ew + j);
    if (E.smooth_on == 0u) {
        return p;
    }
    let inv = 1.0 / (0.2 * max(p.y, 0.0) + 0.015);
    var sw = 0.0;
    var s = vec3<f32>(0.0);
    for (var dy = 0u; dy < 3u; dy++) {
        for (var dx = 0u; dx < 3u; dx++) {
            let q = v2px((i + dy - 1u) * E.ew + j + dx - 1u);
            let e = (q.y - p.y) * inv;
            let wt = 1.0 / (1.0 + e * e);
            sw += wt;
            s += wt * q;
        }
    }
    return p + E.beta * (s / sw - p);
}

// Tap k of the 3x3 neighbourhood read straight from the source (borders are few pixels).
fn tap_direct(acc: ptr<function, Taps>, site: u32, k: u32, x: u32, y: u32) {
    let ky = k / 3u;
    let kx = k - ky * 3u;
    let xx = i32(x + kx) - 1;
    let yy = i32(y + ky) - 1;
    if (xx < 0 || yy < 0 || u32(xx) >= P.width || u32(yy) >= P.height) {
        return;
    }
    let sel = vec3<f32>(vec3<u32>((site >> (2u + k)) & 1u, (site >> (11u + k)) & 1u, (site >> (20u + k)) & 1u));
    if (sel.x + sel.y + sel.z == 0.0) {
        return;
    }
    let v = sample_norm(u32(xx), u32(yy), 0u);
    (*acc).s += v * sel;
    (*acc).n += sel;
}

// Bilinear demosaic of the unbalanced mosaic at frame (x, y), like `develop` (balanced by the
// caller).
fn bilinear_px(x: u32, y: u32) -> vec3<f32> {
    let site = cfa_site(x, y);
    let interior = x > 0u && y > 0u && x + 1u < P.width && y + 1u < P.height;
    var acc = Taps(vec3<f32>(0.0), vec3<f32>(0.0));
    if (E.period == 2u) {
        // Bayer: the CPU fast-path order.
        tap_direct(&acc, site, 3u, x, y);
        tap_direct(&acc, site, 5u, x, y);
        tap_direct(&acc, site, 1u, x, y);
        tap_direct(&acc, site, 7u, x, y);
        tap_direct(&acc, site, 0u, x, y);
        tap_direct(&acc, site, 2u, x, y);
        tap_direct(&acc, site, 6u, x, y);
        tap_direct(&acc, site, 8u, x, y);
        tap_direct(&acc, site, 4u, x, y);
    } else {
        for (var k = 0u; k < 9u; k++) {
            tap_direct(&acc, site, k, x, y);
        }
    }
    var rgb = vec3<f32>(0.0);
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
    return rgb;
}

@compute @workgroup_size(16, 16)
fn develop_edge(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dx = gid.x;
    let dy = P.band_y + gid.y;
    if (dx >= P.dev_w || dy >= P.band_end) {
        return;
    }
    let x = P.crop_x + dx;
    let y = P.crop_y + dy;
    let h = E.halo;
    if (x >= h && y >= h && x + h < P.width && y + h < P.height) {
        finish_balanced(edge_pixel(x, y), dx, dy);
    } else {
        finish(bilinear_px(x, y), dx, dy);
    }
}
