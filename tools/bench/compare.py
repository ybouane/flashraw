#!/usr/bin/env python3
"""Compare FlashRaw with other RAW decoders on the same files, on the same machine.

Libraries: LibRaw, RawSpeed, rawler (dnglab), rawpy (Python, LibRaw inside), dcraw, and the
LibRaw + Little CMS colour-managed pipeline. Nothing from them is vendored: point this script
at their source trees (and, for RawSpeed, at a CMake build); it builds tiny benchmark programs
against them (rawler from crates.io, rawpy from PyPI into a venv), runs every library in a
fresh process per (file, library, mode, threads) and writes JSON lines plus a Markdown report.

    # 1. Get the other libraries (any recent version works; versions are recorded).
    git clone https://github.com/LibRaw/LibRaw ../bench-libs/LibRaw
    git clone https://github.com/darktable-org/rawspeed ../bench-libs/rawspeed
    curl -L https://github.com/mm2/Little-CMS/archive/refs/tags/lcms2.19.1.tar.gz | tar xz -C ../bench-libs
    curl -L -o ../bench-libs/dcraw.c https://www.dechifro.org/dcraw/dcraw.c
    cmake -S ../bench-libs/rawspeed -B ../bench-libs/rawspeed-build \
        -DCMAKE_BUILD_TYPE=Release -DWITH_OPENMP=ON -DWITH_JPEG=OFF \
        -DBUILD_TESTING=OFF -DBUILD_BENCHMARKING=OFF -DBUILD_FUZZERS=OFF \
        -DBUILD_DOCS=OFF -DBUILD_TOOLS=OFF
    cmake --build ../bench-libs/rawspeed-build -j
    # macOS (Apple clang has no OpenMP runtime; use a libomp prefix such as Homebrew's):
    #   add -DOpenMP_CXX_FLAGS="-Xpreprocessor -fopenmp -I$OMP/include" \
    #       -DOpenMP_CXX_LIB_NAMES=omp -DOpenMP_omp_LIBRARY=$OMP/lib/libomp.dylib
    #   (a CMake binary tarball from cmake.org works without installing anything)

    # 2. Build the benchmark programs into target/bench-compare (each option is optional).
    #    LibRaw is built from a copy of its tree with its own Makefile.dist. Add
    #    --zlib-prefix DIR if zlib's headers are not installed.
    tools/bench/compare.py build --libraw-src ../bench-libs/LibRaw \
        --rawspeed-src ../bench-libs/rawspeed --rawspeed-build ../bench-libs/rawspeed-build \
        --lcms-src ../bench-libs/Little-CMS-lcms2.19.1 --dcraw-src ../bench-libs/dcraw.c \
        --rawler-version 0.8.0 --rawpy rawpy==0.27.1

    # 3. FlashRaw CLI (JPEG XL DNG needs the jpegxl feature; add gpu for a GPU column).
    cargo build --release -p flashraw-cli --features jpegxl

    # 4. Run (two or more passes on a shared machine; --max-busy waits for quiet moments
    #    on Linux), merge the passes (lowest median per measurement) and report.
    tools/bench/compare.py run benches/popular-cameras.tsv \
        --libs flashraw libraw rawspeed rawler rawpy dcraw lcms --out pass1.jsonl --max-busy 0.15
    tools/bench/compare.py merge pass1.jsonl pass2.jsonl --out benches/results/<host>-compare.jsonl
    tools/bench/compare.py report benches/results/<host>-compare.jsonl \
        --popular benches/popular-cameras.tsv --markdown benches/results/<host>-compare.md
    tools/bench/charts.py                  # docs/charts/*.svg and data.json

What is measured (every value: median of --iterations timed runs after one warm-up, in
one process per file; input read into memory before timing):

  decode  FlashRaw  open + full sensor decode to 16-bit samples (`--output raw-u16
                    --backend simd`: the CPU backend)
          LibRaw    open_buffer() + unpack()
          RawSpeed  RawParser::getDecoder() + checkSupport() + decodeRaw() + decodeMetaData()
          rawler    rawler::decode() of an in-memory RawSource
          rawpy     rawpy.imread(BytesIO) + raw_image (LibRaw unpack)
          dcraw     whole process `dcraw -c -D -4 FILE > /dev/null`
  rgb     FlashRaw  open + decode + bilinear demosaic + as-shot WB + colour matrix +
                    sRGB 8-bit + orientation (`--output rgba8 --backend simd`)
          LibRaw    open_buffer() + unpack() + dcraw_process() (user_qual=0 bilinear,
                    use_camera_wb=1, output_bps=8, default sRGB output) + dcraw_make_mem_image()
          lcms      LibRaw to 16-bit linear ProPhoto (dcraw_process, bilinear, camera WB,
                    gamma 1, no auto-brightness) + Little CMS transform to 8-bit sRGB RGBA
          rawler    decode + RawDevelop::default() (PPG demosaic, f32) + 8-bit conversion
          rawpy     decode + postprocess(LINEAR demosaic, camera WB, 8-bit)
          dcraw     whole process `dcraw -c -q 0 -w FILE > /dev/null`
          RawSpeed  (no RGB pipeline: not measured)
  rgb-gpu FlashRaw only, `--backend gpu --output rgba8` (frame downloaded to host memory)

Threads: "all" = every core (FlashRaw default; RawSpeed, the LibRaw OpenMP build and the lcms2
transform with OpenMP's default team; rawler with rayon); "1" = one thread (RAYON_NUM_THREADS=1,
OMP_NUM_THREADS=1, LibRaw's default single-threaded build). dcraw has no threads and is run
once, with "1". Peak RSS is the process's ru_maxrss, which includes the input file held in
memory (dcraw: the child process's).
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
import tempfile
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

# The typical colour-managed pipeline of an application built on LibRaw: LibRaw decodes and
# develops to a linear wide-gamut image (bilinear demosaic, as-shot WB, camera matrix to
# linear ProPhoto RGB, 16-bit, no gamma, no auto-brightness), then Little CMS converts it to
# 8-bit sRGB (RGBA, like FlashRaw's rgba8). With OpenMP the transform runs over row blocks on
# all cores (a shared lcms2 transform is reentrant); without, it is one cmsDoTransform call.
LIBRAW_LCMS_BENCH_CPP = r"""
#include <libraw/libraw.h>
#include <lcms2.h>
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
static double med(std::vector<double> v) {
  if (v.empty()) return -1;
  std::sort(v.begin(), v.end());
  return v[v.size() / 2];
}

