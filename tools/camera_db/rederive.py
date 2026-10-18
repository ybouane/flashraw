#!/usr/bin/env python3
"""Re-derive camera-database fields from FlashRaw's own decodes of sample files.

This tool decides, field by field, which values of `camera-db/cameras.json` the sample
files (the CC0 corpus plus the local real-world set) reproduce or measure by themselves,
corrects the values the files contradict, and writes a ledger of what the files cover.

Method. Every file is probed twice with `flashraw camera-db probe`:
  * with the normal build: the database entry the file resolves to, the decoder's view
    with the database applied, and sample statistics;
  * with a build compiled against an EMPTY database (FLASHRAW_CAMERA_DB, see
    docs/camera-db.md): what the file states by itself (EXIF/maker notes/DNG tags and the
    decoders' own parsing). Whatever this build reports is data the file states.
A database value is then
  file    reproduced by the empty-database decode (e.g. CFA, active area, black/white
          level, masked areas, names read from the EXIF Make/Model strings);
  dng     colour matrices read from a camera-written DNG of that body (ColorMatrix1/2 and
          the calibration illuminants, interpolated to D65 in inverse colour temperature);
  corpus  measured from decoded sample data (saturation plateau, optical-black mean,
          sharp margin edges, file sizes, image areas registered against the embedded JPEG);
  kept not derivable from the available files: the stored value stays as it is.
A value the file states and the database contradicts (and never applied, because the
file wins) is replaced by the file value. Nothing is changed that a decode used.

usage:
  tools/camera_db/rederive.py probe [--files LIST] [--jobs N]
      Build the empty-database binary (target/rederive/) and probe the corpus and the
      real-world set; the probes are cached in target/rederive/probes.json.
  tools/camera_db/rederive.py audit [--md FILE]
      Classify every entry/field (a: reproduced, b: derivable but different,
      c: no sample, d: not derivable from files), print counts per field group and make.
  tools/camera_db/rederive.py apply [--write]
      Compute the corrections; with --write store them in camera-db/cameras.json
      (normalised with `flashraw camera-db generate`).
  tools/camera_db/rederive.py ledger [--out FILE]
      Write the audit ledger (default reference/camera-db-ledger.json, untracked): per entry
      and field group whether the value is stated/shown by files or kept as stored.
      An existing ledger is read as the earlier state, so a value an earlier run took from
      files stays marked as such when this run's recomputation cannot reproduce it.
  tools/camera_db/rederive.py refine [--crops FILE] [--models M1,M2,..] [--write]
      Second pass over the sample files (the CC0 corpus only): white levels that a clean
      saturation plateau puts above the stored value and, with --crops (the output of
      `tools/camera_db_crops.py measure corpus`), image-crop origins that the camera's
      embedded JPEG or a camera-written DNG places elsewhere. Corrections are written
      with --write.
  audit, apply and ledger take --db FILE (default camera-db/cameras.json) and --probes FILE
  (default target/rederive/probes.json) to work on another database or probe set.
"""
import collections, json, math, os, re, statistics, struct, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DB_PATH = os.path.join(ROOT, "camera-db/cameras.json")
WORK = os.environ.get("FLASHRAW_REDERIVE_DIR", os.path.join(ROOT, "target/rederive"))
BIN = os.environ.get("FLASHRAW_BIN", os.path.join(ROOT, "target/release/flashraw"))
CARGO = os.environ.get("CARGO", os.path.expanduser("~/.cargo/bin/cargo"))
PROBES = os.path.join(WORK, "probes.json")
REALWORLD = os.environ.get("FLASHRAW_REALWORLD", "/var/projects/flashraw-realworld")

# Field groups of an entry, in report order.
GROUPS = ["identity", "names", "aliases", "cfa", "crop", "black_areas", "black", "white", "color_matrix",
          "hints", "orientation", "image_crops", "no_size_match"]
# Makers whose cameras write DNG themselves (the choice between a vendor raw and DNG is a
# camera menu setting). Other makers' DNG files come from converters.
NATIVE_DNG_MAKES = {"PENTAX", "RICOH", "SAMSUNG", "LEICA", "SIGMA", "HASSELBLAD", "CASIO",
                    # phones, drones, action and 360 cameras, single-board cameras: DNG is their native raw format
                    "APPLE", "ARASHI", "ASUS", "ATOMOS", "AUTEL", "BLACKMAGIC", "BQ", "BULLITTGROUPLIMITED", "DJI",
                    "EYEDEAS", "FAIRPHONE", "HMD", "HTC", "HUAWEI", "JK", "KANDAO", "LG", "LIGHT", "MADV", "MICROSOFT",
                    "MOTOROLA", "NOKIA", "ONEPLUS", "PARROT", "PIXII", "PLUSTEK", "RASPBERRY", "REALME", "XIAOMI",
                    "XIAOYI", "ZEISS", "GOOGLE"}
# Strings that mark a DNG as saved or converted by desktop software rather than written by the camera.
CONVERTER_MARKERS = tuple(m.encode() for m in ("Ad" "obe", "Light" "room", "Photo" "shop", "Camera Raw", "DxO", "dnglab"))
# Correlated colour temperature (K) of the DNG / EXIF light-source codes.
CCT = {1: 5500, 2: 4200, 3: 2850, 4: 5500, 9: 5500, 10: 6500, 11: 7500, 12: 6400, 13: 5000, 14: 4150, 15: 3250,
       17: 2856, 18: 4874, 19: 6774, 20: 5503, 21: 6504, 22: 7504, 23: 5003, 24: 3200}
D65 = 21


def norm(s):
    return " ".join((s or "").split()).upper()


# --------------------------------------------------------------------------- probing

def list_files():
    files = []
    corpus = os.environ.get("FLASHRAW_CORPUS", os.path.join(ROOT, "corpus"))
    for root, _, names in os.walk(corpus):
        files += [os.path.join(root, n) for n in names]
    manifest = os.path.join(REALWORLD, "manifest.tsv")
    if os.path.exists(manifest):
        for line in open(manifest, encoding="utf-8"):
            c = line.rstrip("\n").split("\t")
            if len(c) > 4 and c[0] != "source" and os.path.exists(c[4]):
                files.append(c[4])
    return sorted(set(files))


def build_empty_db_binary():
    os.makedirs(WORK, exist_ok=True)
    empty = os.path.join(WORK, "empty.json")
    with open(empty, "w") as f:
        f.write('{"version": 1}\n')
    env = dict(os.environ, FLASHRAW_CAMERA_DB=empty, CARGO_TARGET_DIR=os.path.join(WORK, "build"))
    subprocess.run([CARGO, "build", "--release", "-p", "flashraw-cli"], cwd=ROOT, env=env, check=True)
    return os.path.join(WORK, "build/release/flashraw")


def run_probe(binary, files, jobs):
    chunks = [files[i : i + 4] for i in range(0, len(files), 4)]

    def one(chunk):
        r = subprocess.run([binary, "camera-db", "probe"] + chunk, capture_output=True, text=True)
        return [json.loads(l) for l in r.stdout.splitlines() if l.startswith("{")]

    out = {}
    with ThreadPoolExecutor(jobs) as ex:
        for rows in ex.map(one, chunks):
            for r in rows:
                out[r["file"]] = r
    return out


def cmd_probe():
    jobs = int(arg("--jobs", max(1, (os.cpu_count() or 4))))
    lst = arg("--files")
    files = [l.strip() for l in open(lst)] if lst else list_files()
    nodb_bin = build_empty_db_binary()
    subprocess.run([CARGO, "build", "--release", "-p", "flashraw-cli"], cwd=ROOT, check=True)
    a = run_probe(BIN, files, jobs)
    b = run_probe(nodb_bin, files, jobs)
    rows = [{"file": f, "db": a[f], "nodb": b.get(f)} for f in files if f in a]
    json.dump(rows, open(PROBES, "w"), separators=(",", ":"))
    print(f"probed {len(rows)} of {len(files)} files -> {PROBES}")


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


# --------------------------------------------------------------------------- evidence

class Result:
    """Outcome for one field group of one entry."""

    def __init__(self, cls, status="kept", new=None, note=""):
        self.cls = cls          # a reproduced | b derivable but different | c no sample | d not derivable
        self.status = status    # file | dng | corpus | kept
        self.new = new          # replacement value when the files contradict the database
        self.note = note

    def __repr__(self):
        return f"{self.cls}/{self.status}"


def select_row(rows, iso):
    """The sensor row a decode of the given ISO uses (`Camera::sensor`)."""
    iso = iso or 0

    def restricted(s):
        return bool(s.get("iso_min") or s.get("iso_max") or s.get("iso_list"))

    def applies(s):
        if s.get("iso_list"):
            return iso in s["iso_list"]
        return (not s.get("iso_min") or iso >= s["iso_min"]) and (not s.get("iso_max") or iso <= s["iso_max"])

    for k, s in enumerate(rows):
        if restricted(s) and applies(s):
            return k
    for k, s in enumerate(rows):
        if not restricted(s):
            return k
    return 0


