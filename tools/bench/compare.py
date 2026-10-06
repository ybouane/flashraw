#!/usr/bin/env python3
"""Compare FlashRaw with LibRaw and RawSpeed on the same files, on the same machine.

Nothing from the other libraries is vendored: point this script at their source trees
(and, for RawSpeed, at a CMake build). It builds two tiny benchmark programs against
them, runs every library in a fresh process per (file, library, mode, threads) and
writes JSON lines plus a Markdown report.

    # 1. Get the other libraries (any recent checkout works; versions are recorded).
    git clone https://github.com/LibRaw/LibRaw ../bench-libs/LibRaw
    git clone https://github.com/darktable-org/rawspeed ../bench-libs/rawspeed
    cmake -S ../bench-libs/rawspeed -B ../bench-libs/rawspeed-build \
        -DCMAKE_BUILD_TYPE=Release -DWITH_OPENMP=ON -DWITH_JPEG=OFF \
        -DBUILD_TESTING=OFF -DBUILD_BENCHMARKING=OFF -DBUILD_FUZZERS=OFF \
        -DBUILD_DOCS=OFF -DBUILD_TOOLS=OFF
    cmake --build ../bench-libs/rawspeed-build -j
    # macOS (Apple clang has no OpenMP runtime; use a libomp prefix such as Homebrew's):
    #   add -DOpenMP_CXX_FLAGS="-Xpreprocessor -fopenmp -I$OMP/include" \
    #       -DOpenMP_CXX_LIB_NAMES=omp -DOpenMP_omp_LIBRARY=$OMP/lib/libomp.dylib
    #   (a CMake binary tarball from cmake.org works without installing anything)

    # 2. Build the benchmark programs (LibRaw single-threaded and OpenMP, RawSpeed) into
    #    target/bench-compare. LibRaw is built from a copy of its tree with its own
    #    Makefile.dist. Add --zlib-prefix DIR if zlib's headers are not installed.
    tools/bench/compare.py build --libraw-src ../bench-libs/LibRaw \
        --rawspeed-src ../bench-libs/rawspeed --rawspeed-build ../bench-libs/rawspeed-build

    # 3. FlashRaw CLI (JPEG XL DNG needs the jpegxl feature; add gpu for a GPU column).
    cargo build --release -p flashraw-cli --features jpegxl

    # 4. Run (two or more passes on a shared machine; --max-busy waits for quiet moments
    #    on Linux) and report (keeps the lowest median of the passes per measurement).
    tools/bench/compare.py run benches/popular-cameras.tsv benches/benchmark-set.tsv \
        --out pass1.jsonl --max-busy 0.15
    tools/bench/compare.py report pass1.jsonl pass2.jsonl \
        --popular benches/popular-cameras.tsv --markdown benches/results/<host>-compare.md

What is measured (every value: median of --iterations timed runs after one warm-up, in
one process per file; input read into memory before timing):

  decode  FlashRaw  open + full sensor decode to 16-bit samples (`--output raw-u16
                    --backend simd`: the CPU backend)
          LibRaw    open_buffer() + unpack()
          RawSpeed  RawParser::getDecoder() + checkSupport() + decodeRaw() + decodeMetaData()
  rgb     FlashRaw  open + decode + bilinear demosaic + as-shot WB + colour matrix +
                    sRGB 8-bit + orientation (`--output rgba8 --backend simd`)
          LibRaw    open_buffer() + unpack() + dcraw_process() (user_qual=0 bilinear,
                    use_camera_wb=1, output_bps=8, default sRGB output) + dcraw_make_mem_image()
          RawSpeed  (no RGB pipeline: not measured)
  rgb-gpu FlashRaw only, `--backend gpu --output rgba8` (frame downloaded to host memory)

Threads: "all" = every core (FlashRaw default; RawSpeed and the LibRaw OpenMP build with
OpenMP's default team); "1" = one thread (FlashRaw with RAYON_NUM_THREADS=1, RawSpeed with
OMP_NUM_THREADS=1, LibRaw's default single-threaded build). Peak RSS is the process's
ru_maxrss, which includes the input file held in memory.
"""
import argparse
import glob
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
DEFAULT_BIN = os.path.join(ROOT, "target", "bench-compare")

