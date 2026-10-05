"""Shared fixtures: the bundled CC0 DNGs (npm/tests/fixtures), the optional corpus and the
native CLI used as the source of expected hashes."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any, Dict, Optional

import pytest

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
FIXTURES = REPO / "npm" / "tests" / "fixtures"
EXPECTED: Dict[str, Dict[str, Any]] = json.loads((FIXTURES / "expected.json").read_text())


def fixture_path(name: str) -> Path:
    return FIXTURES / name


@pytest.fixture(scope="session")
def expected() -> Dict[str, Dict[str, Any]]:
    return EXPECTED


@pytest.fixture(params=sorted(EXPECTED), scope="session")
def fixture_name(request: pytest.FixtureRequest) -> str:
    return str(request.param)


@pytest.fixture(scope="session")
def mlv_path() -> Path:
    """1920x1080 RGGB DNG with two previews (small, fast)."""
    return fixture_path("canon-5d3-mlv.dng")


@pytest.fixture(scope="session")
def chdk_bytes() -> bytes:
    """2144x1560 10-bit packed CHDK DNG whose single strip ends at the end of the file."""
    return fixture_path("canon-a410-chdk.dng").read_bytes()


def corpus_root() -> Optional[Path]:
    for p in (os.environ.get("FLASHRAW_CORPUS"), str(REPO / "corpus")):
        if p and Path(p).is_dir():
            return Path(p)
    return None


def cli_binary() -> Optional[str]:
    for c in (os.environ.get("FLASHRAW_CLI"), str(REPO / "target" / "release" / "flashraw")):
        if c and Path(c).is_file():
            return c
    return shutil.which("flashraw")


def native_verify(path: Path) -> Optional[Dict[str, Any]]:
    """`flashraw verify --json --apply-opcodes` (hashes of a default decode), or None."""
    cli = cli_binary()
    if cli is None:
        return None
    r = subprocess.run([cli, "verify", "--json", "--apply-opcodes", str(path)], capture_output=True, text=True)
    out = r.stdout.strip()
    return json.loads(out) if out else None