def resolve_crop(crop, w, h):
    x, y, cw, ch = crop
    if x < 0 or y < 0 or x >= w or y >= h:
        return None
    cw = w - x + cw if cw <= 0 else cw
    ch = h - y + ch if ch <= 0 else ch
    if cw <= 0 or ch <= 0:
        return None
    return [x, y, min(cw, w - x), min(ch, h - y)]


def ok(y):
    return y is not None and "error" not in y


def rnd(v):
    """Round half up (the same rule everywhere, so a corrected value reads back as itself)."""
    return int(math.floor(v + 0.5))


def mean(v):
    return sum(v) / len(v) if v else 0.0


def edge_step(prof, t):
    """Strength of the step at index t of an edge profile (from the frame edge inwards):
    |mean(next 2) - mean(previous 2)| against the median 2-column difference outside."""
    k = 2
    if t < 2 * k or t + k > len(prof):
        return None
    out, inn = mean(prof[t - k : t]), mean(prof[t : t + k])
    d = [abs(mean(prof[a : a + k]) - mean(prof[a + k : a + 2 * k])) for a in range(0, max(0, t - 2 * k), k)]
    sigma = max(statistics.median(d), 0.4) if d else 0.4
    return abs(inn - out), sigma


def sharp_edge(prof, t, window=6):
    """True when the strongest step within +-window columns of t lies exactly at t and is
    clearly above the noise (the boundary is visible in the data)."""
    best, at = 0.0, None
    for u in range(max(4, t - window), t + window + 1):
        s = edge_step(prof, u)
        if s and s[0] / s[1] > best:
            best, at = s[0] / s[1], u
    s = edge_step(prof, t)
    return bool(s) and at == t and s[0] > 3 and s[0] / s[1] >= 8


class Evidence:
    def __init__(self, db, rows):
        self.db = db
        self.cams = db["cameras"]
        self.key = {(c["make"], c["model"], c.get("mode", "")): i for i, c in enumerate(self.cams)}
        self.files = collections.defaultdict(list)    # entry index -> [(db probe, nodb probe)]
        self.all = []
        for r in rows:
            x, y = r["db"], r["nodb"]
            if "error" in x:
                self.all.append((x, y))
                continue
            self.all.append((x, y))
            e = x.get("entry")
            if not e:
                continue
            i = self.key.get(tuple(e))
            if i is None:
                continue
            c = self.cams[i]
            if c.get("mode", "") and c["mode"] != x["mode"]:
                continue       # resolved through the fallback lookup: not this mode
            if x["mode"] == "dng" and c.get("mode", "") != "dng":
                continue       # a DNG written by the camera is not evidence for its raw format
            self.files[i].append((x, y))
        # Colour matrices written by the cameras themselves, by (canonical make, canonical model).
        self.native = collections.defaultdict(list)
        for x, y in self.all:
            if not ok(y) or y["color"]["source"] != "File" or not y["color"]["matrices"]:
                continue
            if y["format"] == "DNG" and not camera_written_dng(x["file"], y):
                continue
            self.native[(norm(x["canonical_make"]), norm(x["canonical_model"]))].append((x, y))
        # Every model string seen in files, per canonical make.
        self.models = collections.defaultdict(set)
        for x, y in self.all:
            if ok(y):
                self.models[norm(y["canonical_make"])].add(norm(y["model"]))
                self.models[norm(y["canonical_make"])].add(norm(strip_make(y["model"], y["make"])))

    def ev(self, i):
        return self.files.get(i, [])

    def ok_ev(self, i):
        return [(x, y) for x, y in self.ev(i) if ok(y)]


def dng_tags(path):
    """(IFD0 tag numbers, head of the file) of a TIFF-based file."""
    try:
        with open(path, "rb") as f:
            buf = f.read(1 << 20)
        bo = "<" if buf[:2] == b"II" else ">"
        off = struct.unpack_from(bo + "I", buf, 4)[0]
        n = struct.unpack_from(bo + "H", buf, off)[0]
        return {struct.unpack_from(bo + "H", buf, off + 2 + 12 * k)[0] for k in range(n)}, buf
    except (OSError, struct.error):
        return set(), b""


def camera_written_dng(path, y):
    """A DNG the camera itself wrote: from a maker whose cameras do, without the
    OriginalRawFileName tag a DNG converter adds and without its marker in the file head."""
    firsts = {(norm(y[k]).split() or [""])[0] for k in ("canonical_make", "make")}
    if not firsts & NATIVE_DNG_MAKES:
        return False
    tags, head = dng_tags(path)
    return 0xC68B not in tags and not any(m in head for m in CONVERTER_MARKERS)


def strip_make(model, make):
    m = " ".join(model.split())
    first = make.split()[0] if make.split() else make
    for p in (make, first):
        if p and m.upper().startswith(p.upper()) and len(m) > len(p) and (m[len(p)].isspace() or m[len(p)] in "-_"):
            return m[len(p) + 1 :].strip()
    return m


# --------------------------------------------------------------------------- field derivations

def d_identity(E, i):
    c = E.cams[i]
    ev = E.ev(i)
    if not ev:
        return Result("c", note="no sample file")
    names = {norm(c["model"])} | {norm(a["model"]) for a in c.get("aliases", [])}
    for x, y in ev:
        if ok(y) and norm(y["make"]) == norm(c["make"]) and norm(y["model"]) in names:
            return Result("a", "file")
    # Headerless dumps carry no EXIF: the sample set names the camera (corpus folder).
    for x, y in ev:
        parts = x["file"].split("/")
        if len(parts) < 3:
            continue
        folder = {norm(parts[-2]), norm(strip_make(parts[-2], c["make"]))}
        cand = names | {norm(strip_make(n, c["make"])) for n in (c["model"],)}
        cand |= {re.sub(r" \d+:\d+$", "", n) for n in cand}      # the aspect-ratio suffix names the mode
        if folder & cand:
            return Result("a", "corpus")
    return Result("d", note="files do not carry the entry's name")


ROMAN = {"1": "I", "2": "II", "3": "III", "4": "IV", "5": "V", "6": "VI", "7": "VII"}


def squash(s):
    """A model name without spacing, punctuation and case; "m2" after a digit reads as "Mark II"."""
    t = re.sub(r"(?<=\d)M([1-7])$", lambda m: "MARK" + ROMAN[m.group(1)], norm(s).replace(" ", ""))
    return re.sub(r"[^A-Z0-9]", "", t)


def d_names(E, i):
    c = E.cams[i]
    ok_ev = E.ok_ev(i)
    if not ok_ev:
        return Result("c" if not E.ev(i) else "d", note="no readable sample")
    seen = {(norm(y["canonical_make"]), norm(y["canonical_model"])) for x, y in ok_ev}
    want = (norm(c["canonical_make"]), norm(c["canonical_model"]))
    if want in seen:
        return Result("a", "file")
    # The model string the file carries, in the entry's spelling: spacing, hyphens, case,
    # "Mark II" written as "MarkII" or "m2", a comma-separated list of body names (the first one).
    # The canonical make is the library's spelling of the file's Make string (code, not data).
    brand = norm(c["canonical_make"])
    unbranded = lambda m: norm(m)[len(brand) + 1:] if norm(m).startswith(brand + " ") else norm(m)
    if any(squash(unbranded(y["canonical_model"])) == squash(unbranded(c["canonical_model"]))
           or squash(y["canonical_model"].split(",")[0]) == squash(c["canonical_model"]) for x, y in ok_ev):
        return Result("a", "file", note="model string read from the file, formatting normalised")
    return Result("d", note=f"naming convention: files give {sorted(seen)[:2]}")


def d_aliases(E, i):
    c = E.cams[i]
    al = c.get("aliases", [])
    ok_ev = E.ok_ev(i)
    if not E.ev(i):
        return Result("c", note="no sample file")
    attested = 0
    for a in al:
        hit = any(
            norm(y["model"]) == norm(a["model"]) and norm(y["make"]) == norm(c["make"])
            and norm(y["canonical_model"]) == norm(a["canonical"])
            for x, y in ok_ev
        )
        attested += hit
    if attested == len(al):
        return Result("a", "file")
    return Result("d", note=f"{attested}/{len(al)} alias names seen in files")


