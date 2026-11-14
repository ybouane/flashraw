"""DNG camera profiles: the embedded profile, a hand-built .dcp and the error cases."""

from __future__ import annotations

import pathlib
import struct
from typing import List, Tuple

import numpy as np
import pytest

import flashraw

from conftest import fixture_path


def build_dcp(tone_curve: List[Tuple[float, float]]) -> bytes:
    """A minimal DNG camera profile: ColorMatrix1 (D65), a name and a tone curve. A DCP is a
    little-endian TIFF with the magic ``IIRC``."""
    cm = [0.9, -0.2, 0.0, -0.3, 1.1, 0.1, -0.05, 0.2, 0.6]
    curve = [v for p in tone_curve for v in p]
    name = b"Test profile\0"
    entries = [
        (0xC621, 11, 9, struct.pack("<9f", *cm)),  # ColorMatrix1
        (0xC65A, 3, 1, struct.pack("<H", 21)),  # CalibrationIlluminant1: D65
        (0xC6F8, 2, len(name), name),  # ProfileName
        (0xC6FC, 11, len(curve), struct.pack(f"<{len(curve)}f", *curve)),  # ProfileToneCurve
    ]
    entries.sort()
    data_off = 8 + 2 + 12 * len(entries) + 4
    body = b""
    out = b"IIRC" + struct.pack("<I", 8) + struct.pack("<H", len(entries))
    for tag, typ, count, payload in entries:
        if len(payload) <= 4:
            out += struct.pack("<HHI", tag, typ, count) + payload.ljust(4, b"\0")
        else:
            out += struct.pack("<HHII", tag, typ, count, data_off + len(body))
            body += payload
    return out + struct.pack("<I", 0) + body


def test_embedded_profile_changes_the_colours() -> None:
    with flashraw.open(fixture_path("canon-5d3-lossy.dng")) as raw:
        base = raw.decode("rgba8", demosaic="half-size")
        prof = raw.decode("rgba8", demosaic="half-size", camera_profile="embedded")
        assert prof.array.shape == base.array.shape
        assert not np.array_equal(prof.array, base.array)
        mean = lambda f: f.array[..., :3].astype(float).mean()  # noqa: E731
        assert abs(mean(prof) / mean(base) - 1) < 0.15
        lin = raw.decode("linear-rgb", demosaic="half-size", camera_profile="embedded")
        assert np.isfinite(lin.array).all()


def test_file_without_embedded_profile(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        with pytest.raises(flashraw.InvalidArgumentError, match="no embedded camera profile"):
            raw.decode("rgba8", camera_profile="embedded")
        # Without the option nothing changes.
        a = raw.decode("rgba8", demosaic="half-size")
        b = raw.decode("rgba8", demosaic="half-size", camera_profile=None)
        assert np.array_equal(a.array, b.array)


def test_dcp_bytes_and_path(mlv_path: pathlib.Path, tmp_path: pathlib.Path) -> None:
    dcp = build_dcp([(0.0, 0.0), (0.25, 0.4), (0.6, 0.8), (1.0, 1.0)])
    p = tmp_path / "test.dcp"
    p.write_bytes(dcp)
    with flashraw.open(mlv_path) as raw:
        base = raw.decode("rgba8", demosaic="half-size")
        a = raw.decode("rgba8", demosaic="half-size", camera_profile=dcp)
        b = raw.decode("rgba8", demosaic="half-size", camera_profile=p)
        c = raw.decode("rgba8", demosaic="half-size", camera_profile=str(p))
        assert np.array_equal(a.array, b.array) and np.array_equal(a.array, c.array)
        # The tone curve lifts the mid-tones.
        assert a.array[..., :3].astype(float).mean() > base.array[..., :3].astype(float).mean()
        # Raw outputs are untouched.
        r0 = raw.decode("raw-u16")
        r1 = raw.decode("raw-u16", camera_profile=dcp)
        assert np.array_equal(r0.array, r1.array)


def test_bad_profiles_raise(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        with pytest.raises(flashraw.FlashRawError):
            raw.decode("rgba8", camera_profile=b"not a profile")
        with pytest.raises(flashraw.FlashRawError):
            raw.decode("rgba8", camera_profile=b"IIRC\x08\x00\x00\x00")
