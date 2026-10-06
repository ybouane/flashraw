#!/usr/bin/env python3
"""Measure the cameras' own image areas from files and record them in camera-db/cameras.json.

The camera's image size is what the camera itself produced: the size its files record
(EXIF pixel dimensions, maker-note image-size fields, Canon SensorInfo/AspectInfo, Sony
DefaultCrop/FullImageSize, ...) or the size of the full-size JPEG it embedded. Its
position inside the active area is measured by matching that JPEG against the developed
raw data (`flashraw camera-db crops --register`).

usage:
  tools/camera_db_crops.py measure [--out FILE] [--rgba] [PATH ...]
      Run `flashraw camera-db crops --register` over the files (default: the CC0 corpus
      at $FLASHRAW_CORPUS or ./corpus, plus every local file listed in the real-world
      manifest $FLASHRAW_REALWORLD/manifest.tsv when present) and write JSON lines.
  tools/camera_db_crops.py propose [--in FILE] [--write]
      Aggregate measurements per camera-database entry and active-area size; print the
      proposed `image_crops`, and with --write store them in camera-db/cameras.json.
  tools/camera_db_crops.py check [--in FILE]
      Every crop lies inside its active area, crops follow the camera's image size, and
      (after `measure --rgba`) RGBA outputs have the camera's image dimensions.
"""
import collections, json, os, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DB = os.path.join(ROOT, "camera-db/cameras.json")
BIN = os.environ.get("FLASHRAW_BIN", os.path.join(ROOT, "target/release/flashraw"))
DEFAULT_OUT = "camera-crops.jsonl"
# Registrations below this correlation are ignored (lens corrections or heavy processing
# in the camera JPEG); positions at the edge of the search range are failed matches.
MIN_SCORE = 0.95
SKIP_FORMATS = {"Dng"}


def arg(name, default=None):
    if name in sys.argv:
        i = sys.argv.index(name)
        v = sys.argv[i + 1]
        del sys.argv[i : i + 2]
        return v
    return default


def flag(name):
    if name in sys.argv:
        sys.argv.remove(name)
        return True
    return False


def default_files():
    files = []
    corpus = os.environ.get("FLASHRAW_CORPUS", os.path.join(ROOT, "corpus"))
    for root, _, names in os.walk(corpus):
        files += [os.path.join(root, n) for n in names]
    rw = os.environ.get("FLASHRAW_REALWORLD", "/var/projects/flashraw-realworld")
    manifest = os.path.join(rw, "manifest.tsv")
    if os.path.exists(manifest):
        for line in open(manifest, encoding="utf-8"):
            c = line.rstrip("\n").split("\t")
            if len(c) > 4 and c[0] != "source" and os.path.exists(c[4]):
                files.append(c[4])
    return sorted(set(files))