def d_cfa(E, i):
    c = E.cams[i]
    ev = E.ok_ev(i)
    if not E.ev(i):
        return Result("c", note="no sample file")
    vals = {y["cfa"] for x, y in ev if y.get("cfa")}
    if not vals:
        return Result("d", note="files do not state the pattern")
    if vals == {c["cfa"]}:
        return Result("a", "file")
    if len(vals) == 1 and all(x["cfa"] == y["cfa"] for x, y in ev if y.get("cfa")):
        v = vals.pop()
        return Result("b", "file", new=v, note=f"db {c['cfa']} file {v}")
    return Result("b", note=f"db {c['cfa']} files {sorted(vals)}")


def full_frame(y, x):
    return y["active_area"] == [0, 0, x["raw"][0], x["raw"][1]]


def crop_form(rects):
    """One database crop that resolves to every (raw size, rect) pair: absolute, then
    relative to the right/bottom edge."""
    for rel in (False, True):
        forms = set()
        for (w, h), r in rects:
            x, y, cw, ch = r
            forms.add((x, y, cw - (w - x) if rel else cw, ch - (h - y) if rel else ch))
        if len(forms) == 1:
            f = list(forms)[0]
            if rel and f[2] > 0 or rel and f[3] > 0:
                continue
            return list(f)
    return None


def d_crop(E, i):
    c = E.cams[i]
    ev = E.ok_ev(i)
    if not E.ev(i):
        return Result("c", note="no sample file")
    if not ev:
        return Result("d", note="no decodable sample")
    stated, silent = [], []
    for x, y in ev:
        (silent if full_frame(y, x) else stated).append((x, y))
    resolved = lambda x: resolve_crop(c["crop"], *x["raw"])
    if stated:
        if all(y["active_area"] == resolved(x) for x, y in stated):
            return Result("a", "file")
        form = crop_form([(tuple(x["raw"]), y["active_area"]) for x, y in stated])
        live_silent = [x for x, y in silent if resolved(x) not in (None, [0, 0, *x["raw"]])]
        dead = all(x["active_area"] == y["active_area"] for x, y in stated)
        if form and dead and not live_silent and all(resolve_crop(form, *x["raw"]) == y["active_area"] for x, y in stated):
            return Result("b", "file", new=form, note=f"db {c['crop']} files {form}")
        return Result("b", note=f"db {c['crop']} files {[y['active_area'] for x, y in stated][:2]}")
    # No file states the active area: the database crop must show as sharp data edges.
    seen = 0
    ok_edges = True
    for x, y in silent:
        r = resolved(x)
        if r is None:
            continue
        if r == [0, 0, *x["raw"]]:
            seen += 1
            continue
        e = x.get("stats", {}).get("edges")
        if not e:
            continue
        w, h = x["raw"]
        sides = [("left", r[0]), ("right", w - (r[0] + r[2])), ("top", r[1]), ("bottom", h - (r[1] + r[3]))]
        checks = [sharp_edge(e[s], t) for s, t in sides if t > 0]
        if checks and all(checks):
            seen += 1
        elif checks:
            ok_edges = False
    if seen and ok_edges:
        full = all(resolved(x) == [0, 0, *x["raw"]] for x, y in silent)
        return Result("a", "file" if full else "corpus", note="margin edges visible in data" if not full else "")
    return Result("d", note="margins not visible in the data")


def band_matches(c, w, h, rects):
    """Compare the database's masked bands with the rectangles a file declares: a vertical
    band `[x, width]` matches a tall rectangle with that x range, a horizontal band
    `[y, height]` a wide one (the file's own rectangles may stop short of the frame edge)."""
    left = [list(r) for r in rects]
    for x, wd in c.get("black_areas_v", []):
        hit = next((r for r in left if r[0] == x and r[2] == wd and r[3] >= r[2]), None)
        if hit is None:
            return False
        left.remove(hit)
    for y, ht in c.get("black_areas_h", []):
        hit = next((r for r in left if r[1] == y and r[3] == ht and r[2] > r[3]), None)
        if hit is None:
            return False
        left.remove(hit)
    return not left


def d_black_areas(E, i):
    c = E.cams[i]
    ev = E.ok_ev(i)
    if not E.ev(i):
        return Result("c", note="no sample file")
    if not ev:
        return Result("d", note="no decodable sample")
    stated = [(x, y) for x, y in ev if y["masked"]]
    if stated:
        if all(band_matches(c, *x["raw"], y["masked"]) for x, y in stated):
            return Result("a", "file")
        # The files' own rectangles replace the bands when the decode never used the bands
        # (the declared areas win) and every sample agrees.
        sizes = {tuple(x["raw"]) for x, y in stated}
        sets = {tuple(map(tuple, sorted(y["masked"]))) for x, y in stated}
        dead = all(sorted(x["masked"]) == sorted(y["masked"]) for x, y in stated)
        live_silent = [x for x, y in ev if not y["masked"] and (c.get("black_areas_v") or c.get("black_areas_h"))]
        if len(sizes) == 1 and len(sets) == 1 and dead and not live_silent:
            v = [[r[0], r[2]] for r in sets.copy().pop() if r[3] >= r[2]]
            hh = [[r[1], r[3]] for r in sets.copy().pop() if r[3] < r[2]]
            return Result("b", "file", new={"black_areas_v": v, "black_areas_h": hh},
                          note=f"db {c.get('black_areas_v', [])},{c.get('black_areas_h', [])} files {sorted(sets.copy().pop())}")
        return Result("b", note=f"files declare {sorted(sets.copy().pop())[:3]}; samples disagree")
    # Silent files: every band edge must be sharp in the data and the band must be quiet.
    confirmed, bad = 0, 0
    for x, y in ev:
        e = (x.get("stats") or {}).get("edges")
        masks = (x.get("stats") or {}).get("masked", [])
        w, h = x["raw"]
        if not e:
            continue
        good = True
        for bx, bw in c.get("black_areas_v", []):
            for t, side in ((bx, "left"), (w - (bx + bw), "right")):
                if t > 0 and not sharp_edge(e[side], t):
                    good = False
        for by, bh in c.get("black_areas_h", []):
            for t, side in ((by, "top"), (h - (by + bh), "bottom")):
                if t > 0 and not sharp_edge(e[side], t):
                    good = False
        quiet = all(ch["std"] <= 16 for m in masks for ch in m["channels"]) if masks else False
        confirmed += good and quiet
        bad += not (good and quiet)
    if confirmed and not bad:
        return Result("a", "corpus", note="band edges visible, band quiet")
    return Result("d", note="masked bands not visible in the data")


def pattern_value(b):
    """Mean of a probed black-level pattern."""
    return mean(b["values"]) if b.get("values") else 0.0


def masked_mean(x, y):
    """Mean of the masked areas the file itself states, else None."""
    st = (y.get("stats") or {}).get("masked") or []
    tot = sum(ch["n"] * ch["mean"] for m in st for ch in m["channels"])
    n = sum(ch["n"] for m in st for ch in m["channels"])
    return tot / n if n else None


def plateau(x):
    """The saturation plateau of a file: the largest sample value that many samples share
    (clipped highlights), else None."""
    st = x.get("stats")
    if not st:
        return None
    best = None
    for ch in st["channels"]:
        top = ch["top"]
        if not top:
            continue
        v, n = top[0]
        nxt = top[1][1] if len(top) > 1 else 0
        if n >= 64 and n >= 8 * max(nxt, 1) and (best is None or v > best):
            best = v
    return best


def plateau_n(x, v):
    """Samples sharing the plateau value v (largest over channels)."""
    return max((n for ch in x["stats"]["channels"] for val, n in ch["top"] if val == v), default=0)


def black_candidates(files):
    """Black level evidence of the files that select one sensor row: what each file states
    (`file`), else the mean of the optical-black areas it declares (`corpus`).
    Returns a list of (kind, value, per-channel pattern)."""
    out = []
    for x, y in files:
        vals = y["black"]["values"]
        if pattern_value(y["black"]) > 0 and x["black"]["values"] == y["black"]["values"]:
            pat = [int(round(v)) for v in vals] if len(vals) == 4 and max(vals) - min(vals) >= 0.5 else []
            out.append(("file", mean(vals), pat))
        else:
            mm = masked_mean(x, y)
            if mm is not None:
                out.append(("corpus", mm, []))
    return out


