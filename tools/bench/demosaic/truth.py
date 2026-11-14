"""Ground-truth demosaic benchmark: re-mosaic clean tiles (Bayer RGGB and X-Trans), write DNGs, develop them
with FlashRaw and with LibRaw (rawpy), OpenCV and colour-demosaicing, and score against the original.
Needs numpy, rawpy, opencv-python-headless, colour-demosaicing."""
import sys, os, json, time, subprocess
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, '..', '..', 'demosaic')); sys.path.insert(0, HERE)
import numpy as np, rawpy, cv2, edgeaware_train as T
from dng import write_dng
import colour_demosaicing as cd
import argparse
ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument('--flashraw', required=True, help='flashraw CLI binary (build it with held-out tables for an unbiased run)')
ap.add_argument('--gt', required=True, help='ground-truth tiles from edgeaware_train.py gt')
ap.add_argument('--work', required=True); ap.add_argument('--out', required=True)
ap.add_argument('--only', help='run only the methods whose name contains this text (merge the result into an earlier run)')
A = ap.parse_args()
FR_HO = A.flashraw; WORK = A.work; OUT = A.out
PAD = 80
NOISE = {'clean': T.NOISES[0], 'moderate': T.NOISES[1], 'high': T.NOISES[2]}
LR = rawpy.DemosaicAlgorithm
def libraw(path, q):
    with rawpy.imread(path) as r:
        return r.postprocess(demosaic_algorithm=LR(q), output_bps=16, output_color=rawpy.ColorSpace.raw, gamma=(1,1), no_auto_bright=True,
                             user_wb=[1,1,1,1], adjust_maximum_thr=0, user_flip=0).astype(np.float32)/65535
def flashraw(path, mode):
    ppm = path + '.' + mode + '.ppm'
    subprocess.run([FR_HO, 'decode', path, ppm, '--output', 'linear-rgb16', '--demosaic', mode], check=True, capture_output=True)
    o = T.read_ppm(ppm).astype(np.float32)/65535; os.remove(ppm); return o
CV = {}
def cvdemo(u, kind):
    code = {'bilinear': cv2.COLOR_BayerBG2RGB, 'vng': cv2.COLOR_BayerBG2RGB_VNG, 'ea': cv2.COLOR_BayerBG2RGB_EA}[kind]
    return cv2.cvtColor(u, code).astype(np.float32)/65535
def cdm(m, kind):
    f = {'malvar': cd.demosaicing_CFA_Bayer_Malvar2004, 'menon': cd.demosaicing_CFA_Bayer_Menon2007, 'ddfapd': cd.demosaicing_CFA_Bayer_DDFAPD}[kind]
    return f(m, 'RGGB').astype(np.float32)
METHODS = {
 'bayer': [('FlashRaw bilinear', 'fr', 'bilinear'), ('FlashRaw edge-aware', 'fr', 'edge-aware'), ('FlashRaw AHD', 'fr', 'ahd'),
           ('FlashRaw PPG', 'fr', 'ppg'), ('FlashRaw RCD', 'fr', 'rcd'),
           ('LibRaw linear', 'lr', 0), ('LibRaw VNG', 'lr', 1), ('LibRaw PPG', 'lr', 2), ('LibRaw AHD', 'lr', 3), ('LibRaw DCB', 'lr', 4), ('LibRaw DHT', 'lr', 11), ('LibRaw AAHD', 'lr', 12),
           ('OpenCV bilinear', 'cv', 'bilinear'), ('OpenCV VNG', 'cv', 'vng'), ('OpenCV edge-aware', 'cv', 'ea'),
           ('colour-demosaicing Malvar', 'cd', 'malvar'), ('colour-demosaicing Menon', 'cd', 'menon'), ('colour-demosaicing DDFAPD', 'cd', 'ddfapd')],
 'xtrans': [('FlashRaw bilinear', 'fr', 'bilinear'), ('FlashRaw edge-aware', 'fr', 'edge-aware'),
            ('LibRaw linear', 'lr', 0), ('LibRaw VNG', 'lr', 1), ('LibRaw Markesteijn 1-pass', 'lr', 2), ('LibRaw Markesteijn 3-pass', 'lr', 3)],
}
def chroma_err(o, g):
    # mean absolute error of the colour differences R-G and B-G (sRGB-encoded), on edge pixels
    M = 8; A = T.srgb(o[M:-M, M:-M]); B = T.srgb(g[M:-M, M:-M])
    d = np.abs((A[...,0]-A[...,1]) - (B[...,0]-B[...,1])) + np.abs((A[...,2]-A[...,1]) - (B[...,2]-B[...,1]))
    gg = T.srgb(g).mean(-1)
    gx = T.box(np.abs(np.diff(gg, axis=1, prepend=gg[:, :1])) + np.abs(np.diff(gg, axis=0, prepend=gg[:1])), 1)[M:-M, M:-M]
    return float(d[gx > np.quantile(gx, 0.85)].mean()/2)
tiles = T.load(A.gt, 'test')
nreal = len(tiles) - 12
res = []
if A.only and os.path.exists(OUT):
    # Merge: keep the earlier run's rows of the methods not re-run.
    res = [r for r in json.load(open(OUT)) if A.only not in r['method']]
os.makedirs(WORK, exist_ok=True)
for ti, g0 in enumerate(tiles):
    kind = 'photo' if ti < nreal else 'synthetic'
    g = np.pad(g0, ((PAD,PAD),(PAD,PAD),(0,0)), mode='reflect')
    for pname, pat in (('bayer', T.BAYER), ('xtrans', T.XTRANS)):
        for nname, lvl in NOISE.items():
            m, cm = T.mosaic(g, pat)
            m = T.noisy(m, lvl, 5000 + ti)
            u = np.clip(np.round(m*(65535-512)+512), 0, 65535).astype(np.uint16)
            path = f'{WORK}/t.dng'; write_dng(path, u, pat)
            ref = g0
            crop = lambda o: o[PAD:-PAD, PAD:-PAD]
            for name, lib, arg in METHODS[pname]:
                if A.only and A.only not in name: continue
                t0 = time.time()
                try:
                    if lib == 'fr': o = flashraw(path, arg)
                    elif lib == 'lr': o = libraw(path, arg)
                    elif lib == 'cv': o = cvdemo(np.clip(np.round((u.astype(np.float32)-512)*65535/65023), 0, 65535).astype(np.uint16), arg)
                    else: o = cdm(np.clip((u.astype(np.float32)-512)/(65535-512), 0, 1), arg)
                except Exception as e:
                    print('ERR', name, pname, e, flush=True); continue
                o = np.clip(crop(o), 0, 1)
                res.append(dict(tile=ti, kind=kind, cfa=pname, noise=nname, method=name,
                                psnr=float(T.psnr(o, ref)), psnr_edge=float(T.psnr(o, ref, True)), chroma=chroma_err(o, ref)))
    print('tile', ti, kind, flush=True)
    json.dump(res, open(OUT, 'w'))
