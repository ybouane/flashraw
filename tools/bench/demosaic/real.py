"""No-reference benchmark on real files: colour-speckle index (high-frequency colour over high-frequency luma) and
fine detail (luma Laplacian) of FlashRaw and LibRaw (rawpy) developments. Needs numpy, rawpy."""
import sys, os, json, subprocess, time
import numpy as np, rawpy
import argparse
ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument('--flashraw', required=True); ap.add_argument('--corpus', required=True, help='directory with the sample files')
ap.add_argument('--work', required=True); ap.add_argument('--out', required=True)
ap.add_argument('--files', help='JSON {"bayer": [...], "xtrans": [...]} of paths relative to --corpus (default: built-in list)')
A = ap.parse_args()
FR = A.flashraw; OUT = A.out; WORK = A.work; C = A.corpus.rstrip('/') + '/'
BAYER = ['Canon/EOS 250D/Canon250D_RAW-19:9.CR3','Canon/EOS 250D/Canon250D_cRAW-1:1.CR3','Canon/EOS R/Canon_EOS_R_CRAW_ISO_100_crop_nodual.CR3',
 'Canon/EOS R10/IMG_4276.CR3','Canon/EOS R5/Canon_EOS_R5_CRAW_ISO_100_nocrop_nodual.CR3','Canon/EOS R50/Canon_R50_ISO100_cRAW_mechanicShutter.CR3',
 'Canon/EOS R6 Mark II/canon-r6m2-img_0356-craw-dual-crop.cr3','Canon/EOS RP/Canon_EOS_RP_RAW_ISO_100_crop_nodual.CR3','Canon/EOS Rebel T100/IMG_7229.CR2','Canon/EOS Rebel T4i/IMG_6980.CR2',
 'Nikon/D3S/_CB32157.NEF','Nikon/D3S/_CB32158.NEF','Nikon/D3S/_CB32159.NEF']
XT = ['FUJIFILM/X-T50/DSCF0120.RAF','FUJIFILM/X-T30 III/DSCF0131.RAF','Fujifilm/X-E3/DSCF2176.RAF','Fujifilm/X-H1/fuji_xh1_compressed.RAF','Fujifilm/X-H2/Fuji-X-H2-Compressed-Raw.RAF',
 'Fujifilm/X-Pro2/_DSF3051.RAF','Fujifilm/X-Pro3/_DSF2385.RAF','Fujifilm/X-T2/20170114_174341_TFW04727.RAF','Fujifilm/X-E1/DSCF6490.RAF','Fujifilm/X-Pro1/DSCF2131.RAF']
LRB = [('LibRaw linear',0),('LibRaw VNG',1),('LibRaw PPG',2),('LibRaw AHD',3),('LibRaw DCB',4),('LibRaw DHT',11),('LibRaw AAHD',12)]
LRX = [('LibRaw linear',0),('LibRaw VNG',1),('LibRaw Markesteijn 1-pass',2),('LibRaw Markesteijn 3-pass',3)]
def lap(c): return np.abs(4*c[1:-1,1:-1]-c[:-2,1:-1]-c[2:,1:-1]-c[1:-1,:-2]-c[1:-1,2:]).mean()
def metrics(img):
    r,g,b = img[...,0],img[...,1],img[...,2]; y = img@[0.2126,0.7152,0.0722]
    return dict(chroma=float((lap(r-g)+lap(b-g))/lap(y)), sharp=float(lap(y)))
def read_ppm(p):
    d = open(p,'rb').read(); w,h = map(int, d.split(maxsplit=3)[1:3]); return np.frombuffer(d[-w*h*6:], '>u2').reshape(h,w,3)/65535
res = []
if A.files:
    import json as _j; _l = _j.load(open(A.files)); BAYER, XT = _l['bayer'], _l['xtrans']
for cfa, files, lr in (('bayer',BAYER,LRB),('xtrans',XT,LRX)):
    for rel in files:
        p = C+rel
        if not os.path.exists(p): print('missing',rel); continue
        row = dict(cfa=cfa, file=rel, methods={})
        try:
            meta = json.loads(subprocess.run([FR,'verify',p],check=True,capture_output=True).stdout)
            outs = {}
            for name, mode in (('FlashRaw bilinear','bilinear'),('FlashRaw edge-aware','edge-aware'))+((('FlashRaw AHD','ahd'),('FlashRaw PPG','ppg'),('FlashRaw RCD','rcd')) if cfa == 'bayer' else ()):
                ppm = f'{WORK}/o.ppm'
                subprocess.run([FR,'decode','--output','rgb16','--demosaic',mode,p,ppm],check=True,capture_output=True)
                outs[name] = read_ppm(ppm)
            h,w,_ = outs['FlashRaw bilinear'].shape
            dx = meta['crop']['x']-meta['active_area']['x']; dy = meta['crop']['y']-meta['active_area']['y']
            with rawpy.imread(p) as r:
                row['mp'] = round(w*h/1e6,1)
                for name,q in lr:
                    try:
                        o = r.postprocess(demosaic_algorithm=rawpy.DemosaicAlgorithm(q),output_bps=16,use_camera_wb=True,no_auto_bright=True,gamma=(2.4,12.92),highlight_mode=rawpy.HighlightMode.Clip) / 65535
                        outs[name] = o[dy:dy+h, dx:dx+w]
                    except Exception as e: print('ERR',rel,name,e)
            y0 = max(0,h//2-512); x0 = max(0,w//2-512)
            for name,o in outs.items():
                if o.shape[:2] != (h,w): continue
                row['methods'][name] = metrics(o[y0:y0+1024, x0:x0+1024].astype(np.float32))
            res.append(row); print(cfa, rel, row['methods'].get('FlashRaw edge-aware'), flush=True)
        except Exception as e: print('FAIL',rel,e,flush=True)
        json.dump(res, open(OUT,'w'))