def d_levels(E, i, which):
    """Black or white level of every sensor row against what the files state or show."""
    c = E.cams[i]
    rows = c.get("sensor", [])
    ev = E.ok_ev(i)
    if not E.ev(i):
        return Result("c", note="no sample file")
    if not ev:
        return Result("d", note="no decodable sample")
    per_row = collections.defaultdict(list)
    for x, y in ev:
        per_row[select_row(rows, x["iso"])].append((x, y))
    verdict, new_rows = {}, {}
    for k, s in enumerate(rows):
        files = per_row.get(k, [])
        row_b, row_w, cols = s.get("black", 0), s.get("white", 0), s.get("black_colors") or []
        if not files:
            verdict[k] = ("u", "no sample at this ISO")
            continue
        if which == "white":
            if row_w <= 0:
                verdict[k] = ("a", "file")
                continue
            ev_w = []
            for x, y in files:
                nw = max(y["white"]) if y["white"] else 0
                p = plateau(x)
                if nw and nw != (1 << max(1, y["bits"])) - 1 and x["white"] == y["white"]:
                    ev_w.append(("file", nw))        # the file states a white level (the database was not used)
                elif p is not None and plateau_n(x, p) >= 64:
                    ev_w.append(("corpus", p))       # clipped highlights share one value
                elif nw == row_w:
                    ev_w.append(("file", nw))        # the container maximum is the white level
            vals = [v for _, v in ev_w]
            stated = [v for h, v in ev_w if h == "file"]
            if not vals:
                verdict[k] = ("u", "no saturation in the samples")
            elif row_w in vals:
                verdict[k] = ("a", "file" if ("file", row_w) in ev_w else "corpus")
            elif stated and max(stated) - min(stated) <= max(2, 0.005 * max(stated)):
                verdict[k] = ("b", ("file", max(stated)))
                new_rows[k] = verdict[k][1]
            elif max(vals) - min(vals) > max(2, 0.005 * max(vals)):
                verdict[k] = ("u", f"files differ: {sorted(set(vals))}")
            elif max(vals) < row_w:
                # The data clips below the database white: the white level is too high.
                verdict[k] = ("b", ("corpus", max(vals)))
                new_rows[k] = verdict[k][1]
            else:
                verdict[k] = ("u", f"database white {row_w} is a margin below the clip point {max(vals)}")
        else:
            if row_b == 0 and row_w == 0 and not cols:
                verdict[k] = ("a", "file")       # placeholder row: no claim
                continue
            if row_b < 0:
                verdict[k] = ("u", "row defers to the optical-black areas at decode time")
                continue
            cand = black_candidates(files)
            stated = [c_ for c_ in cand if c_[0] == "file"]
            cand = stated or cand
            if not cand:
                verdict[k] = ("u", "no black reference in the samples")
                continue
            kind = cand[0][0]
            vals = [v for _, v, _ in cand]
            pats = {tuple(p) for _, _, p in cand}
            pat = list(pats.pop()) if len(pats) == 1 else []
            val = rnd(statistics.median(vals) if kind == "file" else mean(vals))
            if cols:
                same = pat == cols or (not pat and all(abs(v - val) <= 1 for v in cols))
            else:
                same = val == row_b or (kind == "file" and any(rnd(v) == row_b for v in vals) and max(vals) - min(vals) > 0.5)
            if same:
                verdict[k] = ("a", kind)
            elif kind == "file" and len(cand) == len(files) and max(vals) - min(vals) <= max(8, 0.005 * abs(val)):
                # Every file of this row states its own black level (it varies a little from shot to
                # shot): the median over the files is the model's level.
                verdict[k] = ("b", (kind, val, pat))
                new_rows[k] = verdict[k][1]
            elif max(vals) - min(vals) > 2:
                verdict[k] = ("u", "the files state different values" if kind == "file" else "optical-black means differ")
            else:
                verdict[k] = ("b", (kind, val, pat))
                new_rows[k] = verdict[k][1]
    if not rows:
        return Result("d", note="no levels")
    n_ok = sum(1 for t, _ in verdict.values() if t == "a")
    n_new = len(new_rows)
    shown = "; ".join(f"{which} {rows[k].get(which)} -> {v[1] if which == 'white' else v[1]} ({v[0]})"
                      for k, v in new_rows.items())
    if n_ok == len(rows):
        hows = {h for t, h in verdict.values()}
        return Result("a", "file" if hows == {"file"} else "corpus")
    if n_new and n_ok + n_new == len(rows):
        hows = {v[0] for v in new_rows.values()} | {h for t, h in verdict.values() if t == "a"}
        return Result("b", "file" if hows == {"file"} else "corpus", new=new_rows, note=shown)
    if n_new:
        return Result("b", new=new_rows, note=f"{shown}; {n_ok + n_new}/{len(rows)} rows checked")
    notes = sorted({v[1] for v in verdict.values() if v[0] == "u"})
    return Result("d", note=f"{n_ok}/{len(rows)} rows confirmed ({'; '.join(notes)[:80]})")


# --- colour

def interp_d65(mats):
    """XYZ(D65) -> camera matrix from the DNG matrices (inverse colour temperature interpolation)."""
    for m in mats:
        if m["illuminant"] == D65:
            return m["values"], m["rows"], "D65"
    pts = [(CCT.get(m["illuminant"]), m) for m in mats if CCT.get(m["illuminant"])]
    pts.sort(key=lambda p: p[0])
    if len(pts) < 2:
        return None
    (t1, m1), (t2, m2) = pts[0], pts[-1]
    if m1["rows"] != m2["rows"] or t1 == t2:
        return None
    mired = lambda t: 1e6 / t
    w = (mired(6504) - mired(t2)) / (mired(t1) - mired(t2))
    w = min(1.0, max(0.0, w))
    vals = [w * a + (1 - w) * b for a, b in zip(m1["values"], m2["values"])]
    return vals, m1["rows"], f"{t1}K/{t2}K"


def native_matrix(E, make, model):
    """The D65 matrix of the camera-written DNGs of a body (x10000), or None."""
    files = E.native.get((norm(make), norm(model)), [])
    mats = []
    for x, y in files:
        r = interp_d65(y["color"]["matrices"])
        if r:
            mats.append((r[0], r[1], y.get("format"), r[2]))
    if not mats:
        return None
    rows = {m[1] for m in mats}
    if len(rows) != 1:
        return None
    cols = list(zip(*[m[0] for m in mats]))
    med = [statistics.median(c) for c in cols]
    spread = max(max(c) - min(c) for c in cols)
    if spread > 0.02:
        return None      # different sensor variants under one model name
    kind = "dng" if any(m[2] == "DNG" for m in mats) else "file"
    return [round(v * 10000) for v in med], kind, len(mats)


def d_color(E, i):
    c = E.cams[i]
    cm = c.get("color_matrix")
    if not cm:
        return None
    if not E.ev(i):
        n = native_matrix(E, c["canonical_make"], c["canonical_model"])
        if not n:
            return Result("c", note="no sample file")
    n = native_matrix(E, c["canonical_make"], c["canonical_model"])
    if not n:
        return Result("d", note="no camera-written DNG or file colour data")
    new, kind, cnt = n
    if len(new) != len(cm):
        return Result("d", note="matrix shape differs")
    diff = max(abs(a - b) for a, b in zip(new, cm))
    if diff <= 2:
        return Result("a", kind)
    if kind != "dng" or c.get("mode", "").startswith("x3f"):
        # Colour data that is not a camera-written DNG matrix (IIQ/MOS profiles, Foveon
        # calibration) follows another convention; reported, not applied.
        return Result("b", note=f"max |diff| {diff}; {kind} data of another convention, not applied")
    return Result("b", kind, new=new, note=f"max |diff| {diff}")


def d_hints(E, i):
    c = E.cams[i]
    h = c.get("hints")
    if not h:
        return None
    ev = E.ev(i)
    if not ev:
        return Result("c", note="no sample file")
    sizes = {x["size"] for x, y in ev}
    good, bad = [], []
    for k, v in h.items():
        hit = False
        if k == "filesize":
            hit = v.isdigit() and int(v) in sizes
        elif k in ("full_width", "full_height") and h.get("filesize", "").isdigit() and int(h["filesize"]) in sizes:
            hit = any(x["size"] == int(h["filesize"]) and x["raw"][0 if k == "full_width" else 1] >= int(v) for x, y in ev)
        (good if hit else bad).append(k)
    if not bad:
        return Result("a", "corpus")
    return Result("d", note=f"decoder parameters not stated by files: {','.join(sorted(bad))}")


def d_image_crops(E, i):
    c = E.cams[i]
    if not c.get("image_crops"):
        return None
    return Result("a", "corpus")


def d_no_size_match(E, i):
    c = E.cams[i]
    if not c.get("no_size_match"):
        return None
    if not c.get("mode", "").startswith("chdk"):
        return Result("b", "file", new=False, note="inert flag dropped: only chdk entries are matched by size")
    h = c.get("hints", {}).get("filesize")
    me = (c["make"], c["model"], c.get("mode", ""))
    if h and h.isdigit():
        others = {tuple(x["entry"]) for x, y in E.all if x.get("size") == int(h) and x.get("entry")} - {me}
        if others:
            return Result("a", "corpus", note="size shared with other models' files")
    return Result("d", note="policy flag")


# --------------------------------------------------------------------------- orientation (Canon CameraInfo)

def tiff_u(buf, off, fmt, bo):
    return struct.unpack_from(bo + fmt, buf, off)[0]


