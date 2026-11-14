"""Develop time (post-process stage) of FlashRaw and LibRaw (rawpy) on one Bayer and one X-Trans file."""
import sys, os, json, subprocess, time
import argparse
ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument('--flashraw', required=True); ap.add_argument('--bayer', required=True, help='a Bayer RAW file'); ap.add_argument('--xtrans', required=True, help='an X-Trans RAW file')
ap.add_argument('--out', required=True); ap.add_argument('--threads', type=int, nargs='+', default=[1, 16])
A = ap.parse_args()
FR = A.flashraw
files = {'bayer': A.bayer, 'xtrans': A.xtrans}
LR = {'bayer': [('LibRaw linear',0),('LibRaw VNG',1),('LibRaw PPG',2),('LibRaw AHD',3),('LibRaw DCB',4),('LibRaw DHT',11),('LibRaw AAHD',12)],
      'xtrans': [('LibRaw linear',0),('LibRaw VNG',1),('LibRaw Markesteijn 1-pass',2),('LibRaw Markesteijn 3-pass',3)]}
child = '''
import sys, time, rawpy
p, q = sys.argv[1], int(sys.argv[2])
best = 1e9
with rawpy.imread(p) as r:
    r.unpack()
    for _ in range(2):
        t = time.perf_counter()
        r.postprocess(demosaic_algorithm=rawpy.DemosaicAlgorithm(q), output_bps=16, use_camera_wb=True, no_auto_bright=True, gamma=(2.4,12.92))
        best = min(best, time.perf_counter()-t)
print(best*1000)
'''
res = []
for cfa, p in files.items():
    for threads in A.threads:
        for name, mode in (('FlashRaw bilinear','bilinear'),('FlashRaw edge-aware','edge-aware'))+((('FlashRaw AHD','ahd'),('FlashRaw PPG','ppg'),('FlashRaw RCD','rcd')) if cfa == 'bayer' else ()):
            j = json.loads(subprocess.run([FR,'benchmark',p,'--output','rgb16','--demosaic',mode,'--iterations','3','--threads',str(threads),'--json'],capture_output=True,check=True).stdout)
            j = j[0] if isinstance(j, list) else j
            res.append(dict(cfa=cfa, threads=threads, method=name, ms=j['stages']['Post-process'] if 'stages' in j else None, mp=j['megapixels']))
        for name, q in LR[cfa]:
            env = dict(os.environ, OMP_NUM_THREADS=str(threads))
            t = float(subprocess.run([sys.executable,'-c',child,p,str(q)],capture_output=True,check=True,env=env).stdout)
            res.append(dict(cfa=cfa, threads=threads, method=name, ms=t, mp=res[-1]['mp']))
        print(cfa, threads, [(r['method'], round(r['ms'])) for r in res if r['cfa']==cfa and r['threads']==threads], flush=True)
json.dump(res, open(A.out,'w'))