LIBRAW_BENCH_CPP = r"""
#include <libraw/libraw.h>
#include <sys/resource.h>
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

static double now_ms() {
  return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now().time_since_epoch()).count();
}
static std::string esc(const char *s) {
  std::string o;
  for (; *s; s++) { if (*s == '"' || *s == '\\') o += '\\'; o += (*s >= 32 ? *s : ' '); }
  return o;
}

int main(int argc, char **argv) {
  int iters = 7;
  const char *mode = "decode", *file = nullptr;
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "--iterations") && i + 1 < argc) iters = atoi(argv[++i]);
    else if (!strcmp(argv[i], "--mode") && i + 1 < argc) mode = argv[++i];
    else if (!strcmp(argv[i], "--version")) { printf("%s\n", LibRaw::version()); return 0; }
    else file = argv[i];
  }
  if (!file) { fprintf(stderr, "usage: libraw_bench [--iterations N] [--mode decode|rgb|rgb-half] FILE\n"); return 2; }
  FILE *f = fopen(file, "rb");
  if (!f) { perror(file); return 2; }
  std::vector<char> buf;
  fseek(f, 0, SEEK_END);
  buf.resize(ftell(f));
  fseek(f, 0, SEEK_SET);
  if (fread(buf.data(), 1, buf.size(), f) != buf.size()) return 2;
  fclose(f);
  bool rgb = !strncmp(mode, "rgb", 3);
  std::vector<double> total;
  int err = 0;
  int width = 0, height = 0;
  for (int it = 0; it < iters + 1 && !err; it++) {  // iteration 0 is a warm-up
    LibRaw *r = new LibRaw;
    double t0 = now_ms();
    int rc = r->open_buffer(buf.data(), buf.size());
    if (rc == LIBRAW_SUCCESS) rc = r->unpack();
    if (rc == LIBRAW_SUCCESS && rgb) {
      r->imgdata.params.user_qual = 0;      // bilinear
      r->imgdata.params.use_camera_wb = 1;  // as-shot white balance
      r->imgdata.params.output_bps = 8;
      if (!strcmp(mode, "rgb-half")) r->imgdata.params.half_size = 1;
      rc = r->dcraw_process();
      if (rc == LIBRAW_SUCCESS) {
        int e = 0;
        libraw_processed_image_t *img = r->dcraw_make_mem_image(&e);
        if (img) LibRaw::dcraw_clear_mem(img); else rc = e ? e : LIBRAW_UNSPECIFIED_ERROR;
      }
    }
    double t1 = now_ms();
    width = r->imgdata.sizes.raw_width;
    height = r->imgdata.sizes.raw_height;
    if (rc != LIBRAW_SUCCESS) err = rc;
    else if (it > 0) total.push_back(t1 - t0);
    delete r;
  }
  std::sort(total.begin(), total.end());
  struct rusage ru;
  getrusage(RUSAGE_SELF, &ru);
  long rss_kb = ru.ru_maxrss;
#ifdef __APPLE__
  rss_kb /= 1024;  // bytes on macOS
#endif
  printf("{\"error\":%d,\"error_text\":\"%s\",\"ms_median\":%.4f,\"ms_min\":%.4f,\"runs\":%zu,"
         "\"raw_width\":%d,\"raw_height\":%d,\"maxrss_kb\":%ld}\n",
         err, err ? esc(libraw_strerror(err)).c_str() : "", total.empty() ? -1.0 : total[total.size() / 2],
         total.empty() ? -1.0 : total[0], total.size(), width, height, rss_kb);
  return 0;
}
"""

