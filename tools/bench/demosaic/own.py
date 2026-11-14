"""FlashRaw-only ground-truth benchmark: re-mosaic clean tiles (Bayer RGGB and X-Trans), write DNGs, develop them with
the given FlashRaw demosaics and score against the original (sRGB PSNR over all pixels and over the 15 % highest-gradient
pixels, colour error on edges). Needs numpy only. Tiles come from `tools/demosaic/edgeaware_train.py gt`; every tile is
used (build the tiles from files the shipped tables were not trained on for an unbiased run).
  own.py --flashraw target/release/flashraw --gt DIR --work DIR --out results.json --methods bilinear edge-aware ahd"""
import sys, os, json, glob, subprocess, argparse
from concurrent.futures import ThreadPoolExecutor
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, '..', '..', 'demosaic')); sys.path.insert(0, HERE)
import numpy as np, edgeaware_train as T
from dng import write_dng
ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument('--flashraw', required=True); ap.add_argument('--gt', required=True); ap.add_argument('--work', required=True)
ap.add_argument('--out', required=True); ap.add_argument('--methods', nargs='+', required=True)
ap.add_argument('--cfa', nargs='+', default=['bayer', 'xtrans']); ap.add_argument('--limit', type=int, default=0, help='use the first N tiles only')
ap.add_argument('--jobs', type=int, default=8)
ap.add_argument('--split', default='all', choices=['all', 'train', 'test'], help='tiles of every fifth source file are the test split (as in edgeaware_train.load)')
A = ap.parse_args()
PAD = 80
NOISE = {'clean': T.NOISES[0], 'moderate': T.NOISES[1], 'high': T.NOISES[2]}
def chroma_err(o, g):
    M = 8; a = T.srgb(o[M:-M, M:-M]); b = T.srgb(g[M:-M, M:-M])
    d = np.abs((a[..., 0] - a[..., 1]) - (b[..., 0] - b[..., 1])) + np.abs((a[..., 2] - a[..., 1]) - (b[..., 2] - b[..., 1]))
    gg = T.srgb(g).mean(-1)
    gx = T.box(np.abs(np.diff(gg, axis=1, prepend=gg[:, :1])) + np.abs(np.diff(gg, axis=0, prepend=gg[:1])), 1)[M:-M, M:-M]
    return float(d[gx > np.quantile(gx, 0.85)].mean() / 2)
tiles = []
for f in sorted(glob.glob(A.gt + '/*_?.npy')):
    i = int(os.path.basename(f)[:3])
    if (A.split == 'train' and i % 5 == 0) or (A.split == 'test' and i % 5 != 0): continue
    g = np.load(f).astype(np.float32) / 65535
    if T.noise(g) < 0.0075: tiles.append((os.path.basename(f), g))
if A.limit: tiles = tiles[:A.limit]
os.makedirs(A.work, exist_ok=True)
def one(args):
    ti, (name, g0) = args
    out = []
    g = np.pad(g0, ((PAD, PAD), (PAD, PAD), (0, 0)), mode='reflect')
    for pname in A.cfa:
        pat = T.BAYER if pname == 'bayer' else T.XTRANS
        for nname, lvl in NOISE.items():
            m, cm = T.mosaic(g, pat); m = T.noisy(m, lvl, 5000 + ti)
            u = np.clip(np.round(m * (65535 - 512) + 512), 0, 65535).astype(np.uint16)
            path = f'{A.work}/t{ti}_{pname}_{nname}.dng'; write_dng(path, u, pat)
            for meth in A.methods:
                ppm = path + '.' + meth + '.ppm'
                r = subprocess.run([A.flashraw, 'decode', path, ppm, '--output', 'linear-rgb16', '--demosaic', meth], capture_output=True)
                if r.returncode: out.append(dict(tile=name, cfa=pname, noise=nname, method=meth, error=r.stderr.decode()[-200:])); continue
                o = np.clip(T.read_ppm(ppm).astype(np.float32)[PAD:-PAD, PAD:-PAD] / 65535, 0, 1); os.remove(ppm)
                out.append(dict(tile=name, cfa=pname, noise=nname, method=meth, psnr=float(T.psnr(o, g0)),
                                psnr_edge=float(T.psnr(o, g0, True)), chroma=chroma_err(o, g0)))
            os.remove(path)
    return out
with ThreadPoolExecutor(A.jobs) as ex: res = [r for part in ex.map(one, enumerate(tiles)) for r in part]
json.dump(res, open(A.out, 'w'))
errs = [r for r in res if 'error' in r]
for e in errs[:3]: print('ERR', e)
print(f'{len(tiles)} tiles')
for cfa in A.cfa:
    for nname in NOISE:
        print(f'{cfa:7s} {nname:9s} ' + '  '.join(f'{m}: {np.mean([r["psnr"] for r in res if r.get("cfa")==cfa and r["noise"]==nname and r["method"]==m and "psnr" in r]):.2f}/' +
              f'{np.mean([r["psnr_edge"] for r in res if r.get("cfa")==cfa and r["noise"]==nname and r["method"]==m and "psnr" in r]):.2f}/' +
              f'{np.mean([r["chroma"] for r in res if r.get("cfa")==cfa and r["noise"]==nname and r["method"]==m and "psnr" in r]):.4f}' for m in A.methods))