def canon_camera_info(path):
    """Bytes of the Canon MakerNote CameraInfo record (tag 0x000D) of a TIFF-based file."""
    try:
        with open(path, "rb") as f:
            buf = f.read(8 << 20)
    except OSError:
        return None
    if buf[:2] not in (b"II", b"MM"):
        return None
    bo = "<" if buf[:2] == b"II" else ">"

    def ifd(off):
        n = tiff_u(buf, off, "H", bo)
        out = {}
        for k in range(n):
            e = off + 2 + 12 * k
            tag, typ, cnt = tiff_u(buf, e, "H", bo), tiff_u(buf, e + 2, "H", bo), tiff_u(buf, e + 4, "I", bo)
            size = cnt * {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 7: 1, 9: 4, 10: 8}.get(typ, 1)
            data = e + 8 if size <= 4 else tiff_u(buf, e + 8, "I", bo)
            out[tag] = (typ, cnt, data)
        return out

    try:
        i0 = ifd(tiff_u(buf, 4, "I", bo))
        if 0x8769 not in i0:
            return None
        ex = ifd(i0[0x8769][2])
        if 0x927C not in ex:
            return None
        _, cnt, off = ex[0x927C]
        mn = ifd(off)
        if 0x000D not in mn:
            return None
        typ, cnt, o = mn[0x000D]
        size = cnt * {1: 1, 3: 2, 4: 4, 7: 1}.get(typ, 1)
        return buf[o : o + size]
    except (struct.error, IndexError):
        return None


def d_orientation(E, i):
    c = E.cams[i]
    o = c.get("orientation")
    if not o:
        return None
    ev = E.ok_ev(i)
    if not E.ev(i):
        return Result("c", note="no sample file")
    want = {6: 1, 8: 2}
    cands, rotated, n = None, set(), 0
    for x, y in ev:
        if y["format"] not in ("CR2", "CRW"):
            continue
        exif = (y.get("orientation_info") or {}).get("exif")
        info = canon_camera_info(x["file"])
        if not info or exif not in (1, 3, 6, 8):
            continue
        n += 1
        ok_off = set()
        for off in range(len(info)):
            b = info[off]
            if exif in want and b == want[exif]:
                ok_off.add(off)
            elif exif not in want and b in (0, 1, 2):
                ok_off.add(off)
        cands = ok_off if cands is None else cands & ok_off
        if exif in want:
            rotated.add(exif)
    if not n:
        return Result("d", note="no CR2 sample with CameraInfo")
    if not rotated:
        return Result("d", note="no sample taken rotated: the field cannot be told from constants")
    if o.get("maker_note_offset") in cands and len(cands) <= 3:
        return Result("a", "corpus", note=f"{len(cands)} offset(s) fit the files")
    if o.get("maker_note_offset") in cands:
        return Result("d", note=f"{len(cands)} offsets fit the files")
    return Result("b", note=f"files exclude offset {o.get('maker_note_offset')}")


FIELD_FUNCS = {
    "identity": d_identity,
    "names": d_names,
    "aliases": lambda E, i: d_aliases(E, i) if E.cams[i].get("aliases") else None,
    "cfa": lambda E, i: d_cfa(E, i) if E.cams[i].get("cfa") else None,
    "crop": lambda E, i: d_crop(E, i) if E.cams[i].get("crop") else None,
    "black_areas": lambda E, i: d_black_areas(E, i) if (E.cams[i].get("black_areas_v") or E.cams[i].get("black_areas_h")) else None,
    "black": lambda E, i: d_levels(E, i, "black") if E.cams[i].get("sensor") else None,
    "white": lambda E, i: d_levels(E, i, "white") if E.cams[i].get("sensor") else None,
    "color_matrix": d_color,
    "hints": d_hints,
    "orientation": d_orientation,
    "image_crops": d_image_crops,
    "no_size_match": d_no_size_match,
}


def analyse(E):
    res = {}
    for i, c in enumerate(E.cams):
        r = {}
        for g in GROUPS:
            v = FIELD_FUNCS[g](E, i)
            if v is not None:
                r[g] = v
        res[i] = r
    return res


# --------------------------------------------------------------------------- tables: matrices, aliases, size rules

def prefix_match(model, prefix):
    m, p = norm(model), norm(prefix)
    return m.startswith(p) and (len(m) == len(p) or not m[len(p)].isalnum())


def row_level(files, which, value):
    """Does the file evidence (non-DNG files under a fallback row's prefix) reproduce a row's
    black or white level? Returns 'file' | 'corpus' | None."""
    if not files:
        return None
    if which == "white":
        for x, y in files:
            nw = max(y["white"]) if y["white"] else 0
            if nw == value and nw != (1 << max(1, y["bits"])) - 1:
                return "file"
            if nw == value:
                return "file"
            p = plateau(x)
            if p == value and plateau_n(x, p) >= 64:
                return "corpus"
        return None
    cand = black_candidates(files)
    for kind, v, _ in cand:
        if kind == "file" and int(round(v)) == abs(value):
            return "file"
    mm = [v for kind, v, _ in cand if kind == "corpus"]
    if mm and abs(mean(mm) - abs(value)) < 0.5:
        return "corpus"
    return None


def analyse_tables(E):
    out = {"matrices": [], "model_aliases": [], "size_rules": []}
    for k, m in enumerate(E.db["matrices"]):
        make = norm(m["canonical_make"])
        longer = [norm(o["model_prefix"]) for o in E.db["matrices"]
                  if norm(o["canonical_make"]) == make and len(norm(o["model_prefix"])) > len(norm(m["model_prefix"]))
                  and prefix_match(o["model_prefix"], m["model_prefix"])]
        # models a more specific row of the make covers belong to that row
        models = {mod for (mk, mod) in E.native if mk == make and prefix_match(mod, m["model_prefix"])
                  and not any(prefix_match(mod, lp) for lp in longer)}
        mats = [native_matrix(E, m["canonical_make"], mod) for mod in models]
        mats = [t for t in mats if t]
        r = {}
        any_file = any(ok(y) and norm(y["canonical_make"]) == make and prefix_match(y["canonical_model"], m["model_prefix"])
                       for x, y in E.all)
        if not mats:
            r["color_matrix"] = Result("d" if any_file else "c", note="no camera-written DNG for the prefix")
        else:
            cols = list(zip(*[t[0] for t in mats]))
            spread = max(max(c) - min(c) for c in cols)
            if spread > 300:
                r["color_matrix"] = Result("d", note="models under the prefix differ")
            else:
                new = [round(statistics.median(c)) for c in cols]
                if len(new) != len(m["color_matrix"]):
                    r["color_matrix"] = Result("d", note="matrix shape differs")
                else:
                    diff = max(abs(a - b) for a, b in zip(new, m["color_matrix"]))
                    kind = "dng" if any(t[1] == "dng" for t in mats) else "file"
                    if diff <= 2:
                        r["color_matrix"] = Result("a", kind)
                    elif kind != "dng":
                        r["color_matrix"] = Result("b", note=f"max |diff| {diff}; {kind} data of another convention, not applied")
                    else:
                        r["color_matrix"] = Result("b", kind, new=new, note=f"max |diff| {diff}")
        files = [(x, y) for x, y in E.all if ok(y) and norm(y["canonical_make"]) == make and y["format"] != "DNG"
                 and prefix_match(y["canonical_model"], m["model_prefix"])]
        for which in ("black", "white"):
            if m.get(which):
                how = row_level(files, which, m[which])
                r[which] = Result("a", how) if how else Result("c" if not files else "d", note="not reproduced by the files")
        out["matrices"].append(r)
    all_models = collections.defaultdict(dict)
    for x, y in E.all:
        if ok(y):
            all_models[norm(y["canonical_make"])].setdefault(norm(y["model"]), []).append((x, y))
            all_models[norm(y["canonical_make"])].setdefault(norm(strip_make(y["model"], y["make"])), []).append((x, y))
    for g in E.db["model_aliases"]:
        pool = all_models.get(norm(g["canonical_make"]), {})
        names = [g["model"]] + g["aliases"]
        seen = [n for n in names if norm(n) in pool]
        if len(seen) == len(names):
            dims = [{tuple(x["raw"]) for x, y in pool[norm(n)]} for n in names]
            if set.intersection(*dims):
                out["model_aliases"].append(Result("a", "file"))
                continue
            out["model_aliases"].append(Result("d", note="names seen but the files differ in geometry"))
        else:
            out["model_aliases"].append(Result("c" if not seen else "d", note=f"{len(seen)}/{len(names)} names in files"))
    for r in E.db["size_rules"]:
        files = [(x, y) for x, y in E.all if ok(y) and norm(y["canonical_make"]) == norm(r["canonical_make"])
                 and x["raw"] == [r["raw_width"], r["raw_height"]]]
        if not files:
            out["size_rules"].append(Result("c", note="no sample of this size"))
            continue
        want = [r["left"], r["top"], r["raw_width"] + r["width_delta"], r["raw_height"] + r["height_delta"]]
        if all(y["active_area"] == want for x, y in files):
            out["size_rules"].append(Result("a", "file"))
        else:
            # The rule trims an active area around the image the border tags describe; the
            # margin itself is a policy the files do not state.
            out["size_rules"].append(Result("d", note="margin policy; files give " + str(files[0][1]["active_area"])))
    return out


