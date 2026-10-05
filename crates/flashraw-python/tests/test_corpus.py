"""Corpus-backed equivalence with the native CLI.

For a sample of corpus files across formats (FLASHRAW_CORPUS, skipped when absent), the
FNV-1a hash of the ``raw-u16`` NumPy array must equal the ``hash_full`` printed by the native
``flashraw verify --apply-opcodes`` (FLASHRAW_CLI or target/release/flashraw), and the
geometry and metadata must agree. FLASHRAW_CORPUS_SAMPLE sets the number of files per
extension (default 2).
"""

from __future__ import annotations

import os
from collections import defaultdict
from pathlib import Path
from typing import Dict, List

import numpy as np
import pytest

import flashraw
from conftest import cli_binary, corpus_root, native_verify

MAX_SIZE = 80 * 1024 * 1024


def _sample() -> List[Path]:
    root = corpus_root()
    if root is None:
        return []
    per_ext = int(os.environ.get("FLASHRAW_CORPUS_SAMPLE", "2"))
    by_ext: Dict[str, List[Path]] = defaultdict(list)
    for p in sorted(root.rglob("*")):
        if p.is_file() and p.suffix and p.stat().st_size <= MAX_SIZE:
            by_ext[p.suffix.lower()].append(p)
    out: List[Path] = []
    for ext in sorted(by_ext):
        files = by_ext[ext]
        # Spread the picks over makes: evenly spaced through the sorted list.
        step = max(len(files) // per_ext, 1)
        out.extend(files[::step][:per_ext])
    return out


SAMPLE = _sample()

pytestmark = [
    pytest.mark.skipif(not SAMPLE, reason="corpus not available (set FLASHRAW_CORPUS)"),
    pytest.mark.skipif(cli_binary() is None, reason="native CLI not built (cargo build --release -p flashraw-cli)"),
]


@pytest.mark.parametrize("path", SAMPLE, ids=lambda p: str(p.relative_to(corpus_root() or p.parent)))
def test_raw_u16_hash_matches_cli(path: Path) -> None:
    v = native_verify(path)
    if v is None or "hash_full" not in v or v.get("hash_full") is None:
        # The CLI could not decode it (unsupported mode): Python must fail cleanly too.
        with pytest.raises(flashraw.FlashRawError):
            flashraw.open(path).decode()
        return
    with flashraw.open(path) as raw:
        frame = raw.decode()
        m = raw.metadata
    a = frame.array
    assert a.dtype == np.uint16
    assert (frame.width, frame.height, frame.channels) == (v["raw_width"], v["raw_height"], v["cpp"])
    assert format(flashraw._flashraw.fnv1a64(a), "016x") == v["hash_full"], path
    assert m.format == v["format"]
    assert (m.make, m.model) == (v["make"], v["model"])
    assert (m.canonical_make, m.canonical_model) == (v["canonical_make"], v["canonical_model"])
    assert m.orientation == v["orientation"]
    assert frame.incomplete == v["incomplete"]
    assert len(frame.warnings) == len(v["warnings"])
    # Same samples through a bytes input (zero-copy) as through the memory-mapped path.
    with flashraw.open(path.read_bytes()) as raw:
        assert format(flashraw._flashraw.fnv1a64(raw.decode().array), "016x") == v["hash_full"]
