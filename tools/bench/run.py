#!/usr/bin/env python3
"""Benchmark FlashRaw on a list of corpus files and optionally detect regressions.

Each file is decoded by `flashraw benchmark --json` in a fresh process (input read into
memory first; median of `--iterations` timed decodes after a warm-up), for every mode:
  decode  open + full sensor decode (raw-u16)
  rgb     RAW -> 8-bit sRGB (bilinear demosaic, as-shot white balance, colour matrix)

usage:
  tools/bench/run.py benches/benchmark-set.tsv [--iterations 5] [--threads N] \
      [--out benches/results/<host>.jsonl] [--markdown benches/results/<host>.md] \
      [--baseline old.jsonl --tolerance 10]

With --baseline the run fails (exit 1) when a file is slower than the baseline by more
than --tolerance percent (and by more than 0.5 ms, to ignore timer noise on tiny files).
"""
import argparse
import json
import os
import statistics
import subprocess
import sys

ap = argparse.ArgumentParser()
ap.add_argument("inputs", nargs="+", help="files, or .tsv/.txt lists of corpus-relative paths (first column)")
ap.add_argument("--corpus", default="corpus")
ap.add_argument("--flashraw", default="target/release/flashraw")
ap.add_argument("--iterations", type=int, default=5)
ap.add_argument("--threads", type=int)
ap.add_argument("--mode", nargs="+", default=["decode", "rgb"], choices=["decode", "rgb"])
ap.add_argument("--out", default="benches/results/latest.jsonl")
ap.add_argument("--markdown", default="benches/results/latest.md")
ap.add_argument("--baseline")
ap.add_argument("--tolerance", type=float, default=10.0)
a = ap.parse_args()

files = []
for inp in a.inputs:
    if inp.endswith((".tsv", ".txt")):
        for line in open(inp):
            line = line.strip()
            if line and not line.startswith("#"):
                files.append(os.path.join(a.corpus, line.split("\t")[0]))
    else:
        files.append(inp)

rows = []
for f in files:
    if not os.path.exists(f):
        print(f"missing: {f}", file=sys.stderr)
        continue
    for mode in a.mode:
        cmd = [a.flashraw, "benchmark", "--json", "--iterations", str(a.iterations),
               "--output", "rgba8" if mode == "rgb" else "raw-u16"]
        if a.threads:
            cmd += ["--threads", str(a.threads)]
        p = subprocess.run(cmd + [f], capture_output=True, text=True)
        rec = next((json.loads(l) for l in reversed(p.stdout.splitlines()) if l.startswith("{")), None)
        row = {"file": f, "mode": mode}
        if rec is None:
            row["error"] = (p.stderr.strip().splitlines() or ["no output"])[-1]
        else:
            row.update({
                "camera": rec.get("camera"), "format": rec.get("format"), "compression": rec.get("compression"),
                "megapixels": rec.get("megapixels"), "ms": rec.get("total_ms_median"),
                "ms_min": rec.get("total_ms_min"),
                # The CLI reports ru_maxrss as is: bytes on macOS, KiB elsewhere.
                "rss_mb": (rec.get("maxrss_kb") or 0) / (1024 * 1024 if sys.platform == "darwin" else 1024) or None,
                "backend": (rec.get("backend") or {}).get("decode"), "stages": rec.get("stages"),
            })
        rows.append(row)
        print(f"{mode:6} {row.get('ms') or float('nan'):9.2f} ms  {f}", file=sys.stderr)

os.makedirs(os.path.dirname(a.out) or ".", exist_ok=True)
with open(a.out, "w") as o:
    for r in rows:
        o.write(json.dumps(r) + "\n")

lines = ["| file | camera | compression | MP | mode | ms | MP/s | peak RSS MB |", "|---|---|---|---:|---|---:|---:|---:|"]
for r in rows:
    ms, mp = r.get("ms"), r.get("megapixels")
    ms_s = "" if ms is None else "%.1f" % ms
    rate = "" if not (ms and mp) else "%.0f" % (mp / ms * 1000)
    rss = "" if not r.get("rss_mb") else "%.0f" % r["rss_mb"]
    lines.append(
        f"| {os.path.basename(r['file'])} | {r.get('camera') or ''} | {r.get('compression') or ''} | "
        f"{mp or 0:.1f} | {r['mode']} | {ms_s} | {rate} | {rss} |"
    )
with open(a.markdown, "w") as o:
    o.write("\n".join(lines) + "\n")

if a.baseline:
    base = {(r["file"], r["mode"]): r for r in map(json.loads, open(a.baseline))}
    worse = []
    for r in rows:
        b = base.get((r["file"], r["mode"]))
        if b and b.get("ms") and r.get("ms"):
            if r["ms"] > b["ms"] * (1 + a.tolerance / 100) and r["ms"] - b["ms"] > 0.5:
                worse.append((r["ms"] / b["ms"], r))
    for ratio, r in sorted(worse, reverse=True):
        print(f"REGRESSION x{ratio:.2f}: {r['mode']} {r['file']}", file=sys.stderr)
    ratios = [r["ms"] / base[(r["file"], r["mode"])]["ms"] for r in rows
              if r.get("ms") and base.get((r["file"], r["mode"]), {}).get("ms")]
    if ratios:
        print(f"median time vs baseline: x{statistics.median(ratios):.3f}", file=sys.stderr)
    sys.exit(1 if worse else 0)