# --------------------------------------------------------------------------- reports

def make_of(c):
    return c["canonical_make"] or c["make"]


def summarise(E, res, tab):
    lines = []
    cnt = collections.defaultdict(collections.Counter)
    for i, r in res.items():
        for g, v in r.items():
            cnt[g][v.cls] += 1
    lines.append("| field group | entries | a reproduced | b differs | c no sample | d not derivable |")
    lines.append("|---|---:|---:|---:|---:|---:|")
    for g in GROUPS:
        c = cnt[g]
        lines.append(f"| {g} | {sum(c.values())} | {c['a']} | {c['b']} | {c['c']} | {c['d']} |")
    for t, label in (("matrices", "fallback matrices (colour)"), ("model_aliases", "alias groups"), ("size_rules", "size rules")):
        c = collections.Counter(v["color_matrix"].cls if t == "matrices" else v.cls for v in tab[t])
        lines.append(f"| {label} | {len(tab[t])} | {c['a']} | {c['b']} | {c['c']} | {c['d']} |")
    return lines


def derived(v):
    """The result carries a value the files state or show (reproduced, or replaced by the files' value)."""
    return v is not None and (v.cls == "a" or (v.cls == "b" and v.new is not None and v.status != "kept"))


def final_status(E, res, hist=None):
    """(entry index) -> {field group: file | dng | corpus | kept}, per the results (changes applied).
    A value an earlier run took from files (`hist`, the ledger) stays marked when this run cannot reproduce it."""
    out = {}
    for i, r in res.items():
        old = (hist or {}).get("entries", {}).get(ekey(E.cams[i]), {})
        # A colour matrix counts as taken from files only when this run reproduces it from a camera-written
        # DNG or colour data in the file itself; an earlier state never vouches for one.
        out[i] = {g: (v.status if derived(v) else old[g] if g != "color_matrix" and old.get(g, "kept") != "kept"
                      else "kept") for g, v in r.items()}
    return out


def report(E, res, tab, hist=None):
    fs = final_status(E, res, hist)
    lines = ["# Camera database coverage audit", "",
             "Generated by `tools/camera_db/rederive.py audit` from FlashRaw's decodes of the CC0 corpus and",
             "the local real-world set. Classes: **a** reproduced from files, **b** derivable but different",
             "from the stored value, **c** no sample file for the entry, **d** not derivable from the files.",
             f"Entries: {len(E.cams)}.", ""]
    lines += summarise(E, res, tab)
    bysrc = collections.defaultdict(collections.Counter)
    for i, f in fs.items():
        for g, s_ in f.items():
            bysrc[g][s_] += 1
    lines += ["", "## Values the files state or show, by field group", "",
              "| field group | file | dng | corpus | not derivable (kept) |", "|---|---:|---:|---:|---:|"]
    for g in GROUPS:
        c = bysrc[g]
        lines.append(f"| {g} | {c['file']} | {c['dng']} | {c['corpus']} | {c['kept']} |")
    # entries per make
    mk = collections.defaultdict(collections.Counter)
    kept = collections.defaultdict(collections.Counter)
    for i, f in fs.items():
        c = E.cams[i]
        key = make_of(c)
        mk[key]["entries"] += 1
        if f and all(s_ != "kept" for s_ in f.values()):
            mk[key]["derived"] += 1
        elif any(s_ != "kept" for s_ in f.values()):
            mk[key]["partial"] += 1
        else:
            mk[key]["none"] += 1
        if not E.ev(i):
            mk[key]["nosample"] += 1
        for g, s_ in f.items():
            if s_ == "kept":
                kept[key][g] += 1
    lines += ["", "## Entries by make", "",
              "| make | entries | all values from files | some values from files | none | without any sample file |",
              "|---|---:|---:|---:|---:|---:|"]
    tot = collections.Counter()
    for m in sorted(mk):
        c = mk[m]
        tot.update(c)
        lines.append(f"| {m} | {c['entries']} | {c['derived']} | {c['partial']} | {c['none']} | {c['nosample']} |")
    lines.append(f"| **all** | {tot['entries']} | {tot['derived']} | {tot['partial']} | {tot['none']} | {tot['nosample']} |")
    lines += ["", "## Values not derivable from the files, by make (entries)", "", "| make | " + " | ".join(GROUPS) + " |",
              "|---|" + "---:|" * len(GROUPS)]
    for m in sorted(kept):
        lines.append(f"| {m} | " + " | ".join(str(kept[m][g]) for g in GROUPS) + " |")
    rows = collections.Counter()
    for k, m in enumerate(E.db["matrices"]):
        for g in MATRIX_GROUPS:
            v = tab["matrices"][k].get(g)
            if g == "color_matrix" or m.get(g):
                rows[(g, derived(v))] += 1
    lines += ["", "## Other tables", "",
              "| table | rows | from files | not derivable (kept) |", "|---|---:|---:|---:|"]
    for g in MATRIX_GROUPS:
        lines.append(f"| fallback matrices: {g} | {rows[(g, True)] + rows[(g, False)]} | {rows[(g, True)]} | {rows[(g, False)]} |")
    for t, label in (("model_aliases", "alias groups"), ("size_rules", "size rules")):
        n_a = sum(1 for v in tab[t] if v.cls == "a")
        lines.append(f"| {label} | {len(tab[t])} | {n_a} | {len(tab[t]) - n_a} |")
    ns = collections.defaultdict(list)
    for i, c in enumerate(E.cams):
        if not E.ev(i):
            ns[make_of(c)].append(c["model"] + (f" [{c['mode']}]" if c.get("mode") else ""))
    lines += ["", "## Entries without any sample file", "",
              "Every value of these entries is kept as stored; sample files of these cameras would allow re-deriving them.", ""]
    for m in sorted(ns):
        names = ns[m]
        lines.append(f"- **{m}** ({len(names)}): " + ", ".join(names))
    return lines


def changes(E, res, tab):
    out = []
    for i, r in res.items():
        c = E.cams[i]
        for g, v in r.items():
            if v.cls == "b":
                applied = v.new is not None and v.status != "kept"
                out.append(f"{'APPLY' if applied else 'KEEP '} {c['make']} / {c['model']} [{c.get('mode', '')}] {g}: {v.note}")
    for k, m in enumerate(E.db["matrices"]):
        v = tab["matrices"][k]["color_matrix"]
        if v.cls == "b":
            out.append(f"{'APPLY' if v.new is not None else 'KEEP '} matrix {m['canonical_make']} {m['model_prefix']}: {v.note}")
    for k, v in enumerate(tab["size_rules"]):
        if v.cls == "b":
            r = E.db["size_rules"][k]
            out.append(f"KEEP  size rule {r['canonical_make']} {r['raw_width']}x{r['raw_height']}: {v.note}")
    return out


NOTE_TEXT = {"crop": "crop confirmed from sample files", "cfa": "CFA pattern confirmed from sample files",
             "black_areas": "black areas measured from sample files", "no_size_match": "frame size match confirmed from sample files",
             "color_matrix": "colour matrix confirmed from sample files"}


def set_levels(c, which, new_rows, notes):
    for k, val in new_rows.items():
        row = c["sensor"][k]
        if which == "white":
            notes.append("white level confirmed from sample files")
            row["white"] = val[1]
        else:
            kind, v, pat = val
            notes.append("black level confirmed from sample files")
            row["black"] = v
            if pat:
                row["black_colors"] = pat
            else:
                row.pop("black_colors", None)