RAWSPEED_BENCH_CPP = r"""
#include "RawSpeed-API.h"
#include <sys/resource.h>
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

static double now_ms() {
  return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now().time_since_epoch()).count();
}
static std::string esc(const char *s) {
  std::string o;
  for (; *s; s++) { if (*s == '"' || *s == '\\') o += '\\'; o += (*s >= 32 ? *s : ' '); }
  return o;
}

int main(int argc, char **argv) {
  int iters = 7;
  const char *cameras = nullptr, *file = nullptr;
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "--iterations") && i + 1 < argc) iters = atoi(argv[++i]);
    else if (!strcmp(argv[i], "--cameras") && i + 1 < argc) cameras = argv[++i];
    else file = argv[i];
  }
  if (!file || !cameras) { fprintf(stderr, "usage: rawspeed_bench --cameras cameras.xml [--iterations N] FILE\n"); return 2; }
  FILE *f = fopen(file, "rb");
  if (!f) { perror(file); return 2; }
  fseek(f, 0, SEEK_END);
  size_t size = ftell(f);
  fseek(f, 0, SEEK_SET);
  // RawSpeed requires padded, aligned input (as its own file reader provides).
  size_t padded = (size + 64 + 15) & ~size_t(15);
  uint8_t *data = static_cast<uint8_t *>(aligned_alloc(16, padded));
  memset(data + size, 0, padded - size);
  if (fread(data, 1, size, f) != size) return 2;
  fclose(f);
  const rawspeed::CameraMetaData meta(cameras);
  std::vector<double> total;
  std::string err;
  int width = 0, height = 0;
  for (int it = 0; it < iters + 1 && err.empty(); it++) {  // iteration 0 is a warm-up
    try {
      double t0 = now_ms();
      rawspeed::Buffer buf(data, size);
      rawspeed::RawParser parser(buf);
      auto decoder = parser.getDecoder(&meta);
      decoder->failOnUnknown = false;
      decoder->checkSupport(&meta);
      decoder->decodeRaw();
      decoder->decodeMetaData(&meta);
      double t1 = now_ms();
      auto dim = decoder->mRaw->dim;
      width = dim.x;
      height = dim.y;
      if (it > 0) total.push_back(t1 - t0);
    } catch (const std::exception &e) {
      err = e.what();
      if (err.empty()) err = "exception";
    }
  }
  std::sort(total.begin(), total.end());
  struct rusage ru;
  getrusage(RUSAGE_SELF, &ru);
  long rss_kb = ru.ru_maxrss;
#ifdef __APPLE__
  rss_kb /= 1024;
#endif
  printf("{\"error\":%d,\"error_text\":\"%s\",\"ms_median\":%.4f,\"ms_min\":%.4f,\"runs\":%zu,"
         "\"width\":%d,\"height\":%d,\"maxrss_kb\":%ld}\n",
         err.empty() ? 0 : 1, esc(err.c_str()).c_str(), total.empty() ? -1.0 : total[total.size() / 2],
         total.empty() ? -1.0 : total[0], total.size(), width, height, rss_kb);
  return 0;
}
"""


def sh(cmd, **kw):
    print("+ " + " ".join(cmd), file=sys.stderr)
    subprocess.run(cmd, check=True, **kw)


def openmp_flags(a):
    """(compile flags, link flags) for OpenMP with the configured C++ compiler."""
    if a.openmp_cflags is not None:
        return a.openmp_cflags.split(), (a.openmp_libs or "").split()
    if sys.platform == "darwin":
        # Apple clang has no bundled OpenMP runtime: use a libomp prefix (e.g. Homebrew's).
        prefix = a.libomp_prefix or next((p for p in ("/opt/homebrew/opt/libomp", "/usr/local/opt/libomp")
                                          if os.path.isdir(p)), None)
        if not prefix:
            sys.exit("OpenMP: pass --libomp-prefix (a directory with include/omp.h and lib/libomp.*)")
        return (["-Xpreprocessor", "-fopenmp", f"-I{prefix}/include"],
                [f"-L{prefix}/lib", "-lomp", f"-Wl,-rpath,{prefix}/lib"])
    return ["-fopenmp"], ["-fopenmp"]


