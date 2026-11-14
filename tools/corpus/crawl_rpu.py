#!/usr/bin/env python3
"""Crawl the raw.pixls.us /data/ index and emit a TSV manifest: make, model, filename, size, url.

The raw.pixls.us samples are CC0 (see https://raw.pixls.us). Files are not committed to
this repository; this manifest lets tools/corpus/fetch.py retrieve them on demand.
"""
import re, sys, urllib.request, urllib.parse
from concurrent.futures import ThreadPoolExecutor

BASE = "https://raw.pixls.us/data/"
ROW = re.compile(r'<a href="([^"?][^"]*)">[^<]*</a></td><td align="right">([^<]*)</td><td align="right">\s*([^<]*?)\s*</td>')

def listdir(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        html = r.read().decode("utf-8", "replace")
    out = []
    for href, _mod, size in ROW.findall(html):
        if href.startswith("/"):
            continue
        out.append((href, size))
    return out

def parse_size(s):
    s = s.strip()
    if s in ("-", ""): return -1
    m = re.match(r"([\d.]+)([KMG]?)", s)
    v = float(m.group(1)); mul = {"": 1, "K": 1024, "M": 1024**2, "G": 1024**3}[m.group(2)]
    return int(v * mul)

def main():
    makes = [h for h, _ in listdir(BASE) if h.endswith("/")]
    def crawl_make(make):
        rows = []
        for model, _ in listdir(BASE + make):
            if not model.endswith("/"): continue
            for f, size in listdir(BASE + make + model):
                if f.endswith("/"): continue
                rows.append((urllib.parse.unquote(make[:-1]), urllib.parse.unquote(model[:-1]),
                             urllib.parse.unquote(f), parse_size(size), BASE + make + model + f))
        return rows
    with ThreadPoolExecutor(16) as ex:
        for rows in ex.map(crawl_make, makes):
            for r in rows:
                print("\t".join(map(str, r)))

if __name__ == "__main__":
    main()
