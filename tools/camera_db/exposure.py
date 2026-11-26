#!/usr/bin/env python3
"""Measure per-camera brightness offsets of the standard tone from sample files.

The standard tone (docs/design/tone.md) multiplies linear values by 2^offset before its curve.
DNG files state the offset (BaselineExposure); other formats do not, and their cameras render
at different brightness for the same exposure. This tool measures the offset of each camera
model from its own files and stores it in the `exposures` table of
crates/flashraw-camera-db/data/cameras.json.

Method.
  measure  `flashraw camera-db exposure` finds, per file, the offset that makes the luminance
           of FlashRaw's standard rendering match that of the camera's embedded JPEG over the
           mid-tones (30th to 70th percentiles; JPEGs at least 320 pixels wide).
  apply    Per model (canonical make and model), the median over its files. A camera's JPEG
           engine adds its own brightness on top of the standard rendering, by an amount that
           depends on the make, so a manual correction per make is added (MAKE_CORRECTION,
           docs/design/tone.md). Models with few files are pulled towards the make's default
           (SHRINK, in files), since a single scene can be off by half a stop. A make's default
           is the median over its models, stored for models without files. DNG files are not
           measured: they carry their own offset.

usage:
  tools/camera_db/exposure.py measure [--jobs N] [--out FILE] DIR_OR_FILE...
      Measure every raw file under the given directories (JSON lines, default
      target/exposure/measurements.jsonl; files already measured are skipped).
  tools/camera_db/exposure.py apply [--in FILE...] [--write]
      Aggregate and print the table; with --write store it in cameras.json (normalised with
      `flashraw camera-db generate`).
"""
import argparse, collections, json, os, statistics, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DB_PATH = os.path.join(ROOT, "crates/flashraw-camera-db/data/cameras.json")
BIN = os.environ.get("FLASHRAW_BIN", os.path.join(ROOT, "target/release/flashraw"))
OUT = os.path.join(ROOT, "target/exposure/measurements.jsonl")
RAW_EXT = {
    ".3fr", ".ari", ".arw", ".bay", ".cr2", ".cr3", ".crw", ".dcr", ".erf", ".fff", ".gpr", ".iiq",
    ".k25", ".kdc", ".mdc", ".mef", ".mos", ".mrw", ".nef", ".nrw", ".orf", ".ori", ".pef", ".raf",
    ".raw", ".rw2", ".rwl", ".sr2", ".srf", ".srw", ".x3f",
}

# Manual corrections (stops) added to the JPEG-matched offset of every model of the make, for
# how each make's JPEG engine renders brightness; makes not listed get DEFAULT_CORRECTION.
MAKE_CORRECTION = {
    "Canon": -0.14, "Fujifilm": -0.03, "Kodak": 0.13, "Leica": -0.08, "Nikon": -0.07,
    "OM System": 0.04, "Olympus": -0.06, "Panasonic": -0.04, "Pentax": -0.12, "Raspberry Pi": 0.35,
    "Samsung": 0.01, "Sony": 0.0,
}
DEFAULT_CORRECTION = -0.06
# Files' worth of weight given to the make's default when averaging a model's files.
SHRINK = 0.5
# Makes need this many measured models for a default.
MIN_MODELS_FOR_DEFAULT = 3
# Offsets are stored to this precision (stops).
STEP = 0.05


def files_under(paths):
    for p in paths:
        if os.path.isfile(p):
            yield p
            continue
        for d, _, names in os.walk(p):
            for n in sorted(names):
                if os.path.splitext(n)[1].lower() in RAW_EXT:
                    yield os.path.join(d, n)


def measure(args):
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    done = set()
    if os.path.exists(args.out):
        for line in open(args.out):
            try:
                done.add(json.loads(line)["file"])
            except (ValueError, KeyError):
                pass
    todo = [f for f in files_under(args.paths) if f not in done]
    print(f"{len(todo)} files to measure ({len(done)} already measured)", file=sys.stderr)

    def one(f):
        r = subprocess.run([BIN, "camera-db", "exposure", f], capture_output=True, text=True, timeout=600)
        line = r.stdout.strip()
        if r.returncode != 0 or not line:
            line = json.dumps({"file": f, "error": (r.stderr.strip() or "failed")[-300:]})
        return line

    with open(args.out, "a") as out, ThreadPoolExecutor(args.jobs) as ex:
        for i, line in enumerate(ex.map(one, todo), 1):
            out.write(line + "\n")
            if i % 100 == 0:
                print(f"{i}/{len(todo)}", file=sys.stderr)


def rows(paths):
    for p in paths:
        for line in open(p):
            try:
                r = json.loads(line)
            except ValueError:
                continue
            # DNG files carry their own offset; film scanners have no camera rendering.
            if "ev" not in r or r.get("format") == "Dng" or r.get("mode") == "film-scan":
                continue
            make = r.get("canonical_make") or r.get("make") or ""
            model = r.get("canonical_model") or r.get("model") or ""
            if make and model:
                yield make, model, r["ev"], r["file"]


def snap(v):
    return round(round(v / STEP) * STEP, 2) + 0.0


def aggregate(paths):
    by_model = collections.defaultdict(dict)
    for make, model, ev, f in rows(paths):
        by_model[(make, model)][f] = ev  # one value per file, even if measured twice
    models = {k: (statistics.median(v.values()), len(v)) for k, v in by_model.items()}
    by_make = collections.defaultdict(list)
    for (make, _), (ev, n) in models.items():
        by_make[make].append((ev, n))
    entries = []
    defaults = {}
    for make, l in sorted(by_make.items()):
        corr = MAKE_CORRECTION.get(make, DEFAULT_CORRECTION)
        if len(l) >= MIN_MODELS_FOR_DEFAULT:
            defaults[make] = statistics.median(ev for ev, _ in l) + corr
            entries.append({"canonical_make": make, "ev": snap(defaults[make]), "files": sum(n for _, n in l)})
    for (make, model), (ev, n) in sorted(models.items()):
        v = ev + MAKE_CORRECTION.get(make, DEFAULT_CORRECTION)
        if make in defaults:
            v = (n * v + SHRINK * defaults[make]) / (n + SHRINK)
        entries.append({"canonical_make": make, "model": model, "ev": snap(v), "files": n})
    return entries


def apply(args):
    entries = aggregate(args.inputs)
    for e in entries:
        print(f"{e['canonical_make']:14s} {e.get('model', '(default)'):28s} {e['ev']:+.2f}  {e['files']} files")
    print(f"{len(entries)} entries", file=sys.stderr)
    if args.write:
        db = json.load(open(DB_PATH))
        db["exposures"] = entries
        with open(DB_PATH, "w") as f:
            json.dump(db, f)
        subprocess.run([BIN, "camera-db", "generate", DB_PATH], check=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    m = sub.add_parser("measure")
    m.add_argument("paths", nargs="+")
    m.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 2))
    m.add_argument("--out", default=OUT)
    a = sub.add_parser("apply")
    a.add_argument("--in", dest="inputs", nargs="+", default=[OUT])
    a.add_argument("--write", action="store_true")
    args = ap.parse_args()
    measure(args) if args.cmd == "measure" else apply(args)


if __name__ == "__main__":
    main()