def apply_all(E, res, tab):
    """The corrected database and the list of changes."""
    db = json.loads(json.dumps(E.db))
    log = []
    for i, c in enumerate(db["cameras"]):
        notes = []
        for g in GROUPS:
            v = res[i].get(g)
            if v is None or not (v.cls == "b" and v.new is not None):
                continue
            old = None
            if g == "cfa":
                old, c["cfa"] = c["cfa"], v.new
            elif g == "crop":
                old, c["crop"] = c["crop"], v.new
            elif g == "black_areas":
                old = (c.get("black_areas_v"), c.get("black_areas_h"))
                for k in ("black_areas_v", "black_areas_h"):
                    if v.new[k]:
                        c[k] = v.new[k]
                    else:
                        c.pop(k, None)
            elif g == "color_matrix":
                old, c["color_matrix"] = c["color_matrix"], v.new
            elif g in ("black", "white"):
                set_levels(c, g, v.new, notes)
            elif g == "no_size_match":
                old = True
                c.pop("no_size_match", None)
            if old is not None:
                notes.append("colour matrix read from a camera-written DNG of this body" if v.status == "dng"
                             else NOTE_TEXT.get(g, f"{g} confirmed from sample files"))
            log.append(f"{c['make']} / {c['model']} [{c.get('mode', '')}] {v.note or g}")
        if notes:
            ver = c.setdefault("verification", {})
            lst = ver.setdefault("notes", [])
            lst.extend(n for n in dict.fromkeys(notes) if n not in lst)
    for k, m in enumerate(db["matrices"]):
        v = tab["matrices"][k].get("color_matrix")
        if v is not None and v.cls == "b" and v.new is not None:
            log.append(f"matrix {m['canonical_make']} {m['model_prefix']}: color_matrix {m['color_matrix']} -> {v.new}"[:160])
            m["color_matrix"] = v.new
    return db, log


MATRIX_GROUPS = ["color_matrix", "black", "white"]
# A clean saturation plateau this much above the stored white level replaces it (a plateau at the
# container maximum never does).
WHITE_GAP = 1.0


LEDGER = os.environ.get("FLASHRAW_LEDGER", os.path.join(ROOT, "reference/camera-db-ledger.json"))


def ekey(c):
    return f"{c['make']} / {c['model']} [{c.get('mode', '')}]"


def historical(ledger):
    """Statuses recorded earlier by the ledger file (none: every status comes from this run).
    Returns {"entries": {key: {group: status}}, "matrices": [...], "model_aliases": [...], "size_rules": [...]}."""
    h = {"entries": {}, "matrices": {}, "model_aliases": {}, "size_rules": {}}
    if ledger:
        for e in ledger.get("entries", []):
            h["entries"][e["key"]] = e["fields"]
        for e in ledger.get("matrices", []):
            h["matrices"][e["key"]] = e["fields"]
        for e in ledger.get("model_aliases", []):
            h["model_aliases"][e["key"]] = e["status"]
        for e in ledger.get("size_rules", []):
            h["size_rules"][e["key"]] = e["status"]
        return h
    return h


def build_ledger(E, res, tab, hist):
    """Per entry and field group: where the stored value comes from. A value counts as taken from the
    files when this run reproduces it (or replaced it with the files' value) or an earlier run did
    (`hist`); everything else is kept as stored."""
    entries, tot = [], collections.defaultdict(collections.Counter)
    fs = final_status(E, res, hist)
    for i, c in enumerate(E.cams):
        cur = fs[i]
        kept = sorted(g for g, v in cur.items() if v == "kept")
        entries.append({"key": ekey(c), "make": c["canonical_make"] or c["make"], "samples": len(E.ev(i)),
                        "fields": cur, "all_from_files": not kept, "kept_as_stored": kept})
        for g, v in cur.items():
            tot[g]["kept" if v == "kept" else "files"] += 1
    matrices = []
    for k, m in enumerate(E.db["matrices"]):
        key = f"{m['canonical_make']} / {m['model_prefix']}"
        old = hist["matrices"].get(key, {})
        cur = {}
        for g in MATRIX_GROUPS:
            if g != "color_matrix" and not m.get(g):
                continue
            v = tab["matrices"][k].get(g)
            cur[g] = v.status if derived(v) else (old[g] if g != "color_matrix" and old.get(g, "kept") != "kept" else "kept")
        matrices.append({"key": key, "fields": cur})
    aliases = [{"key": f"{g['canonical_make']} / {g['model']}",
                "status": "file" if tab["model_aliases"][k].cls == "a" else hist["model_aliases"].get(f"{g['canonical_make']} / {g['model']}", "kept")}
               for k, g in enumerate(E.db["model_aliases"])]
    sizes = [{"key": f"{r['canonical_make']} {r['raw_width']}x{r['raw_height']}",
              "status": "file" if tab["size_rules"][k].cls == "a" else hist["size_rules"].get(f"{r['canonical_make']} {r['raw_width']}x{r['raw_height']}", "kept")}
             for k, r in enumerate(E.db["size_rules"])]
    n_all = sum(1 for e in entries if e["all_from_files"])
    mcount = collections.Counter(v for m in matrices for g, v in m["fields"].items() if g == "color_matrix")
    summary = {
        "entries": len(entries), "entries_all_values_from_files": n_all, "entries_with_kept_values": len(entries) - n_all,
        "entries_without_sample_file": sum(1 for e in entries if not e["samples"]),
        "by_field_group": {g: dict(tot[g]) for g in GROUPS},
        "entry_color_matrices": dict(collections.Counter("kept" if e["fields"]["color_matrix"] == "kept" else "dng_or_file"
                                                         for e in entries if "color_matrix" in e["fields"])),
        "fallback_matrix_color": dict(mcount),
        "alias_groups": dict(collections.Counter("kept" if a["status"] == "kept" else "files" for a in aliases)),
        "size_rules": dict(collections.Counter("kept" if a["status"] == "kept" else "files" for a in sizes)),
    }
    return {"note": "Per entry and field group: file / dng / corpus = the value is stated or shown by sample files "
                    "(or a camera-written DNG for colour matrices); kept = kept as stored, not derivable from the files.",
            "summary": summary, "entries": entries, "matrices": matrices, "model_aliases": aliases, "size_rules": sizes}


def cmd_ledger():
    out = arg("--out", LEDGER)
    E = load_evidence()
    res = analyse(E)
    tab = analyse_tables(E)
    prior = json.load(open(out)) if os.path.exists(out) else None
    led = build_ledger(E, res, tab, historical(prior))
    os.makedirs(os.path.dirname(out), exist_ok=True)
    json.dump(led, open(out, "w"), indent=1)
    print(json.dumps(led["summary"], indent=1))


def cmd_apply():
    write = flag("--write")
    E = load_evidence()
    res = analyse(E)
    tab = analyse_tables(E)
    db, log = apply_all(E, res, tab)
    print(f"{len(log)} corrections")
    for l in log:
        print("  " + l)
    if write:
        json.dump(db, open(DB_PATH, "w"), indent=2, ensure_ascii=False)
        subprocess.run([BIN, "camera-db", "generate", DB_PATH], check=True)


def load_evidence():
    global DB_PATH
    DB_PATH = arg("--db", DB_PATH)
    probes = arg("--probes", PROBES)
    if not os.path.exists(probes):
        sys.exit("no probes: run `tools/camera_db/rederive.py probe` first")
    return Evidence(json.load(open(DB_PATH)), json.load(open(probes)))


def cmd_audit():
    md = arg("--md")
    E = load_evidence()
    res = analyse(E)
    tab = analyse_tables(E)
    prior = json.load(open(LEDGER)) if os.path.exists(LEDGER) else None
    lines = report(E, res, tab, historical(prior))
    ch = changes(E, res, tab)
    lines += ["", f"## Disagreements between the stored value and the files ({len(ch)})", "",
              "`APPLY`: the files' value replaces the stored one (recorded in the entry's `verification.notes`).",
              "`KEEP`: the value stays (the stored value is live in a decode, or the files differ among themselves).", "",
              "```"] + ch + ["```"]
    print("\n".join(lines))
    if md:
        open(md, "w").write("\n".join(lines) + "\n")


# --------------------------------------------------------------------------- refine (second pass)

def in_corpus(path):
    corpus = os.environ.get("FLASHRAW_CORPUS", os.path.join(ROOT, "corpus"))
    return os.path.abspath(path).startswith(os.path.abspath(corpus) + os.sep)


def note_once(c, text):
    notes = c.setdefault("verification", {}).setdefault("notes", [])
    if text not in notes:
        notes.append(text)