int main(int argc, char **argv) {
  int iters = 7;
  const char *file = nullptr;
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "--iterations") && i + 1 < argc) iters = atoi(argv[++i]);
    else if (!strcmp(argv[i], "--version")) { printf("%s lcms2 %d\n", LibRaw::version(), cmsGetEncodedCMMversion()); return 0; }
    else if (!strcmp(argv[i], "--mode") && i + 1 < argc) i++;
    else file = argv[i];
  }
  if (!file) { fprintf(stderr, "usage: libraw_lcms_bench [--iterations N] FILE\n"); return 2; }
  FILE *f = fopen(file, "rb");
  if (!f) { perror(file); return 2; }
  std::vector<char> buf;
  fseek(f, 0, SEEK_END);
  buf.resize(ftell(f));
  fseek(f, 0, SEEK_SET);
  if (fread(buf.data(), 1, buf.size(), f) != buf.size()) return 2;
  fclose(f);
  // Linear ProPhoto RGB (ROMM primaries, D50), as LibRaw's output_color=4 with gamma 1.
  cmsCIExyYTRIPLE prophoto = {{0.7347, 0.2653, 1.0}, {0.1596, 0.8404, 1.0}, {0.0366, 0.0001, 1.0}};
  std::vector<double> total, t_decode, t_develop, t_cms;
  int err = 0;
  for (int it = 0; it < iters + 1 && !err; it++) {  // iteration 0 is a warm-up
    LibRaw *r = new LibRaw;
    double t0 = now_ms();
    int rc = r->open_buffer(buf.data(), buf.size());
    if (rc == LIBRAW_SUCCESS) rc = r->unpack();
    double t1 = now_ms(), t2 = t1, t3 = t1;
    if (rc == LIBRAW_SUCCESS) {
      r->imgdata.params.user_qual = 0;       // bilinear
      r->imgdata.params.use_camera_wb = 1;   // as-shot white balance
      r->imgdata.params.output_bps = 16;
      r->imgdata.params.output_color = 4;    // ProPhoto primaries
      r->imgdata.params.gamm[0] = 1.0;       // linear
      r->imgdata.params.gamm[1] = 1.0;
      r->imgdata.params.no_auto_bright = 1;
      rc = r->dcraw_process();
    }
    libraw_processed_image_t *img = nullptr;
    if (rc == LIBRAW_SUCCESS) {
      int e = 0;
      img = r->dcraw_make_mem_image(&e);
      if (!img) rc = e ? e : LIBRAW_UNSPECIFIED_ERROR;
    }
    if (rc == LIBRAW_SUCCESS) {
      t2 = now_ms();
      cmsToneCurve *lin = cmsBuildGamma(nullptr, 1.0);
      cmsToneCurve *curves[3] = {lin, lin, lin};
      cmsHPROFILE in = cmsCreateRGBProfile(cmsD50_xyY(), &prophoto, curves);
      cmsHPROFILE out = cmsCreate_sRGBProfile();
      cmsHTRANSFORM xf = cmsCreateTransform(in, TYPE_RGB_16, out, TYPE_RGBA_8, INTENT_PERCEPTUAL, 0);
      int w = img->width, h = img->height;
      std::vector<unsigned char> rgba((size_t)w * h * 4);
      const unsigned short *src = reinterpret_cast<const unsigned short *>(img->data);
#pragma omp parallel for schedule(dynamic, 1)
      for (int y = 0; y < h; y += 32) {
        int n = std::min(32, h - y);
        cmsDoTransform(xf, src + (size_t)y * w * 3, rgba.data() + (size_t)y * w * 4, (cmsUInt32Number)w * n);
      }
      cmsDeleteTransform(xf);
      cmsCloseProfile(in);
      cmsCloseProfile(out);
      cmsFreeToneCurve(lin);
      t3 = now_ms();
    }
    if (img) LibRaw::dcraw_clear_mem(img);
    if (rc != LIBRAW_SUCCESS) err = rc;
    else if (it > 0) {
      total.push_back(t3 - t0);
      t_decode.push_back(t1 - t0);
      t_develop.push_back(t2 - t1);
      t_cms.push_back(t3 - t2);
    }
    delete r;
  }
  struct rusage ru;
  getrusage(RUSAGE_SELF, &ru);
  long rss_kb = ru.ru_maxrss;
#ifdef __APPLE__
  rss_kb /= 1024;