def cmd_build(a):
    a.bin_dir = os.path.abspath(a.bin_dir)
    os.makedirs(a.bin_dir, exist_ok=True)
    cxx = a.cxx
    native = [] if a.no_native or sys.platform == "darwin" else ["-march=native"]
    zinc = [f"-I{a.zlib_prefix}/include"] if a.zlib_prefix else []
    zlib = [f"{a.zlib_prefix}/lib/libz.a"] if a.zlib_prefix else ["-lz"]
    omp_c, omp_l = openmp_flags(a)
    jobs = str(os.cpu_count() or 4)
    info = {}

    if a.libraw_src:
        src = os.path.abspath(a.libraw_src)
        for variant, extra_c, extra_l in (("st", [], []), ("omp", omp_c, omp_l)):
            tree = os.path.join(a.bin_dir, f"libraw-{variant}")
            if not os.path.exists(os.path.join(tree, "lib", "libraw.a")):
                shutil.rmtree(tree, ignore_errors=True)
                shutil.copytree(src, tree, ignore=shutil.ignore_patterns(".git", "object", "lib", "bin"))
                os.makedirs(os.path.join(tree, "object"), exist_ok=True)
                os.makedirs(os.path.join(tree, "lib"), exist_ok=True)
                # LibRaw's own Makefile.dist, with its defaults (-O3, zlib for deflate DNG)
                # plus X3F and the 6by9 Raspberry Pi format, as in its full builds.
                cflags = ["-O3", "-I.", "-w", "-DUSE_ZLIB", "-DUSE_X3FTOOLS", "-DUSE_6BY9RPI"] + native + zinc + extra_c
                sh(["make", "-f", "Makefile.dist", "-j", jobs, "lib/libraw.a",
                    "CXX=" + cxx, "CC=" + a.cc, "CFLAGS=" + " ".join(cflags)], cwd=tree)
            cpp = os.path.join(a.bin_dir, "libraw_bench.cpp")
            with open(cpp, "w") as o:
                o.write(LIBRAW_BENCH_CPP)
            out = os.path.join(a.bin_dir, f"libraw_bench_{variant}")
            sh([cxx, "-O2", "-std=c++17", f"-I{tree}", cpp, os.path.join(tree, "lib", "libraw.a")] + zinc + zlib
               + extra_c + extra_l + ["-lm", "-o", out])
            info[f"libraw_{variant}"] = out
        info["libraw_version"] = subprocess.run([os.path.join(a.bin_dir, "libraw_bench_st"), "--version"],
                                                capture_output=True, text=True).stdout.strip()
        info["libraw_commit"] = git_rev(src)

    if a.rawspeed_src and a.rawspeed_build:
        src, bld = os.path.abspath(a.rawspeed_src), os.path.abspath(a.rawspeed_build)
        libs = [os.path.join(bld, "librawspeed.a")]
        libs += glob.glob(os.path.join(bld, "src", "librawspeed", "common", "*.a"))
        libs += [libs[0]]
        bundled = glob.glob(os.path.join(bld, "src", "external", "pugixml", "build", "libpugixml.a"))
        libs += bundled or cmake_cache(bld, "Pugixml_LIBRARY")
        cpp = os.path.join(a.bin_dir, "rawspeed_bench.cpp")
        with open(cpp, "w") as o:
            o.write(RAWSPEED_BENCH_CPP)
        out = os.path.join(a.bin_dir, "rawspeed_bench")
        sh([cxx, "-O3", "-std=c++20", "-DNDEBUG", f"-I{bld}/src", f"-I{src}/src/librawspeed",
            "-isystem", f"{src}/src/external", cpp] + native + omp_c + libs + zinc + zlib + omp_l
           + ["-lpthread", "-o", out])
        info["rawspeed"] = out
        info["rawspeed_cameras"] = os.path.join(src, "data", "cameras.xml")
        info["rawspeed_commit"] = git_rev(src)

    info["cxx"] = subprocess.run([cxx, "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    info["native"] = bool(native)
    path = os.path.join(a.bin_dir, "build.json")
    old = json.load(open(path)) if os.path.exists(path) else {}
    old.update(info)
    json.dump(old, open(path, "w"), indent=1)
    print(json.dumps(old, indent=1))


def cmake_cache(build_dir, key):
    """[value] of a CMake cache entry (e.g. a system library RawSpeed was configured with)."""
    for line in open(os.path.join(build_dir, "CMakeCache.txt")):
        if line.startswith(key + ":"):
            return [line.split("=", 1)[1].strip()]
    return []


def git_rev(path):
    p = subprocess.run(["git", "-C", path, "rev-parse", "--short=12", "HEAD"], capture_output=True, text=True)
    return p.stdout.strip() or None


def read_list(inputs, corpus):
    files = []
    for inp in inputs:
        if inp.endswith((".tsv", ".txt")):
            for line in open(inp):
                line = line.rstrip("\n")
                if line.strip() and not line.startswith("#"):
                    files.append(line.split("\t")[0])
        else:
            files.append(inp)
    seen, out = set(), []
    for f in files:
        if f not in seen:
            seen.add(f)
            out.append(f)
    return out


def last_json(text):
    return next((json.loads(l) for l in reversed(text.splitlines()) if l.startswith("{")), None)


def cpu_busy(interval=0.5):
    """Fraction of all CPUs busy over `interval` seconds (Linux /proc/stat), or None."""
    def snap():
        with open("/proc/stat") as f:
            v = list(map(int, f.readline().split()[1:]))
        return sum(v), v[3] + v[4]  # total, idle + iowait
    try:
        t0, i0 = snap()
        time.sleep(interval)
        t1, i1 = snap()
    except OSError:
        return None
    return 1 - (i1 - i0) / max(1, t1 - t0)


def run_one(cmd, env=None, timeout=1800):
    t = time.time()
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=timeout)
    except subprocess.TimeoutExpired:
        return None, "timeout", time.time() - t
    return last_json(p.stdout), (p.stderr.strip().splitlines() or [f"exit {p.returncode}"])[-1], time.time() - t


def cmd_run(a):
    build = json.load(open(os.path.join(a.bin_dir, "build.json"))) if os.path.exists(
        os.path.join(a.bin_dir, "build.json")) else {}
    files = read_list(a.inputs, a.corpus)
    done = set()
    if a.resume and os.path.exists(a.out):
        for l in open(a.out):
            r = json.loads(l)
            if a.redo_busy is None or (r.get("busy") is not None and r["busy"] <= a.redo_busy):
                done.add((r["file"], r["lib"], r["mode"], r["threads"]))
    elif os.path.exists(a.out):
        os.remove(a.out)
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    env_all = dict(os.environ)
    env_all.pop("OMP_NUM_THREADS", None)
    env_one = dict(env_all, OMP_NUM_THREADS="1", RAYON_NUM_THREADS="1")
    libs = [l for l in a.libs if l == "flashraw" or build.get({"libraw": "libraw_st", "rawspeed": "rawspeed"}[l])]
    jobs = []
    for f in files:
        for threads in a.threads:
            for mode in a.modes:
                for lib in libs:
                    if lib == "rawspeed" and mode != "decode":
                        continue
                    if mode == "rgb-gpu" and (lib != "flashraw" or threads != "all"):
                        continue
                    jobs.append((f, lib, mode, threads))
    # Run the libraries for one file back to back (and alternate their order) so that slow
    # drifts in machine load affect them alike.
    for i, (f, lib, mode, threads) in enumerate(jobs):
        if (f, lib, mode, threads) in done:
            continue
        path = os.path.join(a.corpus, f)
        if not os.path.exists(path):
            print(f"missing: {path}", file=sys.stderr)
            continue
        env = env_one if threads == "1" else env_all
        it = str(a.iterations)
        # Other jobs on the machine: wait (up to --max-wait s) until it is quiet.
        t_wait = time.time()
        busy = cpu_busy()
        while a.max_busy and busy is not None and busy > a.max_busy and time.time() - t_wait < a.max_wait:
            time.sleep(1)
            busy = cpu_busy()
        load = os.getloadavg()[0]
        if lib == "flashraw":
            cmd = [a.flashraw, "benchmark", "--json", "--iterations", it]
            cmd += {"decode": ["--output", "raw-u16"], "rgb": ["--output", "rgba8"],
                    "rgb-half": ["--output", "rgba8", "--demosaic", "half"],
                    "rgb-gpu": ["--output", "rgba8", "--backend", "gpu"]}[mode]
            if mode != "rgb-gpu":
                cmd += ["--backend", "simd"]  # CPU only (a GPU build's `auto` may pick the GPU)
            # Single thread: RAYON_NUM_THREADS=1 (set in env_one); like `--threads 1`, the
            # decode then runs on the calling thread, as with the other libraries.
            j, err, _ = run_one(cmd + [path], env)
            rec = {"ok": False, "error": err} if j is None or "total_ms_median" not in j else {
                "ok": True, "ms": j["total_ms_median"], "ms_min": j.get("total_ms_min"),
                "mp": j.get("megapixels"),
                "rss_mb": (j.get("maxrss_kb") or 0) / 1024,
                "camera": j.get("camera"), "format": j.get("format"), "compression": j.get("compression"),
                "backend": (j.get("backend") or {}).get("decode"),
                "postprocess": (j.get("backend") or {}).get("postprocess")}
        elif lib == "libraw":
            exe = build["libraw_st"] if threads == "1" else build.get("libraw_omp", build["libraw_st"])
            j, err, _ = run_one([exe, "--iterations", it, "--mode", "decode" if mode == "decode" else mode, path], env)
            rec = {"ok": False, "error": err} if j is None else (
                {"ok": False, "error": j["error_text"], "code": j["error"]} if j["error"] else
                {"ok": True, "ms": j["ms_median"], "ms_min": j["ms_min"], "rss_mb": j["maxrss_kb"] / 1024})
        else:
            j, err, _ = run_one([build["rawspeed"], "--cameras", build["rawspeed_cameras"], "--iterations", it, path], env)
            rec = {"ok": False, "error": err} if j is None else (
                {"ok": False, "error": j["error_text"]} if j["error"] else
                {"ok": True, "ms": j["ms_median"], "ms_min": j["ms_min"], "rss_mb": j["maxrss_kb"] / 1024})
        rec.update(file=f, lib=lib, mode=mode, threads=threads, load=round(load, 2),
                   busy=None if busy is None else round(busy, 3))
        with open(a.out, "a") as o:
            o.write(json.dumps(rec) + "\n")
        print(f"[{i + 1}/{len(jobs)}] {lib:8} {mode:7} t={threads:3} "
              f"{('%9.2f ms' % rec['ms']) if rec['ok'] else 'FAIL ' + str(rec.get('error'))[:60]}  {f}",
              file=sys.stderr, flush=True)


# ---------------------------------------------------------------- report


def gmean(xs):
    xs = [x for x in xs if x and x > 0]
    return math.exp(sum(map(math.log, xs)) / len(xs)) if xs else None


def fmt_ms(v):
    if v is None:
        return ""
    return "%.1f" % v if v >= 10 else "%.2f" % v


def fmt_x(v):
    return "" if v is None else ("%.1f×" % v if v >= 10 else "%.2f×" % v)


def cmd_report(a):
    # Several passes: for every (file, library, mode, threads) keep the pass with the lowest
    # median, so a burst of background load in one pass does not count against anyone.
    rows, by, meta = [], {}, {}
    for path in a.results:
        for l in open(path):
            r = json.loads(l)
            k = (r["file"], r["lib"], r["mode"], r["threads"])
            old = by.get(k)
            if old is None:
                rows.append(r)
            if old is None or (r.get("ok") and (not old.get("ok") or r["ms"] < old["ms"])):
                by[k] = r
    rows = [by[(r["file"], r["lib"], r["mode"], r["threads"])] for r in rows]
    for r in rows:
        if r["lib"] == "flashraw" and r.get("ok") and r["mode"] == "decode":
            meta.setdefault(r["file"], r)
    popular = []
    if a.popular:
        for line in open(a.popular):
            if line.strip() and not line.startswith("#"):
                p = line.rstrip("\n").split("\t")
                popular.append((p[0], p[1], p[2] if len(p) > 2 else ""))
    files = list(dict.fromkeys(r["file"] for r in rows))

    def ms(f, lib, mode, th):
        r = by.get((f, lib, mode, th))
        return r["ms"] if r and r.get("ok") else None

    def cell(f, lib, mode, th):
        r = by.get((f, lib, mode, th))
        if r is None:
            return "–"
        if not r.get("ok"):
            return "❌"
        if mode == "rgb-gpu" and not str(r.get("postprocess") or "").startswith("GPU"):
            return "CPU¹"  # the GPU pipeline does not handle this file: it ran on the CPU
        # Bold the fastest library for this file, mode and thread count.
        fastest = min((x["ms"] for l in ("flashraw", "libraw", "rawspeed")
                       for x in [by.get((f, l, mode, th))] if x and x.get("ok")), default=None)
        return f"**{fmt_ms(r['ms'])}**" if mode != "rgb-gpu" and r["ms"] == fastest else fmt_ms(r["ms"])

    def label(f, cam, note):
        # The note names the camera's own compression mode; skip it when it only repeats the format.
        if not note or note.upper() == str(meta.get(f, {}).get("format")).upper() or note in ("RW2", "ORF", "PEF"):
            return cam
        return f"{cam} ({note})"

    def codec(f):
        m = meta.get(f, {})
        return f"{m.get('format', '?')} · {m.get('compression', '?')}"

    out = []
    title = a.title or "FlashRaw vs LibRaw vs RawSpeed"
    out.append(f"# {title}\n")
    if a.header:
        out.append(open(a.header).read().rstrip() + "\n")
    out.append("Times in ms (median of the timed runs; lower is better). "
               "❌ = the library failed or does not support the file; – = not measured. "
               "Speedup = other library's time / FlashRaw's time. **Bold** = fastest library for that file.\n")

    def summary(sel, label):
        lines = [f"### {label}\n", "| Comparison | threads | files | geomean speedup | median speedup | FlashRaw faster |",
                 "|---|---|---:|---:|---:|---:|"]
        for mode, lib, name in (("decode", "libraw", "decode vs LibRaw"), ("decode", "rawspeed", "decode vs RawSpeed"),
                                ("rgb", "libraw", "RAW→RGB vs LibRaw")):
            for th in ("all", "1"):
                sp = []
                for f in sel:
                    x, y = ms(f, "flashraw", mode, th), ms(f, lib, mode, th)
                    if x and y:
                        sp.append(y / x)
                if sp:
                    wins = sum(1 for s in sp if s > 1)
                    lines.append(f"| {name} | {th} | {len(sp)} | **{fmt_x(gmean(sp))}** | {fmt_x(statistics.median(sp))} | {wins}/{len(sp)} |")
        return lines + [""]

    pop_files = [p[0] for p in popular if p[0] in meta]
    if pop_files:
        out += summary(pop_files, f"Summary: popular cameras ({len(pop_files)} files)")
    other = [f for f in files if f in meta]
    out += summary(other, f"Summary: all files ({len(other)})")

    has_gpu = any(r["mode"] == "rgb-gpu" for r in rows)
    if popular:
        out.append("## Popular cameras\n")
        hdr = ("| Camera | Format · compression | MP | Decode: FlashRaw | LibRaw | RawSpeed | "
               "RAW→RGB: FlashRaw | LibRaw |" + (" FlashRaw GPU |" if has_gpu else ""))
        out += [hdr, "|---|---|---:|---:|---:|---:|---:|---:|" + ("---:|" if has_gpu else "")]
        for f, cam, note in popular:
            if f not in meta:
                continue
            mp = meta[f].get("mp") or 0
            line = (f"| {label(f, cam, note)} | {codec(f)} | {mp:.1f} | {cell(f, 'flashraw', 'decode', 'all')} | "
                    f"{cell(f, 'libraw', 'decode', 'all')} | {cell(f, 'rawspeed', 'decode', 'all')} | "
                    f"{cell(f, 'flashraw', 'rgb', 'all')} | {cell(f, 'libraw', 'rgb', 'all')} |")
            if has_gpu:
                line += f" {cell(f, 'flashraw', 'rgb-gpu', 'all')} |"
            out.append(line)
        out.append("")
        if has_gpu:
            out.append("FlashRaw GPU = `--backend gpu --output rgba8`: CPU decode or GPU unpack, GPU develop, "
                       "frame downloaded to host memory. ¹ FlashRaw's GPU pipeline does not handle "
                       "this file's processing yet and ran it on the CPU (same result, CPU time).\n")
        out.append("Single thread (same files):\n")
        out += ["| Camera | Decode: FlashRaw | LibRaw | RawSpeed | RAW→RGB: FlashRaw | LibRaw |",
                "|---|---:|---:|---:|---:|---:|"]
        for f, cam, note in popular:
            if f not in meta:
                continue
            out.append(f"| {label(f, cam, note)} | {cell(f, 'flashraw', 'decode', '1')} | "
                       f"{cell(f, 'libraw', 'decode', '1')} | {cell(f, 'rawspeed', 'decode', '1')} | "
                       f"{cell(f, 'flashraw', 'rgb', '1')} | {cell(f, 'libraw', 'rgb', '1')} |")
        out.append("")

    # Per (format, compression).
    groups = {}
    for f in files:
        if f in meta:
            groups.setdefault((meta[f].get("format"), meta[f].get("compression")), []).append(f)
    out.append("## Per format (geometric mean speedup of FlashRaw, all threads)\n")
    out += ["| Format | Compression | files | decode vs LibRaw | decode vs RawSpeed | RAW→RGB vs LibRaw | LibRaw ✅ | RawSpeed ✅ |",
            "|---|---|---:|---:|---:|---:|---:|---:|"]
    for (fmt, comp), fs in sorted(groups.items(), key=lambda kv: (str(kv[0][0]), str(kv[0][1]))):
        def sp(lib, mode):
            v = [ms(f, lib, mode, "all") / ms(f, "flashraw", mode, "all") for f in fs
                 if ms(f, lib, mode, "all") and ms(f, "flashraw", mode, "all")]
            return fmt_x(gmean(v)) if v else "❌"
        lr_ok = sum(1 for f in fs if ms(f, "libraw", "decode", "all"))
        rs_ok = sum(1 for f in fs if ms(f, "rawspeed", "decode", "all"))
        out.append(f"| {fmt} | {comp} | {len(fs)} | {sp('libraw', 'decode')} | {sp('rawspeed', 'decode')} | "
                   f"{sp('libraw', 'rgb')} | {lr_ok}/{len(fs)} | {rs_ok}/{len(fs)} |")
    out.append("")

    out.append("## All files\n")
    out += ["| File | Camera | Codec | MP | threads | Decode FlashRaw | LibRaw | RawSpeed | RGB FlashRaw | LibRaw | "
            "Peak RSS MB (decode) FR / LR / RS |", "|---|---|---|---:|---|---:|---:|---:|---:|---:|---|"]
    for f in files:
        if f not in meta:
            out.append(f"| {os.path.basename(f)} | | | | | ❌ | | | | | |")
            continue
        for th in ("all", "1"):
            if (f, "flashraw", "decode", th) not in by:
                continue
            rss = " / ".join(("%.0f" % by[(f, l, "decode", th)]["rss_mb"]) if by.get((f, l, "decode", th), {}).get("ok")
                             else "–" for l in ("flashraw", "libraw", "rawspeed"))
            out.append(f"| {os.path.basename(f)} | {meta[f].get('camera') or ''} | {codec(f)} | {meta[f].get('mp') or 0:.1f} | {th} | "
                       f"{cell(f, 'flashraw', 'decode', th)} | {cell(f, 'libraw', 'decode', th)} | {cell(f, 'rawspeed', 'decode', th)} | "
                       f"{cell(f, 'flashraw', 'rgb', th)} | {cell(f, 'libraw', 'rgb', th)} | {rss} |")
    out.append("")
    fails = [r for r in rows if not r.get("ok")]
    if fails:
        out.append("## Failures and unsupported files\n")
        out += ["| File | library | mode | threads | error |", "|---|---|---|---|---|"]
        for r in fails:
            out.append(f"| {os.path.basename(r['file'])} | {r['lib']} | {r['mode']} | {r['threads']} | "
                       f"{str(r.get('error'))[:120].replace('|', '/')} |")
        out.append("")
    with open(a.markdown, "w") as o:
        o.write("\n".join(out))
    print(f"wrote {a.markdown}", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build", help="build the LibRaw and RawSpeed benchmark programs")
    b.add_argument("--libraw-src")
    b.add_argument("--rawspeed-src")
    b.add_argument("--rawspeed-build")
    b.add_argument("--bin-dir", default=DEFAULT_BIN)
    b.add_argument("--cxx", default=os.environ.get("CXX", "c++"))
    b.add_argument("--cc", default=os.environ.get("CC", "cc"))
    b.add_argument("--zlib-prefix", help="zlib install prefix (default: the system zlib)")
    b.add_argument("--libomp-prefix", help="macOS: libomp prefix (default: Homebrew's)")
    b.add_argument("--openmp-cflags")
    b.add_argument("--openmp-libs")
    b.add_argument("--no-native", action="store_true", help="do not add -march=native on x86")
    r = sub.add_parser("run", help="run the benchmark")
    r.add_argument("inputs", nargs="+", help="files, or .tsv/.txt lists of corpus-relative paths (first column)")
    r.add_argument("--corpus", default=os.path.join(ROOT, "corpus"))
    r.add_argument("--flashraw", default=os.path.join(ROOT, "target", "release", "flashraw"))
    r.add_argument("--bin-dir", default=DEFAULT_BIN)
    r.add_argument("--iterations", type=int, default=7)
    r.add_argument("--libs", nargs="+", default=["flashraw", "libraw", "rawspeed"])
    r.add_argument("--modes", nargs="+", default=["decode", "rgb"], choices=["decode", "rgb", "rgb-half", "rgb-gpu"])
    r.add_argument("--threads", nargs="+", default=["all", "1"], choices=["all", "1"])
    r.add_argument("--out", required=True)
    r.add_argument("--resume", action="store_true", help="append, skipping measurements already in --out")
    r.add_argument("--max-busy", type=float, default=0,
                   help="Linux: before each measurement wait (up to --max-wait s) until the fraction of busy "
                        "CPU time over 0.5 s (other processes) is below this, e.g. 0.1")
    r.add_argument("--max-wait", type=float, default=60)
    r.add_argument("--redo-busy", type=float,
                   help="with --resume: measure again what was measured while the machine was busier than this "
                        "(the report keeps the lowest median)")
    p = sub.add_parser("report", help="write the Markdown report")
    p.add_argument("results", nargs="+", help="one or more passes (.jsonl); the lowest median per measurement is kept")
    p.add_argument("--markdown", required=True)
    p.add_argument("--popular", help="benches/popular-cameras.tsv (path, camera, note)")
    p.add_argument("--header", help="Markdown file with the run conditions, inserted at the top")
    p.add_argument("--title")
    a = ap.parse_args()
    {"build": cmd_build, "run": cmd_run, "report": cmd_report}[a.cmd](a)


if __name__ == "__main__":
    main()
