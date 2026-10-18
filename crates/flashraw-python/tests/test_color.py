"""Output colour spaces, ICC profiles and ICC-tagged frames."""

from __future__ import annotations

import pathlib

import numpy as np
import pytest

import flashraw


def test_default_is_srgb(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        a = raw.decode("rgba8", demosaic="half-size")
        b = raw.decode("rgba8", demosaic="half-size", color_space="srgb")
        assert a.color_space == "srgb"
        assert np.array_equal(a.array, b.array)
        assert a.icc_profile is not None and a.icc_profile[36:40] == b"acsp"
        assert raw.decode().icc_profile is None


@pytest.mark.parametrize("space", [s for s in flashraw.COLOR_SPACES if s != "srgb"])
def test_spaces_change_the_pixels_and_tag_the_frame(mlv_path: pathlib.Path, space: str) -> None:
    with flashraw.open(mlv_path) as raw:
        srgb = raw.decode("rgb16", demosaic="half-size")
        other = raw.decode("rgb16", demosaic="half-size", color_space=space)
        assert other.color_space == space
        assert not np.array_equal(srgb.array, other.array)
        assert other.icc_profile == flashraw.icc_profile(space)
        lin = raw.decode("linear-rgb", demosaic="half-size", color_space=space)
        assert lin.icc_profile == flashraw.icc_profile(space, linear=True)
        assert lin.icc_profile != other.icc_profile


def test_unknown_space_and_garbage_profile(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        with pytest.raises(flashraw.InvalidArgumentError):
            raw.decode("rgba8", color_space="nope")
        with pytest.raises(flashraw.InvalidArgumentError):
            raw.decode("rgba8", output_profile=b"\x00" * 300)


@pytest.mark.skipif(not flashraw.icc_supported(), reason="built without ICC support")
def test_output_profile_matches_builtin_space(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        want = raw.decode("rgba8", demosaic="half-size", color_space="display-p3")
        got = raw.decode("rgba8", demosaic="half-size", output_profile=flashraw.icc_profile("display-p3"))
        assert got.color_space == "custom"
        diff = np.abs(want.array.astype(int) - got.array.astype(int))
        assert diff.max() <= 1
        assert got.icc_profile == flashraw.icc_profile("display-p3")