def measure():
    out = arg("--out", DEFAULT_OUT)
    rgba = flag("--rgba")
    paths = sys.argv[2:]
    files = []
    for p in paths:
        if os.path.isdir(p):
            for root, _, names in os.walk(p):
                files += [os.path.join(root, n) for n in names]
        else:
            files.append(p)
    files = sorted(files) if files else default_files()
    cmd = [BIN, "camera-db", "crops", "--register"] + (["--rgba"] if rgba else [])

    def run(chunk):
        r = subprocess.run(cmd + chunk, capture_output=True, text=True)
        return [l for l in r.stdout.splitlines() if l.startswith("{")]

    chunks = [files[i : i + 4] for i in range(0, len(files), 4)]
    n = 0
    with ThreadPoolExecutor(max(1, (os.cpu_count() or 4) // 2)) as ex, open(out, "w", encoding="utf-8") as o:
        for lines in ex.map(run, chunks):
            for l in lines:
                o.write(l + "\n")
                n += 1
    print(f"measured {n} of {len(files)} files -> {out}")


def load(path):
    rows = []
    for line in open(path, encoding="utf-8"):
        r = json.loads(line)
        if "error" not in r and r.get("format") not in SKIP_FORMATS:
            rows.append(r)
    return rows


def norm(s):
    return " ".join(s.split()).upper()


class Index:
    """camera-db entries as the decoders find them: (make, model, mode), the default
    mode, then any mode; model aliases included."""

    def __init__(self, db):
        self.db = db
        self.exact = {}
        self.any = {}
        for c in db["cameras"]:
            for model in [c["model"]] + [a["model"] for a in c.get("aliases", [])]:
                self.exact.setdefault((norm(c["make"]), norm(model), norm(c.get("mode", ""))), c)
                self.any.setdefault((norm(c["make"]), norm(model)), c)

    def find(self, make, model, mode):
        k = (norm(make), norm(model))
        return self.exact.get(k + (norm(mode),)) or self.exact.get(k + ("",)) or self.any.get(k)


def centred(area, w, h):
    return [((area[0] - w) // 2) & ~1, ((area[1] - h) // 2) & ~1]


def even(v):
    return int(round(v / 2.0)) * 2


def aggregate(rows):
    """{entry key: {(area w, area h): {(crop w, crop h): [rows]}}}"""
    groups = collections.defaultdict(lambda: collections.defaultdict(lambda: collections.defaultdict(list)))
    for r in rows:
        aa, cr = r["active_area"], r["crop"]
        if cr == aa:
            # The active area is the camera's image when the file's own size says so.
            sizes = [r.get("image_size"), r.get("jpeg")]
            if not any(s and sorted(s) == sorted(cr[2:]) for s in sizes):
                continue
            r = dict(r, crop_source=r.get("crop_source") or "active-area")
        elif not r.get("crop_source"):
            continue
        key = (r["make"], r["model"], r["mode"])
        groups[key][(aa[2], aa[3])][(cr[2], cr[3])].append(r)
    return groups


def proposal(area, size, rs):
    """The image area for one body / active-area size / image size, and its evidence."""
    aa_w, aa_h = area
    w, h = size
    sources = collections.Counter(r["crop_source"] for r in rs)
    offs = collections.Counter((r["crop"][0] - r["active_area"][0], r["crop"][1] - r["active_area"][1]) for r in rs)
    off = list(offs.most_common(1)[0][0])
    reg = []
    for r in rs:
        g = r.get("registration") or {}
        if "x" in g and g.get("score", 0) >= MIN_SCORE and r.get("jpeg") and sorted(r["jpeg"]) == sorted(size):
            edge = any(v < 0.5 or v > lim - 0.5 for v, lim in ((g["x"], aa_w - w), (g["y"], aa_h - h)) if lim > 0)
            if g.get("rotation") in (0, 180) and not edge:
                reg.append((g["x"], g["y"]))
    note = ""
    if len(reg) >= 2 and sources.most_common(1)[0][0] not in ("file", "active-area"):
        xs = sorted(x for x, _ in reg)
        ys = sorted(y for _, y in reg)
        mx, my = xs[len(xs) // 2], ys[len(ys) // 2]
        c = centred(area, w, h)
        # Per axis, keep the centred position unless the camera's JPEG is consistently
        # (within 2 px across at least two files) more than a pixel away from it.
        off = list(c)
        moved = []
        for i, (vals, med, lim) in enumerate(((xs, mx, aa_w - w), (ys, my, aa_h - h))):
            m = min(max(even(med), 0), lim)
            if vals[-1] - vals[0] <= 2.0 and abs(m - c[i]) > 1:
                off[i] = m
                moved.append("xy"[i])
        if moved:
            note = f"JPEG measured at ({mx:g}, {my:g}) in the active area"
    return {"area": [aa_w, aa_h], "crop": off + [w, h], "files": len(rs)}, sources, reg, note


def propose():
    rows = load(arg("--in", DEFAULT_OUT))
    write = flag("--write")
    db = json.load(open(DB, encoding="utf-8"))
    idx = Index(db)
    groups = aggregate(rows)
    per_entry = collections.defaultdict(dict)
    meta = {}
    for key, areas in sorted(groups.items()):
        make, model, mode = key
        entry = idx.find(make, model, mode)
        ekey = (entry["make"], entry["model"], entry.get("mode", "")) if entry else (make, model, "")
        sample = next(iter(next(iter(areas.values())).values()))[0]
        meta[ekey] = (sample["canonical_make"], sample["canonical_model"], entry)
        for area, sizes in areas.items():
            for size, rs in sizes.items():
                p, sources, reg, note = proposal(area, size, rs)
                k = (tuple(p["area"]), tuple(p["crop"][2:]))
                old = per_entry[ekey].get(k)
                if old:
                    old[0]["files"] += p["files"]
                    old[1].update(sources)
                else:
                    per_entry[ekey][k] = [p, sources, note]
                print(
                    f"{make} / {model} [{mode}]\tarea {area[0]}x{area[1]}\t-> {p['crop']}\t{p['files']} files"
                    f"\t{dict(sources)}\treg {len(reg)}\t{note}"
                )
    print(f"{len(per_entry)} entries, {sum(len(v) for v in per_entry.values())} image crops")
    if not write:
        return
    added = 0
    for ekey, crops in per_entry.items():
        canon_make, canon_model, entry = meta[ekey]
        if entry is None:
            entry = {
                "make": ekey[0],
                "model": ekey[1],
                "canonical_make": canon_make,
                "canonical_model": canon_model,
            }
            db["cameras"].append(entry)
            added += 1
        items = sorted(crops.values(), key=lambda v: (-v[0]["crop"][2] * v[0]["crop"][3], v[0]["area"]))
        entry["image_crops"] = [v[0] for v in items]
        ver = entry.setdefault("verification", {})
        notes = [n for n in ver.get("notes", []) if not n.startswith("image crops:")]
        srcs = collections.Counter()
        for v in items:
            srcs.update(v[1])
        names = {
            "file": "the format's own crop field",
            "image-size": "the image size the file records",
            "preview": "the embedded full-size JPEG",
            "camera-db": "this entry",
            "active-area": "the active area, which is the recorded size",
        }
        text = "image crops: the camera's own output size, measured from {} files ({})".format(
            sum(v[0]["files"] for v in items), ", ".join(f"{names.get(s, s)}: {n}" for s, n in srcs.most_common())
        )
        extra = [v[2] for v in items if v[2]]
        if extra:
            text += "; " + "; ".join(sorted(set(extra)))
        notes.append(text)
        ver["notes"] = notes
    with open(DB, "w", encoding="utf-8") as f:
        json.dump(db, f, indent=2)
    subprocess.run([BIN, "camera-db", "generate", DB], check=True)
    print(f"wrote {DB}: {len(per_entry)} entries with image crops ({added} new)")


def check():
    rows = load(arg("--in", DEFAULT_OUT))
    bad = []
    covered = collections.Counter()
    for r in rows:
        aa, cr = r["active_area"], r["crop"]
        name = f"{r['file']} ({r['make']} {r['model']} [{r['mode']}])"
        if not (aa[0] <= cr[0] and aa[1] <= cr[1] and cr[0] + cr[2] <= aa[0] + aa[2] and cr[1] + cr[3] <= aa[1] + aa[3]):
            bad.append(f"{name}: crop {cr} outside active area {aa}")
        if (r["width"], r["height"]) != (cr[2], cr[3]):
            bad.append(f"{name}: metadata size {r['width']}x{r['height']} != crop {cr[2]}x{cr[3]}")
        src = r.get("crop_source")
        if src in ("preview", "image-size"):
            want = r["jpeg"] if src == "preview" else r["image_size"]
            if sorted(want) != sorted(cr[2:]):
                bad.append(f"{name}: crop {cr[2]}x{cr[3]} != {src} {want[0]}x{want[1]}")
        if src:
            covered[(r["canonical_make"], r["canonical_model"])] += 1
        g = r.get("rgba")
        if g and "width" in g and [g["width"], g["height"]] != g["expected_half"]:
            bad.append(f"{name}: RGBA {g['width']}x{g['height']} != {g['expected_half']} (half size)")
    for b in bad:
        print(b)
    models = {(r["canonical_make"], r["canonical_model"]) for r in rows}
    print(f"{len(rows)} files, {len(models)} models, {len(covered)} with a camera image crop, {len(bad)} problems")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    {"measure": measure, "propose": propose, "check": check}.get(cmd, lambda: sys.exit(__doc__))()
