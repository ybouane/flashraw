#!/usr/bin/env python3
"""Train the coefficient tables of the edge-aware demosaic (`Demosaic::EdgeAware`).

The demosaic is a directionally weighted set of linear filters, one set per position of the
colour filter array (2x2 Bayer, 6x6 X-Trans). For every pixel
  1. a first luminance-like plane G0 is estimated with a non-directional filter,
  2. the squared gradients of sqrt(G0) along four directions (horizontal, vertical and the
     two diagonals), summed over 3x3, give four weights w_d = 1 / (E_d + EPS)^2 (normalised),
  3. each missing colour is sum_d w_d * (7x7 filter_d applied to the mosaic),
  4. missing R and B = G + a cleaned R-G / B-G, in two passes: pass 1 the luminance-gated mean of the measured
     differences of the 3x3 window (Bayer) or the 3x3 median of the filtered differences (X-Trans), pass 2 the
     luminance-gated mean of the pass-1 differences of the window; measured samples kept (not trained, see
     `chroma_stage`).
The filters are the least-squares solution (ridge regularised) that reconstructs ground truth
images from their re-mosaicked version, with the weights held fixed, so everything stays one
linear problem. No third-party code or tables are involved: the training images are
  * half-size renders (2x2 binning, so every pixel has all three colours measured) of CC0
    sample files (raw.pixls.us, `tools/corpus/fetch.py`), cropped to 384x384 tiles, and
  * synthetic images with hard edges, thin lines and gradients, generated here.
Usage:
  edgeaware_train.py gt     --corpus DIR --flashraw BIN --out GTDIR   # extract training tiles
  edgeaware_train.py train  --gt GTDIR --out crates/flashraw/src/edgeaware
  edgeaware_train.py eval   --gt GTDIR [--tables DIR]                  # PSNR against bilinear
Needs numpy. Output: `bayer.bin`, `xtrans.bin` (little-endian f32).
"""
import argparse, glob, os, subprocess, sys
import numpy as np
from numpy.lib.stride_tricks import sliding_window_view as swv

K, D, EPS = 7, 4, 1e-3
SR = 3     # support radius of the filters (3 = the full 7x7; 2 trains 5x5 filters inside the 7x7 layout, the outer taps zero)
GW = 0.0   # least squares row weight (green of the truth)^-GW; 0.58 weighs errors as the sRGB encoding does
# Sensor noise simulated on the training mosaics: variance = a * value + b (white balanced
# linear units), clean images plus two noise levels, so that the filters also smooth chroma noise.
NOISES = ((0.0, 0.0), (4e-4, 1e-5), (1.5e-3, 2e-5))
BAYER = np.array([[0, 1], [1, 2]])
XTRANS = np.array([[1,1,0,1,1,2],[1,1,2,1,1,0],[2,0,1,0,2,1],[1,1,2,1,1,0],[1,1,0,1,1,2],[0,2,1,2,0,1]])
MARGIN = 6   # rows/columns at the border that the filters cannot reach

# ---- helpers ---------------------------------------------------------------------------------

def srgb(x):
    x = np.clip(x, 0, 1)
    return np.where(x <= 0.0031308, 12.92 * x, 1.055 * np.power(x, 1 / 2.4) - 0.055)

def box(a, r):
    k = 2 * r + 1
    p = np.pad(a, r)
    s = np.zeros(a.shape, a.dtype)
    for dy in range(k):
        for dx in range(k):
            s += p[dy:dy + a.shape[0], dx:dx + a.shape[1]]
    return s

