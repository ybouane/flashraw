#!/usr/bin/env python3
"""Python binding overhead vs the native CLI.

For each file, the native CLI (`flashraw benchmark --json`) reports the median open and
decode times measured inside the process. The same decode is then timed from Python:

- ``py open``: wall time of ``flashraw.open(path)`` (memory-mapped);
- ``py decode``: wall time of ``RawFile.decode()``, i.e. what a Python caller waits for,
  including building the NumPy array (no copy) and the frame description;
- ``native``: wall time of the native decode call inside that ``decode()`` (what the CLI
  measures);
- ``overhead``: ``py decode - native`` (argument parsing, releasing/re-acquiring the GIL,
  wrapping the buffer in a NumPy array, converting the frame description to Python objects).

``cli dec`` and ``py dec`` come from separate runs and differ by run-to-run noise; the
overhead column is measured within the same call and is the precise binding cost.

Usage::

    python bench.py [--cli target/release/flashraw] [--iterations 9] [--output raw-u16]
                    [--corpus DIR] [FILES...]

Without FILES, a few representative files are taken from the corpus (FLASHRAW_CORPUS).
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List

import flashraw

REPO = Path(__file__).resolve().parents[3]

DEFAULT_FILES = [
    "Canon/EOS 5D Mark IV/B13A0729.CR2",
    "Canon/EOS R5/Canon_EOS_R5_RAW_ISO_100_nocrop_dual.CR3",
    "Nikon/Z 8/Nikon_Z8_raw_14_bit_lossless_compression.NEF",
    "Sony/ILCE-7RM5/7RM5-LosslessCompressedLarge.ARW",
    "Fujifilm/X100V/DSCF0734.RAF",
    "Pentax/K-1 Mark II/_K1I9784.DNG",
    "Olympus/E-M10 Mark IV/PA040002.ORF",
    "Nikon/D850/Nikon-D850-14bit-uncompressed.NEF",
]


def cli_bench(cli: str, path: Path, output: str, iterations: int) -> Dict[str, float]:
    r = subprocess.run(
        [cli, "benchmark", "--json", "--iterations", str(iterations), "--output", output, str(path)],
        capture_output=True,
        text=True,
        check=True,
    )
    v = json.loads(r.stdout.strip().splitlines()[-1])
    return {"open": v["open_ms_median"], "decode": v["decode_ms_median"], "mp": v["megapixels"]}


def py_bench(path: Path, output: str, iterations: int) -> Dict[str, float]:
    opens: List[float] = []
    walls: List[float] = []
    natives: List[float] = []
    # Warm up (page cache, thread pool, lazy metadata).
    with flashraw.open(path) as raw:
        raw.decode(output)
    for _ in range(iterations):
        t0 = time.perf_counter()
        raw = flashraw.open(path)
        opens.append((time.perf_counter() - t0) * 1e3)
        f = raw.decode(output)
        walls.append(f.stats.wall_ms)
        natives.append(f.stats.call_ms)
        raw.close()
        del f
    return {
        "open": statistics.median(opens),
        "decode": statistics.median(walls),
        "native": statistics.median(natives),
        "overhead": statistics.median(w - n for w, n in zip(walls, natives)),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="*", type=Path)
    ap.add_argument("--cli", default=os.environ.get("FLASHRAW_CLI", str(REPO / "target" / "release" / "flashraw")))
    ap.add_argument("--corpus", type=Path, default=Path(os.environ.get("FLASHRAW_CORPUS", REPO / "corpus")))
    ap.add_argument("--iterations", type=int, default=9)
    ap.add_argument("--output", default="raw-u16", choices=flashraw.OUTPUTS)
    ap.add_argument("--json", action="store_true", help="print one JSON record per file")
    args = ap.parse_args()

    files = args.files or [args.corpus / f for f in DEFAULT_FILES if (args.corpus / f).is_file()]
    if not files:
        print("no input files (pass FILES or set FLASHRAW_CORPUS)", file=sys.stderr)
        return 2
    if not Path(args.cli).is_file():
        print(f"native CLI not found at {args.cli} (cargo build --release -p flashraw-cli)", file=sys.stderr)
        return 2

    info = flashraw.build_info()
    print(f"flashraw {flashraw.__version__}, output {args.output}, {info['threads']} threads, SIMD {info['simd']}")
    header = f"{'file':<44} {'MP':>6} {'cli open':>9} {'py open':>8} {'cli dec':>8} {'py dec':>8} {'native':>8} {'overhead':>9}"
    if not args.json:
        print(header)
        print("-" * len(header))
    for path in files:
        c = cli_bench(args.cli, path, args.output, args.iterations)
        p = py_bench(path, args.output, args.iterations)
        if args.json:
            print(json.dumps({"file": str(path), "output": args.output, "cli": c, "python": p}))
            continue
        name = path.name if len(path.name) <= 44 else path.name[:41] + "..."
        print(
            f"{name:<44} {c['mp']:>6.1f} {c['open']:>7.2f}ms {p['open']:>6.2f}ms "
            f"{c['decode']:>6.1f}ms {p['decode']:>6.1f}ms {p['native']:>6.1f}ms {p['overhead']:>7.3f}ms"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