#endif
  std::vector<double> sorted = total;
  std::sort(sorted.begin(), sorted.end());
  printf("{\"error\":%d,\"error_text\":\"%s\",\"ms_median\":%.4f,\"ms_min\":%.4f,\"runs\":%zu,"
         "\"decode_ms\":%.4f,\"develop_ms\":%.4f,\"cms_ms\":%.4f,\"maxrss_kb\":%ld}\n",
         err, err ? esc(libraw_strerror(err)).c_str() : "", med(total), sorted.empty() ? -1.0 : sorted[0],
         total.size(), med(t_decode), med(t_develop), med(t_cms), rss_kb);
  return 0;
}
"""

# rawler (the Rust decoder of dnglab), from crates.io. decode = rawler::decode() of an
# in-memory source (sensor data + metadata); rgb = decode + RawDevelop::default()
# (scaling, PPG demosaic, WB, camera matrix, crop, sRGB gamma; f32) + quantisation to RGB8.
RAWLER_CARGO_TOML = """[package]
name = "rawler-bench"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
rawler = "={version}"
libc = "0.2"

[profile.release]
debug = false

[workspace]
"""

RAWLER_BENCH_RS = r"""
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::{Intermediate, RawDevelop};
use rawler::rawsource::RawSource;
use std::sync::Arc;
use std::time::Instant;