def mosaic(gt, pat):
    h, w, _ = gt.shape
    P = pat.shape[0]
    cm = np.tile(pat, (h // P + 1, w // P + 1))[:h, :w]
    return np.take_along_axis(gt, cm[..., None], 2)[..., 0], cm

def bilinear3(m, cm):
    out = np.zeros(m.shape + (3,), np.float32)
    for c in range(3):
        mask = (cm == c).astype(np.float32)
        v = box(m * mask, 1) / np.maximum(box(mask, 1), 1e-9)
        out[..., c] = np.where(cm == c, m, v)
    return out

def direction_weights(G0):
    g = np.sqrt(np.maximum(G0, 0))
    pad = np.pad(g, 1, mode='edge')
    sh = lambda dy, dx: pad[1 + dy:1 + dy + g.shape[0], 1 + dx:1 + dx + g.shape[1]]
    ds = [sh(0, 1) - sh(0, -1), sh(1, 0) - sh(-1, 0),
          (sh(1, 1) - sh(-1, -1)) * 0.70710678, (sh(1, -1) - sh(-1, 1)) * 0.70710678]
    E = np.stack([box(d * d, 1) for d in ds], -1)
    w = 1.0 / (E + EPS) ** 2
    return (w / w.sum(-1, keepdims=True)).astype(np.float32)

# ---- training / application -------------------------------------------------------------------

def sample_slices(H, W, P, py, px, mg):
    sy = (py - mg) % P
    sx = (px - mg) % P
    return sy, sx

def noisy(m, level, seed):
    a, b = level
    if a == 0 and b == 0:
        return m
    rng = np.random.default_rng(seed)
    return m + rng.standard_normal(m.shape).astype(np.float32) * np.sqrt(a * np.maximum(m, 0) + b).astype(np.float32)

def fit(images, pat, featfn, nd, ridge=1e-4, noises=((0.0, 0.0),)):
    """Ridge least squares for all filters of every CFA position, with the weights fixed.
    Every filter sums to one (a flat field stays flat): the centre tap is eliminated."""
    P = pat.shape[0]; r = K // 2; npos = P * P
    centre = r * K + r
    others = [t for t in range(K * K) if t != centre and max(abs(t // K - r), abs(t % K - r)) <= SR]
    nt = len(others); nc = nd * nt
    XtX = np.zeros((npos, nc, nc)); Xty = np.zeros((npos, 3, nc))
    for n_img, (gt, level) in enumerate((g, l) for g in images for l in noises):
        m, cm = mosaic(gt, pat)
        m = noisy(m, level, n_img)
        w = featfn(m, cm)
        H, W = m.shape
        patches = swv(m, (K, K))[MARGIN - r:H - K + 1 - (MARGIN - r), MARGIN - r:W - K + 1 - (MARGIN - r)]
        wc = w[MARGIN:H - MARGIN, MARGIN:W - MARGIN]
        T = gt[MARGIN:H - MARGIN, MARGIN:W - MARGIN]
        for py in range(P):
            for px in range(P):
                sy = (py - MARGIN) % P; sx = (px - MARGIN) % P
                pt = patches[sy::P, sx::P].reshape(-1, K * K)
                ww = wc[sy::P, sx::P].reshape(-1, nd)
                rel = pt[:, others] - pt[:, centre:centre + 1]
                X = (ww[:, :, None] * rel[:, None, :]).reshape(len(pt), -1).astype(np.float32)
                tt = T[sy::P, sx::P].reshape(-1, 3) - pt[:, centre:centre + 1]
                if GW:
                    sw = (np.maximum(T[sy::P, sx::P].reshape(-1, 3)[:, 1], 0.01) ** -GW).astype(np.float32)
                    X = X * sw[:, None]; tt = tt * sw[:, None]
                i = py * P + px
                XtX[i] += X.T @ X
                Xty[i] += (X.T @ tt).T
    coefs = np.zeros((npos, 3, nd, K * K), np.float32)
    for py in range(P):
        for px in range(P):
            i = py * P + px
            A = XtX[i] + ridge * np.trace(XtX[i]) / nc * np.eye(nc)
            for c in range(3):
                if c != pat[py, px]:
                    s = np.linalg.solve(A, Xty[i, c]).reshape(nd, nt)
                    coefs[i, c][:, others] = s
                    coefs[i, c][:, centre] = 1.0 - s.sum(1)
    return coefs.reshape(npos, 3, nd, K, K)

def apply(m, cm, pat, coefs, w):
    P = pat.shape[0]; r = K // 2; nd = coefs.shape[2]
    H, W = m.shape
    patches = swv(m, (K, K))
    out = np.zeros((H, W, 3), np.float32)
    wc = w[r:H - r, r:W - r]
    for py in range(P):
        for px in range(P):
            sy = (py - r) % P; sx = (px - r) % P
            pt = patches[sy::P, sx::P]; ww = wc[sy::P, sx::P]
            i = py * P + px
            for c in range(3):
                if c == pat[py, px]:
                    out[r + sy:H - r:P, r + sx:W - r:P, c] = m[r + sy:H - r:P, r + sx:W - r:P]
                    continue
                acc = 0
                for d in range(nd):
                    acc = acc + ww[..., d] * np.tensordot(pt, coefs[i, c, d], axes=([2, 3], [0, 1]))
                out[r + sy:H - r:P, r + sx:W - r:P, c] = acc
    return out

def train_pattern(images, pat, noises=NOISES):
    one = lambda m, cm: np.ones(m.shape + (1,), np.float32)
    iso = fit(images, pat, one, 1, noises=noises)
    g0 = lambda m, cm: apply(m, cm, pat, iso, one(m, cm))[..., 1]
    feat = lambda m, cm: direction_weights(g0(m, cm))
    dirc = fit(images, pat, feat, D, noises=noises)
    return iso, dirc, feat

def chroma_two_pass(o, cm, xtrans):
    """Step 4, two passes (mirrors Rust). Pass 1 for a missing R or B: Bayer, the mean of the measured differences
    (R-G or B-G) at the R or B sites of the 3x3 window, a site weighing 1 / (1 + e^2) with
    e = (G_site - G) / (0.2 G + 0.015); X-Trans, the 3x3 median of the differences of the filtered planes. Pass 2,
    both: the same weighted mean over the pass-1 differences of every pixel of the window, with s = 0.1 G + 0.007.
    Measured samples are kept. X-Trans, clean frames: green at a red or blue site becomes the measured sample minus
    the pass-2 difference (Rust: weight 1 - 2 beta, 1 on clean frames)."""
    G = o[..., 1]
    H, W = G.shape
    def window(a):
        p = np.pad(a, 1, mode='edge')
        return [p[dy:dy + H, dx:dx + W] for dy in range(3) for dx in range(3)]
    gs = window(G)
    def gate(sr, sa):
        s = sr * np.maximum(G, 0) + sa
        return [1.0 / (1.0 + ((g - G) / s) ** 2) for g in gs]
    w1, w2 = gate(0.2, 0.015), gate(0.1, 0.007)
    d2 = {}
    for ch in (0, 2):
        site = (cm == ch)
        if xtrans:
            mixed = np.median(np.stack(window(o[..., ch] - G), 0), 0)
        else:
            ps = window(site.astype(np.float32)); pd = window(np.where(site, o[..., ch] - G, 0.0))
            sw = sum(w * s_ for w, s_ in zip(w1, ps)); sd = sum(w * s_ * d for w, s_, d in zip(w1, ps, pd))
            mixed = sd / np.maximum(sw, 1e-12)
        d1 = np.where(site, o[..., ch] - G, mixed)
        sw2 = sum(w2); sd2 = sum(w * d for w, d in zip(w2, window(d1)))
        d2[ch] = sd2 / sw2
    Gn = G
    if xtrans:
        Gn = np.where(cm == 0, o[..., 0] - d2[0], np.where(cm == 2, o[..., 2] - d2[2], G))
    out = [np.where(cm == 0, o[..., 0], Gn + d2[0]), Gn, np.where(cm == 2, o[..., 2], Gn + d2[2])]
    return np.stack(out, -1)

def chroma_stage(o, cm, pat):
    return chroma_two_pass(o, cm, pat.shape[0] != 2)

def blob(pat, iso, dirc):
    P = pat.shape[0]
    g = iso[:, 1, 0].copy()                         # G0 filters, [npos][7][7]
    for i in range(P * P):
        if pat[i // P, i % P] == 1:
            g[i] = 0; g[i, K // 2, K // 2] = 1
    return np.concatenate([g.reshape(-1), dirc.reshape(-1)]).astype('<f4').tobytes()

# ---- data ---------------------------------------------------------------------------------------

def read_ppm(p):
    with open(p, 'rb') as f:
        assert f.readline().strip() == b'P6'
        w, h = map(int, f.readline().split()); f.readline()
        return np.frombuffer(f.read(), dtype='>u2').reshape(h, w, 3)

def noise(g):
    x = srgb(g).mean(-1)
    lap = np.abs(4 * x[1:-1, 1:-1] - x[:-2, 1:-1] - x[2:, 1:-1] - x[1:-1, :-2] - x[1:-1, 2:])
    return np.percentile(lap, 30)

def synth(seed, S=384, ss=4):
    rng = np.random.default_rng(seed)
    n = S * ss
    yy, xx = np.mgrid[0:n, 0:n].astype(np.float32)
    c0 = rng.uniform(0.02, 0.9, 3); c1 = rng.uniform(0.02, 0.9, 3)
    t = (xx * rng.uniform(-1, 1) + yy * rng.uniform(-1, 1)) / n
    t = (t - t.min()) / (t.max() - t.min() + 1e-9)
    img = c0[None, None] * (1 - t[..., None]) + c1[None, None] * t[..., None]
    for _ in range(rng.integers(25, 60)):
        col = rng.uniform(0.0, 1.0, 3) ** 1.5
        kind = rng.integers(0, 3)
        cx, cy = rng.uniform(0, n, 2); a = rng.uniform(0, np.pi)
        ca, sa = np.cos(a), np.sin(a)
        u = (xx - cx) * ca + (yy - cy) * sa; v = -(xx - cx) * sa + (yy - cy) * ca
        if kind == 0:
            hw, hh = rng.uniform(2, 120, 2) * ss / 2; m = (abs(u) < hw) & (abs(v) < hh)
        elif kind == 1:
            rx, ry = rng.uniform(3, 100, 2) * ss / 2; m = (u / rx) ** 2 + (v / ry) ** 2 < 1
        else:
            hw = rng.uniform(0.6, 2.5) * ss / 2; hh = rng.uniform(40, 300) * ss / 2; m = (abs(u) < hw) & (abs(v) < hh)
        img[m] = col
    img = img.reshape(S, ss, S, ss, 3).mean((1, 3)).astype(np.float32)
    return np.clip(img + rng.normal(0, 0.002, img.shape).astype(np.float32), 0, 1)

def cmd_gt(a):
    os.makedirs(a.out, exist_ok=True)
    exts = ('.cr2', '.cr3', '.nef', '.arw', '.rw2', '.dng', '.orf', '.pef', '.raf')
    files = sorted(p for p in glob.glob(a.corpus + '/**/*', recursive=True) if p.lower().endswith(exts))
    rng = np.random.default_rng(1); S = 384
    tmp = os.path.join(a.out, 'tmp.ppm')
    for i, p in enumerate(files):
        if os.path.exists(f'{a.out}/{i:03d}.done'):
            continue
        r = subprocess.run([a.flashraw, 'decode', p, tmp, '--output', 'linear-rgb16', '--demosaic', 'half'],
                           capture_output=True)
        if r.returncode != 0 or not os.path.exists(tmp):
            continue
        img = read_ppm(tmp).astype(np.float32) / 65535
        os.remove(tmp)
        h, w, _ = img.shape
        if h < S + 1 or w < S + 1:
            continue
        cands = []
        for _ in range(60):
            y = rng.integers(0, h - S); x = rng.integers(0, w - S)
            c = img[y:y + S, x:x + S]; g = c[..., 1]
            e = np.abs(np.diff(g, axis=0)).mean() + np.abs(np.diff(g, axis=1)).mean()
            if 0.03 < c.mean() < 0.8 and noise(c) < 0.0075:
                cands.append((e, y, x))
        cands.sort()
        for k, (e, y, x) in enumerate([cands[-1], cands[len(cands) // 2]] if cands else []):
            np.save(f'{a.out}/{i:03d}_{k}.npy', (img[y:y + S, x:x + S] * 65535).astype(np.uint16))
        open(f'{a.out}/{i:03d}.done', 'w').write(p)
        print(i, os.path.basename(p), flush=True)

def load(gtdir, split):
    out = []
    for f in sorted(glob.glob(gtdir + '/*_?.npy')):
        i = int(os.path.basename(f)[:3])
        if split == 'train' and i % 5 == 0: continue
        if split == 'test' and i % 5 != 0: continue
        g = np.load(f).astype(np.float32) / 65535
        if noise(g) < 0.0075:
            out.append(g)
    synthetic = range(0, 36) if split in ('train', 'all') else range(100, 112)
    return out + [synth(i) for i in synthetic]

def psnr(a, b, edge=False):
    M = 8
    A = srgb(a[M:-M, M:-M]); B = srgb(b[M:-M, M:-M])
    e = ((A - B) ** 2).mean(-1)
    if edge:
        g = srgb(b).mean(-1)
        gx = box(np.abs(np.diff(g, axis=1, prepend=g[:, :1])) + np.abs(np.diff(g, axis=0, prepend=g[:1])), 1)[M:-M, M:-M]
        e = e[gx > np.quantile(gx, 0.85)]
    return -10 * np.log10(e.mean() + 1e-12)

def cmd_train(a):
    global SR
    SR = a.support
    imgs = load(a.gt, a.split)
    os.makedirs(a.out, exist_ok=True)
    for name, pat in (('bayer', BAYER), ('xtrans', XTRANS)):
        if a.cfa != 'both' and a.cfa != name:
            continue
        iso, dirc, _ = train_pattern(imgs, pat)
        open(f'{a.out}/{name}.bin', 'wb').write(blob(pat, iso, dirc))
        print('wrote', name, flush=True)

def cmd_eval(a):
    tr, te = load(a.gt, 'train'), load(a.gt, 'test')
    for name, pat in (('bayer', BAYER), ('xtrans', XTRANS)):
        iso, dirc, feat = train_pattern(tr, pat)
        for tag, level in (('clean', NOISES[0]), ('noisy', NOISES[1])):
            base, new = [], []
            for n, g in enumerate(te):
                m, cm = mosaic(g, pat)
                m = noisy(m, level, 1000 + n)
                b = bilinear3(m, cm)
                o = chroma_stage(apply(m, cm, pat, dirc, feat(m, cm)), cm, pat)
                base.append((psnr(b, g), psnr(b, g, True))); new.append((psnr(o, g), psnr(o, g, True)))
            print(name, tag, 'bilinear3', np.mean(base, 0).round(2), 'edge-aware', np.mean(new, 0).round(2), flush=True)

if __name__ == '__main__':
    ap = argparse.ArgumentParser(); sub = ap.add_subparsers(dest='cmd', required=True)
    g = sub.add_parser('gt'); g.add_argument('--corpus', required=True); g.add_argument('--flashraw', required=True); g.add_argument('--out', required=True)
    t = sub.add_parser('train'); t.add_argument('--gt', required=True); t.add_argument('--out', required=True)
    t.add_argument('--support', type=int, default=3, help='filter radius: 3 = 7x7 (default), 2 = 5x5 inside the 7x7 tables')
    t.add_argument('--split', default='all', choices=['all', 'train'], help="'train' leaves out every fifth source file (the test split)")
    t.add_argument('--cfa', default='both', choices=['both', 'bayer', 'xtrans'])
    e = sub.add_parser('eval'); e.add_argument('--gt', required=True)
    a = ap.parse_args()
    {'gt': cmd_gt, 'train': cmd_train, 'eval': cmd_eval}[a.cmd](a)