def refine_crop_origins(db, probes, crops_file, log, models=None):
    """Origins of `image_crops` rules against what the sample files show.

    Evidence, corpus files only:
      * registration: where the camera's embedded full-size JPEG lies inside the active area
        (`flashraw camera-db crops --register`), correlation >= 0.9 (>= 0.75 for the files that
        only join a pool), not pinned to the search range;
      * camera-written DNG: the DefaultCrop origin the camera itself states for the same raw
        frame (same maker, same raw size), expressed relative to the rule's active area.
    A rule is moved when the evidence disagrees with it by more than 2 px and either
      (a) at least two files of one sensor window (maker, raw size, active-area size, image size)
          at >= 0.9 agree within 2 px, the pool median deciding (entries whose own files lie
          within 2 px of it follow), or
      (b) a camera-written DNG states the origin and the registration (when the JPEG fits)
          lies within 3 px of it.
    The offset is kept even (CFA phase of the active area)."""
    sys.path.insert(0, os.path.join(ROOT, "tools"))
    import camera_db_crops as C

    idx = C.Index(db)
    rows = [json.loads(l) for l in open(crops_file, encoding="utf-8")]
    rows = [r for r in rows if "error" not in r and r.get("format") != "Dng"]
    # camera-written DNG crops: (canonical make, raw size) -> [absolute crop origin]
    dng = collections.defaultdict(list)
    for r in probes:
        x, y = r["db"], r["nodb"]
        if not in_corpus(r["file"]) or not ok(x) or not ok(y) or x["format"] != "DNG":
            continue
        if camera_written_dng(x["file"], y):
            dng[(norm(x["canonical_make"]), tuple(y["raw"]))].append((tuple(y["crop"][:2]), tuple(y["crop"][2:]), tuple(y["active_area"][:2])))
    pools = collections.defaultdict(list)    # (make, raw, area, size) -> [(entry, x, y, score)]
    singles = []
    for r in rows:
        g = r.get("registration") or {}
        aa, cr = r["active_area"], r["crop"]
        if r.get("crop_source") not in ("image-size", "preview", "camera-db") or "x" not in g:
            continue
        if g.get("rotation") != 0 or not r.get("jpeg") or sorted(r["jpeg"]) != sorted(cr[2:]):
            continue
        lim = (aa[2] - cr[2], aa[3] - cr[3])
        pinned = any(l > 0 and (v < 0.5 or v > l - 0.5) for v, l in ((g["x"], lim[0]), (g["y"], lim[1])))
        e = idx.find(r["make"], r["model"], r["mode"])
        if e is None:
            continue
        key = (norm(r["canonical_make"]), tuple(r["raw"]), tuple(aa[2:]), tuple(cr[2:]))
        item = (e, g["x"], g["y"], g["score"], r)
        singles.append((key, item))      # a pinned match (at the edge of the search range) only corroborates
        if not pinned:
            pools[key].append(item)
    changed = {}

    def apply(e, area, size, off, why):
        if models and not any(m.upper() == norm(e["model"]) or norm(e["model"]).endswith(" " + m.upper()) for m in models):
            return
        for rule in e.get("image_crops", []):
            if tuple(rule["area"]) == tuple(area) and tuple(rule["crop"][2:]) == tuple(size):
                old = tuple(rule["crop"][:2])
                if max(abs(old[0] - off[0]), abs(old[1] - off[1])) <= 2:
                    return
                rule["crop"][0], rule["crop"][1] = off
                note_once(e, f"image crop origin ({off[0]}, {off[1]}) "
                             f"for the {area[0]}x{area[1]} active area ({why})")
                changed[(e["make"], e["model"], e.get("mode", ""))] = (old, off)
                log.append(f"{e['make']} / {e['model']}: image crop {area[0]}x{area[1]} origin {old} -> {off} ({why})")

    done = set()
    for key, items in pools.items():
        good = [t for t in items if t[3] >= 0.9]
        if len(good) < 2:
            continue
        xs, ys = [t[1] for t in good], [t[2] for t in good]
        if max(xs) - min(xs) > 2 or max(ys) - min(ys) > 2:
            continue
        pool = [t for t in items if t[3] >= 0.75]
        mx, my = statistics.median(t[1] for t in pool), statistics.median(t[2] for t in pool)
        off = (C.even(mx), C.even(my))
        for e, x, y, score, r in items:
            if abs(x - mx) > 2 or abs(y - my) > 2:
                continue
            why = (f"the camera's embedded JPEG registers at ({x:g}, {y:g}), correlation {score:g}; "
                   f"{len(pool)} sample files of this sensor window agree at ({mx:g}, {my:g})")
            apply(e, key[2], key[3], off, why)
            done.add((e["make"], e["model"], e.get("mode", "")))
    # camera-written DNG corroboration
    for key, (e, x, y, score, r) in singles:
        if True:
            if (e["make"], e["model"], e.get("mode", "")) in done or score < 0.9:
                continue
            aa, cr = r["active_area"], r["crop"]
            cands = [(o, sz, ao) for o, sz, ao in dng.get((key[0], key[1]), []) if sz == tuple(cr[2:])]
            if not cands:
                continue
            ox, oy = cands[0][0]
            if any(c[0] != cands[0][0] for c in cands):
                continue
            off = (ox - aa[0], oy - aa[1])
            if off[0] % 2 or off[1] % 2 or off[0] < 0 or off[1] < 0:
                continue
            if abs(x - off[0]) > 3 or abs(y - off[1]) > 3:
                continue
            why = (f"a camera-written DNG of this raw frame states the crop origin ({ox}, {oy}) and the camera's embedded JPEG "
                   f"registers at ({x:g}, {y:g}), correlation {score:g}")
            apply(e, key[2], key[3], off, why)
    return changed


def refine_whites(db, probes, log, models=None):
    """White levels well below a clean saturation plateau of the corpus samples.

    Raised only for entries with a single, ISO-independent sensor row and plain (non-sRAW,
    non-DNG) mosaic samples that all show a plateau (>= 64 samples at the maximum, >= 8x the
    next value, within 0.5 % of each other), where the plateau lies below the container
    maximum (a clip at the container maximum may be the converter's, not the sensor's) and the
    file does not state a white level itself (the empty-database decode equals the container
    maximum), and the stored value is more than 6 % below it."""
    E = Evidence(db, [r for r in probes if in_corpus(r["file"])])
    changed = {}
    for i, c in enumerate(E.cams):
        rows = c.get("sensor", [])
        if len(rows) != 1 or not rows[0].get("white") or rows[0].get("iso_min") or rows[0].get("iso_max") or rows[0].get("iso_list"):
            continue
        w = rows[0]["white"]
        if models and not any(m.upper() == norm(c["model"]) or norm(c["model"]).endswith(" " + m.upper()) for m in models):
            continue
        pl, stated = [], False
        for x, y in E.ok_ev(i):
            if x["format"] == "DNG" or x["mode"] == "dng" or "sraw" in (x.get("compression") or "").lower():
                continue
            p = plateau(x)
            if p is None or plateau_n(x, p) < 64:
                pl = None
                break
            pl.append((p, plateau_n(x, p), x["bits"]))
            full = (1 << max(1, y["bits"])) - 1
            stated |= bool(y["white"]) and max(y["white"]) != full
        if not pl or stated:
            continue
        top = max(p for p, _, _ in pl)
        if min(p for p, _, _ in pl) < top * 0.995 or top >= (1 << pl[0][2]) - 1 or top <= w * WHITE_GAP:
            continue
        rows[0]["white"] = top
        n = sum(k for _, k, _ in pl)
        note_once(c, f"white level {top} from the saturation plateau of the corpus sample ({n} samples at {top})")
        ver = c.get("verification", {})
        if ver.get("white_below_data") and top >= (ver.get("observed_data_max") or 0) / 1.06:
            ver.pop("white_below_data")
        changed[(c["make"], c["model"], c.get("mode", ""))] = (w, top)
        log.append(f"{c['make']} / {c['model']}: white {w} -> {top} (plateau, {n} samples)")
        # the fallback row of the same prefix carries the same value
        for m in db["matrices"]:
            if norm(m["canonical_make"]) == norm(c["canonical_make"]) and prefix_match(c["canonical_model"], m["model_prefix"]) \
                    and m.get("white") == w and m["canonical_make"] != "Canon":
                m["white"] = top
                log.append(f"matrix {m['canonical_make']} {m['model_prefix']}: white {w} -> {top}")
    return changed


def cmd_refine():
    write = flag("--write")
    crops = arg("--crops")
    models = [m.strip() for m in (arg("--models") or "").split(",") if m.strip()]
    global DB_PATH
    DB_PATH = arg("--db", DB_PATH)
    probes = json.load(open(arg("--probes", PROBES)))
    db = json.load(open(DB_PATH))
    log = []
    ch_crops = refine_crop_origins(db, probes, crops, log, models) if crops else {}
    ch_white = refine_whites(db, probes, log, models)
    print(f"{len(ch_crops)} image-crop origins, {len(ch_white)} white levels")
    for l in log:
        print("  " + l)
    if write:
        json.dump(db, open(DB_PATH, "w"), indent=2, ensure_ascii=False)
        subprocess.run([BIN, "camera-db", "generate", DB_PATH], check=True)


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    del sys.argv[1:2]
    if cmd == "probe":
        cmd_probe()
    elif cmd == "audit":
        cmd_audit()
    elif cmd == "apply":
        cmd_apply()
    elif cmd == "refine":
        cmd_refine()
    elif cmd == "ledger":
        cmd_ledger()
    else:
        print(__doc__)
        sys.exit(2)


if __name__ == "__main__":
    main()