fn to_u8(v: &[f32]) -> Vec<u8> {
    v.iter().map(|x| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect()
}

fn run(src: &RawSource, rgb: bool) -> Result<usize, String> {
    let raw = rawler::decode(src, &RawDecodeParams::default()).map_err(|e| e.to_string())?;
    if !rgb {
        return Ok(std::hint::black_box(raw.width * raw.height));
    }
    let img = RawDevelop::default().develop_intermediate(&raw).map_err(|e| e.to_string())?;
    let out = match img {
        Intermediate::Monochrome(p) => to_u8(&p.data),
        Intermediate::ThreeColor(p) => to_u8(&p.flatten()),
        Intermediate::FourColor(p) => to_u8(&p.flatten()),
    };
    Ok(std::hint::black_box(out.len()))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (mut iters, mut mode, mut file) = (7usize, "decode".to_string(), None);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--iterations" => { iters = args[i + 1].parse().unwrap(); i += 1; }
            "--mode" => { mode = args[i + 1].clone(); i += 1; }
            _ => file = Some(args[i].clone()),
        }
        i += 1;
    }
    let buf = std::fs::read(file.expect("FILE")).expect("read");
    let src = RawSource::new_from_shared_vec(Arc::new(buf));
    std::panic::set_hook(Box::new(|_| {}));
    let mut times = Vec::new();
    let mut err = String::new();
    for it in 0..=iters {
        let t0 = Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&src, mode == "rgb")));
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        match r {
            Ok(Ok(_)) => if it > 0 { times.push(ms) },
            Ok(Err(e)) => { err = e; break; }
            Err(p) => {
                err = p.downcast_ref::<&str>().map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
                err = format!("panic: {err}");
                break;
            }
        }
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let rss_kb = if cfg!(target_os = "macos") { ru.ru_maxrss / 1024 } else { ru.ru_maxrss };
    let med = times.get(times.len() / 2).copied().unwrap_or(-1.0);
    let min = times.first().copied().unwrap_or(-1.0);
    let esc: String = err.chars().map(|c| if c == '"' || c == '\\' || (c as u32) < 32 { ' ' } else { c }).collect();
    println!("{{\"error\":{},\"error_text\":\"{}\",\"ms_median\":{:.4},\"ms_min\":{:.4},\"runs\":{},\"maxrss_kb\":{}}}",
             if err.is_empty() { 0 } else { 1 }, esc, med, min, times.len(), rss_kb);
}
"""

# rawpy (the Python binding of LibRaw, with the LibRaw build its wheels bundle). decode =
# rawpy.imread() of the bytes + raw_image (unpack); rgb = + postprocess() with bilinear
# demosaic, camera WB, 8-bit sRGB (the same LibRaw settings as the LibRaw rgb mode).
RAWPY_BENCH_PY = r'''
import io, json, resource, sys, time
import rawpy
iters, mode, file = 7, "decode", None
a = sys.argv[1:]
i = 0
while i < len(a):
    if a[i] == "--iterations": iters = int(a[i + 1]); i += 1
    elif a[i] == "--mode": mode = a[i + 1]; i += 1
    elif a[i] == "--version": print(rawpy.__version__, rawpy.libraw_version); sys.exit(0)
    else: file = a[i]
    i += 1
scale = 1024 if sys.platform == "darwin" else 1
base_kb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss // scale
buf = open(file, "rb").read()
times, err = [], ""
for it in range(iters + 1):
    t0 = time.perf_counter()
    try:
        with rawpy.imread(io.BytesIO(buf)) as r:
            if mode == "decode":
                x = r.raw_image
            else:
                x = r.postprocess(demosaic_algorithm=rawpy.DemosaicAlgorithm.LINEAR, use_camera_wb=True, output_bps=8)
            del x
    except Exception as e:
        err = (type(e).__name__ + ": " + str(e))[:200]
        break
    if it > 0:
        times.append((time.perf_counter() - t0) * 1e3)
times.sort()
rss_kb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss // scale
print(json.dumps({"error": 1 if err else 0, "error_text": err, "ms_median": times[len(times) // 2] if times else -1,
                  "ms_min": times[0] if times else -1, "runs": len(times), "maxrss_kb": rss_kb, "base_rss_kb": base_kb}))
'''

# Display names, in chart/table order.
LIBS = {
    "flashraw": "FlashRaw", "libraw": "LibRaw", "rawspeed": "RawSpeed", "rawler": "rawler",
    "rawpy": "rawpy", "dcraw": "dcraw", "lcms": "LibRaw + Little CMS",
}
# Which library measures which mode. "rgb" for lcms is the LibRaw + Little CMS pipeline.
LIB_MODES = {
    "flashraw": ("decode", "rgb", "rgb-half", "rgb-gpu"), "libraw": ("decode", "rgb", "rgb-half"),
    "rawspeed": ("decode",), "rawler": ("decode", "rgb"), "rawpy": ("decode", "rgb"),
    "dcraw": ("decode", "rgb"), "lcms": ("rgb",),
}
# Libraries without any multithreading: measured once, on one thread, and shown with that
# time in the all-cores comparisons too.
SINGLE_THREADED = {"dcraw"}


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

    if a.lcms_src:
        # Little CMS: its sources compiled into a static library (no configure step needed).
        src = os.path.abspath(a.lcms_src)
        lib = os.path.join(a.bin_dir, "lcms2", "liblcms2.a")
        if not os.path.exists(lib):
            obj = os.path.join(a.bin_dir, "lcms2", "obj")
            os.makedirs(obj, exist_ok=True)
            for c in sorted(glob.glob(os.path.join(src, "src", "*.c"))):
                sh([a.cc, "-O3", "-w", f"-I{src}/include", "-c", c, "-o",
                    os.path.join(obj, os.path.basename(c)[:-2] + ".o")] + native)
            sh(["ar", "rcs", lib] + sorted(glob.glob(os.path.join(obj, "*.o"))))
        cpp = os.path.join(a.bin_dir, "libraw_lcms_bench.cpp")
        with open(cpp, "w") as o:
            o.write(LIBRAW_LCMS_BENCH_CPP)
        for variant, extra_c, extra_l in (("st", [], []), ("omp", omp_c, omp_l)):
            tree = os.path.join(a.bin_dir, f"libraw-{variant}")
            if not os.path.exists(os.path.join(tree, "lib", "libraw.a")):
                sys.exit("build LibRaw first (--libraw-src)")
            out = os.path.join(a.bin_dir, f"libraw_lcms_bench_{variant}")
            sh([cxx, "-O3", "-std=c++17", "-w", f"-I{tree}", f"-I{src}/include", cpp, os.path.join(tree, "lib", "libraw.a"),
                lib] + native + zinc + zlib + extra_c + extra_l + ["-lm", "-lpthread", "-o", out])
            info[f"lcms_{variant}"] = out
        info["lcms_version"] = subprocess.run([info["lcms_st"], "--version"], capture_output=True,
                                              text=True).stdout.strip()

    if a.dcraw_src:
        # dcraw.c without its optional libjpeg / LittleCMS dependencies.
        out = os.path.join(a.bin_dir, "dcraw")
        sh([a.cc, "-O3", "-w", "-DNODEPS", os.path.abspath(a.dcraw_src), "-lm", "-o", out] + native)
        info["dcraw"] = out
        p = subprocess.run([out], capture_output=True, text=True)
        info["dcraw_version"] = next((l.split("v", 1)[-1].strip() for l in (p.stdout + p.stderr).splitlines()
                                      if "raw photo decoder" in l.lower()), None)

    if a.rawler_version:
        proj = os.path.join(a.bin_dir, "rawler-bench")
        os.makedirs(os.path.join(proj, "src"), exist_ok=True)
        with open(os.path.join(proj, "Cargo.toml"), "w") as o:
            o.write(RAWLER_CARGO_TOML.replace("{version}", a.rawler_version))
        with open(os.path.join(proj, "src", "main.rs"), "w") as o:
            o.write(RAWLER_BENCH_RS)
        sh([a.cargo, "build", "--release", "--manifest-path", os.path.join(proj, "Cargo.toml")])
        info["rawler"] = os.path.join(proj, "target", "release", "rawler-bench")
        info["rawler_version"] = a.rawler_version
        info["rustc"] = subprocess.run([os.path.join(os.path.dirname(a.cargo), "rustc") if os.path.dirname(a.cargo) else "rustc", "--version"], capture_output=True, text=True).stdout.strip()

    if a.rawpy:
        venv = os.path.join(a.bin_dir, "rawpy-venv")
        if not os.path.exists(os.path.join(venv, "bin", "pip")):
            shutil.rmtree(venv, ignore_errors=True)
            if subprocess.run([a.python, "-m", "venv", venv]).returncode != 0:
                # No ensurepip (e.g. Debian without python3-venv): bootstrap pip with get-pip.py.
                shutil.rmtree(venv, ignore_errors=True)
                sh([a.python, "-m", "venv", "--without-pip", venv])
                getpip = os.path.join(a.bin_dir, "get-pip.py")
                sh(["curl", "-sSL", "-o", getpip, "https://bootstrap.pypa.io/get-pip.py"])
                sh([os.path.join(venv, "bin", "python"), getpip, "-q"])
        sh([os.path.join(venv, "bin", "python"), "-m", "pip", "install", "-q", "--upgrade", "pip"])
        sh([os.path.join(venv, "bin", "python"), "-m", "pip", "install", "-q", a.rawpy])
        script = os.path.join(a.bin_dir, "rawpy_bench.py")
        with open(script, "w") as o:
            o.write(RAWPY_BENCH_PY)
        info["rawpy"] = [os.path.join(venv, "bin", "python"), script]
        info["rawpy_version"] = subprocess.run(info["rawpy"] + ["--version"], capture_output=True,
                                               text=True).stdout.strip()
        info["python"] = subprocess.run([os.path.join(venv, "bin", "python"), "--version"], capture_output=True,
                                        text=True).stdout.strip()

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


def run_dcraw(exe, mode, path, iters):
    """dcraw is a program, not a library: time whole processes (1 warm-up + iters), output
    to /dev/null. decode = `-D -4` (raw sensor values, 16-bit PGM); rgb = `-q 0 -w` (bilinear,
    camera WB, 8-bit sRGB PPM). Peak RSS from the child's rusage."""
    args = {"decode": ["-c", "-D", "-4"], "rgb": ["-c", "-q", "0", "-w"]}[mode]
    times, rss, err = [], 0, None
    # dcraw "succeeds" on some files it cannot decode (e.g. Canon CR3, newer Fujifilm
    # compressed RAF), writing an all-zero image: check the warm-up run's output.
    # The image goes to a temporary file, read back in chunks: this process must stay small,
    # since a child's ru_maxrss can include the parent's peak (Linux records the old address
    # space's high-water mark at exec).
    with tempfile.TemporaryFile(dir=os.path.dirname(os.path.abspath(exe))) as tf:
        p = subprocess.run([exe] + args + [path], stdout=tf, stderr=subprocess.DEVNULL)
        if p.returncode == 0:
            tf.seek(0)
            head = tf.read(64)
            nonzero = False
            if head.count(b"\n") >= 3:  # PGM/PPM: magic, size, maxval, then the samples
                tf.seek(head.index(b"\n", head.index(b"\n", head.index(b"\n") + 1) + 1) + 1)
                while not nonzero:
                    chunk = tf.read(1 << 20)
                    if not chunk:
                        break
                    nonzero = bool(chunk.strip(b"\0"))
            if not nonzero:
                return {"ok": False, "error": "dcraw wrote an all-zero image (it cannot decode this compression)"}
    for it in range(1, iters + 1):
        with open(os.devnull, "wb") as null:
            t0 = time.perf_counter()
            p = subprocess.Popen([exe] + args + [path], stdout=null, stderr=subprocess.PIPE)
            _, status, ru = os.wait4(p.pid, 0)
            ms = (time.perf_counter() - t0) * 1e3
            stderr = p.stderr.read().decode(errors="replace").strip()
            p.stderr.close()
            p.returncode = os.waitstatus_to_exitcode(status)
        if p.returncode != 0:
            err = (stderr.splitlines() or [f"exit {p.returncode}"])[-1]
            break
        rss = max(rss, ru.ru_maxrss // (1024 if sys.platform == "darwin" else 1))
        if it > 0:
            times.append(ms)
    if err or not times:
        return {"ok": False, "error": err or "no runs"}
    times.sort()
    return {"ok": True, "ms": times[len(times) // 2], "ms_min": times[0], "rss_mb": rss / 1024}


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
    need = {"libraw": "libraw_st", "rawspeed": "rawspeed", "rawler": "rawler", "rawpy": "rawpy",
            "dcraw": "dcraw", "lcms": "lcms_st"}
    libs = [l for l in a.libs if l == "flashraw" or build.get(need[l])]
    for l in set(a.libs) - set(libs):
        print(f"not built, skipped: {l}", file=sys.stderr)
    jobs = []
    for f in files:
        for threads in a.threads:
            for mode in a.modes:
                for lib in libs:
                    if mode not in LIB_MODES[lib]:
                        continue
                    if mode == "rgb-gpu" and threads != "all":
                        continue
                    if lib in SINGLE_THREADED and threads != "1":
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
        elif lib == "dcraw" and f.lower().endswith(".cr3"):
            # dcraw 9.28 parses CR3 containers but its CRX decoder is an empty stub: it
            # "succeeds" without reading any image data, so do not count it.
            rec = {"ok": False, "error": "dcraw 9.28 has no CR3 decoder (canon_crx_load_raw is empty)"}
        elif lib == "dcraw":
            rec = run_dcraw(build["dcraw"], mode, path, a.iterations)
        else:
            if lib == "rawspeed":
                cmd = [build["rawspeed"], "--cameras", build["rawspeed_cameras"], "--iterations", it, path]
            elif lib == "lcms":
                cmd = [build["lcms_st"] if threads == "1" else build.get("lcms_omp", build["lcms_st"]),
                       "--iterations", it, path]
            elif lib == "rawpy":
                cmd = build["rawpy"] + ["--iterations", it, "--mode", mode, path]
            else:
                cmd = [build["rawler"], "--iterations", it, "--mode", mode, path]
            j, err, _ = run_one(cmd, env)
            rec = {"ok": False, "error": err} if j is None else (
                {"ok": False, "error": j["error_text"]} if j["error"] else
                {"ok": True, "ms": j["ms_median"], "ms_min": j["ms_min"], "rss_mb": j["maxrss_kb"] / 1024})
            if j and not j["error"]:
                for k in ("decode_ms", "develop_ms", "cms_ms"):
                    if k in j:
                        rec[k] = j[k]
                if "base_rss_kb" in j:
                    rec["base_rss_mb"] = j["base_rss_kb"] / 1024
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


def load_results(paths):
    """Measurements of one or more passes: for every (file, library, mode, threads) the pass
    with the lowest median is kept (a burst of background load in one pass does not count
    against anyone); `ms_passes` lists every pass's median. Returns (by key, keys in order)."""
    by, order, passes = {}, [], {}
    for path in paths:
        for l in open(path):
            r = json.loads(l)
            k = (r["file"], r["lib"], r["mode"], r["threads"])
            if k not in by:
                order.append(k)
            prev = r.get("ms_passes")
            passes.setdefault(k, []).extend(prev if prev else [r["ms"] if r.get("ok") else None])
            old = by.get(k)
            if old is None or (r.get("ok") and (not old.get("ok") or r["ms"] < old["ms"])):
                by[k] = r
    for k, v in passes.items():
        by[k] = dict(by[k], ms_passes=[None if x is None else round(x, 3) for x in v])
    return by, order


def read_popular(path):
    popular = []
    for line in open(path):
        if line.strip() and not line.startswith("#"):
            p = line.rstrip("\n").split("\t")
            popular.append((p[0], p[1], p[2] if len(p) > 2 else ""))
    return popular


class Results:
    """Lookups over load_results(), shared by the report and tools/bench/charts.py."""

    def __init__(self, paths):
        self.by, self.order = load_results(paths)
        self.meta = {}
        for k in self.order:
            r = self.by[k]
            if k[1] == "flashraw" and k[2] == "decode" and r.get("ok"):
                self.meta.setdefault(k[0], r)
        self.files = list(dict.fromkeys(k[0] for k in self.order))
        self.libs = set(k[1] for k in self.order)

    def rec(self, f, lib, mode, th):
        """The measurement; a single-threaded-only library answers for "all" with its 1-thread run."""
        if lib in SINGLE_THREADED:
            th = "1"
        return self.by.get((f, lib, mode, th))

    def ms(self, f, lib, mode, th):
        r = self.rec(f, lib, mode, th)
        if not r or not r.get("ok"):
            return None
        if mode == "rgb-gpu" and not str(r.get("postprocess") or "").startswith("GPU"):
            return None  # the GPU pipeline did not handle this file (it ran on the CPU)
        return r["ms"]

    def rss(self, f, lib, mode, th):
        r = self.rec(f, lib, mode, th)
        if mode == "rgb-gpu" and self.ms(f, lib, mode, th) is None:
            return None
        return r["rss_mb"] if r and r.get("ok") and r.get("rss_mb") else None

    def compare(self, files, lib, mode, th, fr_mode=None):
        """Per-file speedups and memory ratios of FlashRaw (in fr_mode, default the same mode)
        against `lib`: other library's time (peak RSS) ÷ FlashRaw's."""
        sp, mem = [], []
        for f in files:
            x, y = self.ms(f, "flashraw", fr_mode or mode, th), self.ms(f, lib, mode, th)
            if x and y:
                sp.append(y / x)
                a, b = self.rss(f, "flashraw", fr_mode or mode, th), self.rss(f, lib, mode, th)
                if a and b:
                    mem.append(b / a)
        return sp, mem


# Comparisons reported, in order: (mode, library, label).
COMPARISONS = [
    ("decode", "libraw", "decode vs LibRaw"),
    ("decode", "rawspeed", "decode vs RawSpeed"),
    ("decode", "rawler", "decode vs rawler"),
    ("decode", "rawpy", "decode vs rawpy"),
    ("decode", "dcraw", "decode vs dcraw"),
    ("rgb", "libraw", "RAW→RGB vs LibRaw"),
    ("rgb", "lcms", "RAW→sRGB vs LibRaw + Little CMS"),
    ("rgb", "rawler", "RAW→RGB vs rawler"),
    ("rgb", "rawpy", "RAW→RGB vs rawpy"),
    ("rgb", "dcraw", "RAW→RGB vs dcraw"),
]


def cmd_report(a):
    R = Results(a.results)
    meta, files = R.meta, R.files
    popular = read_popular(a.popular) if a.popular else []
    pop_files = [p[0] for p in popular if p[0] in meta]
    has_gpu = any(k[2] == "rgb-gpu" for k in R.order)

    def cell(f, lib, mode, th, libs):
        r = R.rec(f, lib, mode, th)
        if r is None:
            return "–"
        if not r.get("ok"):
            return "❌"
        if mode == "rgb-gpu" and not str(r.get("postprocess") or "").startswith("GPU"):
            return "CPU¹"
        fastest = min((v for l in libs for v in [R.ms(f, l, mode, th)] if v), default=None)
        return f"**{fmt_ms(r['ms'])}**" if r["ms"] == fastest else fmt_ms(r["ms"])

    def mb(f, lib, mode, th):
        r = R.rec(f, lib, mode, th)
        v = R.rss(f, lib, mode, th)
        return "–" if r is None else ("❌" if not v else "%.0f" % v)

    def label(f, cam, note):
        # The note names the camera's own compression mode; skip it when it only repeats the format.
        if not note or note.upper() == str(meta.get(f, {}).get("format")).upper() or note in ("RW2", "ORF", "PEF"):
            return cam
        return f"{cam} ({note})"

    def codec(f):
        m = meta.get(f, {})
        return f"{m.get('format', '?')} · {m.get('compression', '?')}"

    def name(l):
        return LIBS[l] + (" ²" if l in SINGLE_THREADED else "")

    out = [f"# {a.title or 'FlashRaw compared'}\n"]
    if a.header:
        out.append(open(a.header).read().rstrip() + "\n")
    out.append("Times in ms (median of the timed runs; lower is better); memory = peak RSS in MB. "
               "❌ = the library failed or does not support the file; – = not measured. "
               "Speedup = other library's time ÷ FlashRaw's time; memory ratio = other library's peak RSS ÷ "
               "FlashRaw's (> 1 = FlashRaw uses less). **Bold** = fastest. ² dcraw has no multithreading: its "
               "one-thread time is used in the all-cores columns too.\n")

    def summary(sel, title):
        lines = [f"### {title}\n",
                 "| Comparison | threads | files | geomean speedup | median speedup | FlashRaw faster | median memory ratio |",
                 "|---|---|---:|---:|---:|---:|---:|"]
        rows = [c for c in COMPARISONS if c[1] in R.libs]
        if has_gpu:
            rows.append(("rgb", "libraw", "RAW→RGB, FlashRaw GPU vs LibRaw", "rgb-gpu"))
        for row in rows:
            mode, lib, lab = row[:3]
            for th in ("all", "1"):
                if len(row) > 3 and th != "all":
                    continue
                sp, mem = R.compare(sel, lib, mode, th, row[3] if len(row) > 3 else None)
                if sp:
                    wins = sum(1 for s in sp if s > 1)
                    lines.append(f"| {lab} | {th} | {len(sp)} | **{fmt_x(gmean(sp))}** | {fmt_x(statistics.median(sp))} | "
                                 f"{wins}/{len(sp)} | {'%.2f' % statistics.median(mem) if mem else ''} |")
        return lines + [""]

    if pop_files:
        out += summary(pop_files, f"Summary: popular cameras ({len(pop_files)} files)")
    allf = [f for f in files if f in meta]
    if len(allf) > len(pop_files):
        out += summary(allf, f"Summary: all files ({len(allf)}; rawler, rawpy, dcraw and Little CMS were measured "
                             f"on the popular cameras only)")

    dec_libs = [l for l in ("flashraw", "libraw", "rawspeed", "rawler", "rawpy", "dcraw") if l in R.libs]
    rgb_libs = [l for l in ("flashraw", "libraw", "lcms", "rawler", "rawpy", "dcraw") if l in R.libs]
    if popular:
        for th, title in (("all", "all cores"), ("1", "one thread")):
            out.append(f"## Popular cameras: decode, {title} (ms)\n")
            out.append("| Camera | Format · compression | MP | " + " | ".join(name(l) for l in dec_libs) + " |")
            out.append("|---|---|---:|" + "---:|" * len(dec_libs))
            for f, cam, note in popular:
                if f in meta:
                    out.append(f"| {label(f, cam, note)} | {codec(f)} | {meta[f].get('mp') or 0:.1f} | "
                               + " | ".join(cell(f, l, "decode", th, dec_libs) for l in dec_libs) + " |")
            out.append("")
            gpu = has_gpu and th == "all"
            out.append(f"## Popular cameras: RAW → RGB, {title} (ms)\n")
            out.append("| Camera | " + " | ".join(name(l) for l in rgb_libs) + (" | FlashRaw GPU |" if gpu else " |"))
            out.append("|---|" + "---:|" * (len(rgb_libs) + (1 if gpu else 0)))
            for f, cam, note in popular:
                if f in meta:
                    line = f"| {label(f, cam, note)} | " + " | ".join(cell(f, l, "rgb", th, rgb_libs) for l in rgb_libs) + " |"
                    if gpu:
                        line += f" {cell(f, 'flashraw', 'rgb-gpu', 'all', [])} |"
                    out.append(line)
            out.append("")
            if gpu:
                out.append("FlashRaw GPU = `--backend gpu --output rgba8`: CPU decode or GPU unpack, GPU develop, "
                           "frame downloaded to host memory. ¹ FlashRaw's GPU pipeline does not handle this file's "
                           "processing yet and ran it on the CPU.\n")
        out.append("## Popular cameras: peak memory, all cores (MB)\n")
        out.append("| Camera | MP | decode: " + " | ".join(name(l) for l in dec_libs) + " | RGB: "
                   + " | ".join(name(l) for l in rgb_libs) + " |")
        out.append("|---|---:|" + "---:|" * (len(dec_libs) + len(rgb_libs)))
        for f, cam, note in popular:
            if f in meta:
                out.append(f"| {label(f, cam, note)} | {meta[f].get('mp') or 0:.1f} | "
                           + " | ".join(mb(f, l, "decode", "all") for l in dec_libs) + " | "
                           + " | ".join(mb(f, l, "rgb", "all") for l in rgb_libs) + " |")
        out.append("")
        if "lcms" in R.libs:
            out.append("## LibRaw + Little CMS pipeline, stages (all cores, ms)\n")
            out.append("decode = LibRaw `open_buffer` + `unpack`; develop = `dcraw_process` to 16-bit linear ProPhoto "
                       "+ `dcraw_make_mem_image`; Little CMS = profiles, transform and conversion to 8-bit sRGB RGBA; "
                       "FlashRaw = RAW → RGBA8 in one pass.\n")
            out += ["| Camera | LibRaw decode | LibRaw develop | Little CMS | total | FlashRaw | speedup |",
                    "|---|---:|---:|---:|---:|---:|---:|"]
            for f, cam, note in popular:
                r = R.rec(f, "lcms", "rgb", "all")
                x = R.ms(f, "flashraw", "rgb", "all")
                if f in meta and r and r.get("ok") and x:
                    out.append(f"| {label(f, cam, note)} | {fmt_ms(r.get('decode_ms'))} | {fmt_ms(r.get('develop_ms'))} | "
                               f"{fmt_ms(r.get('cms_ms'))} | {fmt_ms(r['ms'])} | **{fmt_ms(x)}** | {fmt_x(r['ms'] / x)} |")
            out.append("")

    groups = {}
    for f in allf:
        groups.setdefault((meta[f].get("format"), meta[f].get("compression")), []).append(f)
    cols = [c for c in COMPARISONS if c[1] in R.libs]
    out.append("## Per format (geometric mean speedup of FlashRaw, all cores)\n")
    out.append("| Format | Compression | files | " + " | ".join(
        c[2].replace("RAW→sRGB vs LibRaw + Little CMS", "RGB vs LibRaw + LCMS").replace("RAW→RGB", "RGB") for c in cols) + " |")
    out.append("|---|---|---:|" + "---:|" * len(cols))
    for (fmt, comp), fs in sorted(groups.items(), key=lambda kv: (str(kv[0][0]), str(kv[0][1]))):
        vals = []
        for m, l, _ in cols:
            sp, _ = R.compare(fs, l, m, "all")
            vals.append(fmt_x(gmean(sp)) if sp else ("❌" if any(R.rec(f, l, m, "all") for f in fs) else "–"))
        out.append(f"| {fmt} | {comp} | {len(fs)} | " + " | ".join(vals) + " |")
    out.append("")

    core = [l for l in ("flashraw", "libraw", "rawspeed") if l in R.libs]
    out.append("## All files (FlashRaw, LibRaw, RawSpeed)\n")
    out += ["| File | Camera | Codec | MP | threads | Decode FlashRaw | LibRaw | RawSpeed | RGB FlashRaw | LibRaw | "
            "Peak RSS MB (decode) FR / LR / RS |", "|---|---|---|---:|---|---:|---:|---:|---:|---:|---|"]
    for f in files:
        if f not in meta:
            out.append(f"| {os.path.basename(f)} | | | | | ❌ | | | | | |")
            continue
        for th in ("all", "1"):
            if (f, "flashraw", "decode", th) not in R.by:
                continue
            rss = " / ".join(mb(f, l, "decode", th) for l in ("flashraw", "libraw", "rawspeed"))
            out.append(f"| {os.path.basename(f)} | {meta[f].get('camera') or ''} | {codec(f)} | {meta[f].get('mp') or 0:.1f} | {th} | "
                       f"{cell(f, 'flashraw', 'decode', th, core)} | {cell(f, 'libraw', 'decode', th, core)} | "
                       f"{cell(f, 'rawspeed', 'decode', th, core)} | {cell(f, 'flashraw', 'rgb', th, ['flashraw', 'libraw'])} | "
                       f"{cell(f, 'libraw', 'rgb', th, ['flashraw', 'libraw'])} | {rss} |")
    out.append("")
    fails = [R.by[k] for k in R.order if not R.by[k].get("ok")]
    if fails:
        out.append("## Failures and unsupported files\n")
        out += ["| File | library | mode | threads | error |", "|---|---|---|---|---|"]
        for r in fails:
            out.append(f"| {os.path.basename(r['file'])} | {LIBS.get(r['lib'], r['lib'])} | {r['mode']} | {r['threads']} | "
                       f"{str(r.get('error'))[:120].replace('|', '/')} |")
        out.append("")
    with open(a.markdown, "w") as o:
        o.write("\n".join(out))
    print(f"wrote {a.markdown}", file=sys.stderr)


def cmd_merge(a):
    """Merge passes into one .jsonl (lowest median kept, every pass's median in ms_passes)."""
    by, order = load_results(a.results)
    with open(a.out, "w") as o:
        for k in order:
            o.write(json.dumps(by[k]) + "\n")
    print(f"wrote {a.out} ({len(order)} measurements)", file=sys.stderr)


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
    b.add_argument("--lcms-src", help="Little CMS 2 source tree (for the LibRaw + Little CMS pipeline; "
                                      "needs the LibRaw builds)")
    b.add_argument("--dcraw-src", help="dcraw.c")
    b.add_argument("--rawler-version", help="rawler version from crates.io, e.g. 0.8.0")
    b.add_argument("--cargo", default="cargo")
    b.add_argument("--rawpy", help="pip requirement for rawpy, e.g. rawpy==0.25.1 (installed in a venv)")
    b.add_argument("--python", default=sys.executable, help="interpreter for the rawpy venv")
    r = sub.add_parser("run", help="run the benchmark")
    r.add_argument("inputs", nargs="+", help="files, or .tsv/.txt lists of corpus-relative paths (first column)")
    r.add_argument("--corpus", default=os.path.join(ROOT, "corpus"))
    r.add_argument("--flashraw", default=os.path.join(ROOT, "target", "release", "flashraw"))
    r.add_argument("--bin-dir", default=DEFAULT_BIN)
    r.add_argument("--iterations", type=int, default=7)
    r.add_argument("--libs", nargs="+", default=["flashraw", "libraw", "rawspeed"], choices=list(LIBS))
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
    m = sub.add_parser("merge", help="merge passes into one .jsonl (lowest median per measurement)")
    m.add_argument("results", nargs="+")
    m.add_argument("--out", required=True)
    a = ap.parse_args()
    {"build": cmd_build, "run": cmd_run, "report": cmd_report, "merge": cmd_merge}[a.cmd](a)


if __name__ == "__main__":
    main()
