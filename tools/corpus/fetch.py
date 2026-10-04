#!/usr/bin/env python3
"""Download the test corpus described by a manifest TSV (make, model, file, size, url).

Usage: tools/corpus/fetch.py [--manifest tools/corpus/manifest.tsv] [--dest corpus]
                             [--max-size-mb N] [--filter REGEX] [--jobs 8]
Files already present with the expected size are skipped, so the command is resumable.
"""
import argparse, os, re, sys, urllib.request, shutil
from concurrent.futures import ThreadPoolExecutor

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default="tools/corpus/manifest.tsv")
    ap.add_argument("--dest", default=os.environ.get("FLASHRAW_CORPUS", "corpus"))
    ap.add_argument("--max-size-mb", type=float, default=0)
    ap.add_argument("--filter", default="")
    ap.add_argument("--jobs", type=int, default=8)
    a = ap.parse_args()
    rx = re.compile(a.filter, re.I) if a.filter else None
    todo = []
    for line in open(a.manifest, encoding="utf-8"):
        make, model, fname, size, url = line.rstrip("\n").split("\t")
        size = int(size)
        if a.max_size_mb and size > a.max_size_mb * 1024 * 1024:
            continue
        rel = os.path.join(make, model, fname)
        if rx and not rx.search(rel):
            continue
        todo.append((rel, size, url))
    def get(item):
        rel, size, url = item
        path = os.path.join(a.dest, rel)
        # Index sizes are rounded (e.g. "59M"), so only re-download obviously truncated files.
        if os.path.exists(path) and os.path.getsize(path) >= size * 0.9:
            return "skip"
        os.makedirs(os.path.dirname(path), exist_ok=True)
        tmp = path + ".part"
        try:
            with urllib.request.urlopen(url, timeout=120) as r, open(tmp, "wb") as f:
                shutil.copyfileobj(r, f, 1 << 20)
            os.replace(tmp, path)
            return "ok"
        except Exception as e:
            print(f"FAIL {rel}: {e}", file=sys.stderr)
            return "fail"
    stats = {}
    with ThreadPoolExecutor(a.jobs) as ex:
        for i, res in enumerate(ex.map(get, todo)):
            stats[res] = stats.get(res, 0) + 1
            if i % 50 == 0:
                print(f"{i}/{len(todo)} {stats}", flush=True)
    print(f"done {stats}")

if __name__ == "__main__":
    main()
