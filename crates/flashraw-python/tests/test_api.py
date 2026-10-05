"""API coverage on the bundled fixtures."""

from __future__ import annotations

import dataclasses
import io
import json
import mmap
import pathlib

import numpy as np
import pytest

import flashraw
from conftest import EXPECTED, fixture_path


def test_version_and_build_info() -> None:
    assert isinstance(flashraw.__version__, str) and flashraw.__version__.count(".") >= 2
    info = flashraw.build_info()
    assert info["version"] == flashraw.__version__
    assert info["threads"] >= 1
    assert set(flashraw.OUTPUTS) <= set(info["outputs"])
    assert isinstance(flashraw.gpu_available(), bool)


def test_metadata_matches_expected(fixture_name: str) -> None:
    exp = EXPECTED[fixture_name]
    with flashraw.open(fixture_path(fixture_name)) as raw:
        m = raw.metadata
        assert raw.format == m.format == exp["format"]
        assert m.camera.canonical_make == m.canonical_make == exp["make"]
        assert m.camera.canonical_model == m.canonical_model == exp["model"]
        assert m.make and m.model
        assert m.camera.name.startswith(exp["make"])
        assert (m.cfa.pattern if m.cfa else None) == exp["cfa"]
        assert max(m.white_level) == exp["white"]
        assert len(m.previews) == len(raw.previews) == exp["previews"]
        assert m.orientation in range(1, 9)
        assert m.orientation_info.resolved == m.orientation
        assert m.orientation_info.source in ("exif", "maker-note", "camera-profile", "default")
        assert m.layout in ("bayer", "linear", "monochrome", "x-trans", "other-cfa", "quad-bayer", "foveon")
        assert m.dng is not None and m.dng_version is not None
        assert isinstance(m.black_level.mean, float)
        assert m.color_matrices.source in ("file", "camera-db", "none")
        for cm in m.color_matrices.color:
            assert cm.as_array().shape == (cm.rows, len(cm.values) // cm.rows)
        # Plain data all the way down: dataclasses.asdict + JSON round trip.
        json.dumps(dataclasses.asdict(m))
        fi = raw.frame_info
        assert fi.channels == exp["cpp"]
        assert fi.sample_format in ("u16", "f32")
        assert isinstance(fi.active_area, flashraw.Rect)


def test_metadata_is_frozen(mlv_path: pathlib.Path) -> None:
    m = flashraw.read_metadata(mlv_path)
    with pytest.raises(dataclasses.FrozenInstanceError):
        m.width = 1  # type: ignore[misc]
    assert m == flashraw.read_metadata(mlv_path.read_bytes())


def test_exposure_lens_and_dng_extras() -> None:
    m = flashraw.read_metadata(fixture_path("canon-5d3-lossy.dng"))
    assert isinstance(m.exposure, flashraw.Exposure)
    assert isinstance(m.lens, flashraw.Lens)
    assert isinstance(m.dng, flashraw.DngInfo)
    assert isinstance(m.dng.profile, flashraw.DngProfile)
    assert all(isinstance(o, flashraw.Opcode) for o in m.opcodes)


@pytest.mark.parametrize(
    "output,dtype,channels",
    [
        ("raw-u16", np.uint16, None),
        ("raw-f32", np.float32, None),
        ("normalized", np.float32, None),
        ("linear-rgb", np.float32, 3),
        ("rgba8", np.uint8, 4),
    ],
)
def test_output_dtype_and_shape(mlv_path: pathlib.Path, output: str, dtype: type, channels: int) -> None:
    with flashraw.open(mlv_path) as raw:
        f = raw.decode(output)
    assert f.output == output
    a = f.array
    assert a.dtype == dtype and f.dtype == dtype
    assert a.flags.c_contiguous
    if channels is None:
        assert a.shape == (f.height, f.width) and f.channels == 1
        assert f.cfa is not None and f.layout == "bayer"
    else:
        assert a.shape == (f.height, f.width, channels) and f.channels == channels
        assert f.cfa is None and f.layout == "linear"
    assert f.shape == a.shape
    if output == "normalized":
        assert 0.0 <= float(a.min()) and float(a.max()) <= 1.0 + 1e-6
    if output == "rgba8":
        assert (a[..., 3] == 255).all()
    assert f.stats.total_ms > 0
    assert 0 < f.stats.call_ms <= f.stats.wall_ms
    assert f.stats.output_bytes == a.nbytes
    assert f.stats.backend.kind == "cpu"
    assert not f.incomplete and f.warnings == ()


def test_multichannel_raw_output_shape() -> None:
    with flashraw.open(fixture_path("canon-5d3-lossy.dng")) as raw:
        f = raw.decode()
    assert f.channels == 3 and f.array.shape == (f.height, f.width, 3)


def test_raw_hashes_match_expected(fixture_name: str) -> None:
    with flashraw.open(fixture_path(fixture_name)) as raw:
        f = raw.decode()
    exp = EXPECTED[fixture_name]
    assert f.array.dtype == np.uint16
    assert format(flashraw._flashraw.fnv1a64(f.array), "016x") == exp["hash_full"]


def test_frame_fields(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        f = raw.decode()
        m = raw.metadata
    assert f.metadata is m
    assert f.origin == (0, 0)
    assert f.bits_per_sample > 0
    assert isinstance(f.black_level, flashraw.BlackLevel)
    assert f.white_level and f.white_balance is not None
    assert f.active_view().shape == (f.active_area.height, f.active_area.width)
    assert f.crop_view().shape == (f.crop.height, f.crop.width)
    assert np.shares_memory(f.active_view(), f.array)
    assert f.color_at(0, 0) == "R" and f.color_at(1, 0) == "G" and f.color_at(1, 1) == "B"
    assert np.asarray(f) is f.array
    assert np.asarray(f, dtype=np.float32).dtype == np.float32
    assert "raw-u16" in repr(f)
    assert set(f.stats.stages) and all(v >= 0 for v in f.stats.stages.values())


def test_region_and_active_crop(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        full = raw.decode().array
        for region in (flashraw.Rect(10, 20, 64, 32), (10, 20, 64, 32), {"x": 10, "y": 20, "width": 64, "height": 32}):
            f = raw.decode(region=region)
            assert f.array.shape == (32, 64)
            assert f.origin == (10, 20)
            np.testing.assert_array_equal(f.array, full[20:52, 10:74])
        f = raw.decode_region((0, 0, 16, 16), "normalized")
        assert f.array.shape == (16, 16) and f.array.dtype == np.float32
        a = raw.frame_info.active_area
        f = raw.decode(crop="active")
        assert f.array.shape == (a.height, a.width)
        assert f.origin == (a.x, a.y)


def test_tiles() -> None:
    with flashraw.open(fixture_path("bmpcc-tiles.dng")) as raw:
        assert raw.capabilities.random_access.kind == "tiles"
        tiles = raw.tiles
        assert len(tiles) == raw.capabilities.tiles > 1
        full = raw.decode().array
        t = tiles[1]
        f = raw.decode_tile(1)
        assert f.origin == (t.rect.x, t.rect.y)
        np.testing.assert_array_equal(f.array, full[t.rect.slices])


def test_options(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        ref = raw.decode().array
        for backend in flashraw.BACKENDS[:3]:
            np.testing.assert_array_equal(raw.decode(backend=backend).array, ref)
        np.testing.assert_array_equal(raw.decode(threads=1).array, ref)
        assert raw.decode(threads=2).stats.backend.threads == 2
        half = raw.decode("linear-rgb", demosaic="half-size")
        assert half.width <= ref.shape[1] // 2 + 1
        uncropped = raw.decode("rgba8", crop=False)
        assert uncropped.array.ndim == 3
        raw.decode(apply_opcodes=False, corruption_policy="strict")


def test_previews(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        assert len(raw.previews) == 2
        big = raw.preview()
        assert big is not None and big.data is not None
        assert big.info.encoding == "jpeg" and big.info.mime_type == "image/jpeg"
        assert big.data[:2] == b"\xff\xd8"
        assert raw.preview_bytes() == big.data
        assert raw.preview_bytes(big.info) == big.data
        assert raw.preview_bytes(big.info.index) == big.data
        small = raw.thumbnail(decode=True)
        assert small is not None and small.array is not None
        assert small.array.dtype == np.uint8 and small.array.shape == (small.info.height, small.info.width, 3)
        assert raw.preview_bytes("smallest") == raw.preview_bytes(-1 if small.info.index == 1 else 0)
        with pytest.raises(flashraw.NotAvailableError):
            raw.preview_bytes(5)
        with pytest.raises(flashraw.InvalidArgumentError):
            raw.preview_bytes("biggest")
    with flashraw.open(fixture_path("bmpcc-tiles.dng")) as raw:
        assert raw.preview() is None and raw.thumbnail() is None
        with pytest.raises(flashraw.NotAvailableError):
            raw.preview_bytes()


def test_inputs(mlv_path: pathlib.Path) -> None:
    data = mlv_path.read_bytes()
    ref = flashraw.open(mlv_path).decode().array
    sources = {
        "str path": (str(mlv_path), "mapped"),
        "pathlib": (mlv_path, "mapped"),
        "bytes": (data, "borrowed"),
        "memoryview(bytes)": (memoryview(data), "borrowed"),
        "bytearray": (bytearray(data), "copied"),
        "memoryview(bytearray)": (memoryview(bytearray(data)), "copied"),
        "sliced memoryview": (memoryview(b"xx" + data)[2:], "copied"),
        "numpy": (np.frombuffer(data, dtype=np.uint8), "copied"),
        "BytesIO": (io.BytesIO(data), "borrowed"),
    }
    for name, (src, kind) in sources.items():
        with flashraw.open(src) as raw:
            assert raw.source == kind, name
            assert raw.byte_length == len(data), name
            np.testing.assert_array_equal(raw.decode().array, ref, err_msg=name)
    with open(mlv_path, "rb") as fh:
        with flashraw.open(fh) as raw:
            assert raw.source == "borrowed"
            np.testing.assert_array_equal(raw.decode().array, ref)
    with open(mlv_path, "rb") as fh, mmap.mmap(fh.fileno(), 0, access=mmap.ACCESS_READ) as mm:
        with flashraw.open(mm) as raw:
            np.testing.assert_array_equal(raw.decode().array, ref)


def test_detect_and_supports(mlv_path: pathlib.Path) -> None:
    data = mlv_path.read_bytes()
    assert flashraw.detect_format(mlv_path) == "DNG"
    assert flashraw.detect_format(str(mlv_path)) == "DNG"
    assert flashraw.detect_format(data) == "DNG"
    assert flashraw.detect_format(bytearray(data[:4096])) == "DNG"
    assert flashraw.supports(data)
    assert not flashraw.supports(b"not a raw file at all")
    assert flashraw.detect_format(b"") is None


def test_imread(mlv_path: pathlib.Path) -> None:
    a = flashraw.imread(mlv_path)
    assert a.dtype == np.uint8 and a.ndim == 3 and a.shape[2] == 4
    b = flashraw.imread(mlv_path.read_bytes(), "raw-u16", region=(0, 0, 8, 8))
    assert b.shape == (8, 8) and b.dtype == np.uint16


def test_to_pil(mlv_path: pathlib.Path) -> None:
    pil = pytest.importorskip("PIL.Image")
    with flashraw.open(mlv_path) as raw:
        f = raw.decode("rgba8")
        img = f.to_pil()
        assert isinstance(img, pil.Image)
        assert img.size == (f.width, f.height) and img.mode == "RGBA"
        np.testing.assert_array_equal(np.asarray(img), f.array)
        with pytest.raises(flashraw.InvalidArgumentError):
            raw.decode().to_pil()
        p = raw.preview()
        assert p is not None and p.to_pil().size == (p.info.width, p.info.height)


def test_context_manager_closes(mlv_path: pathlib.Path) -> None:
    with flashraw.open(mlv_path) as raw:
        frame = raw.decode()
        assert not raw.closed
    assert raw.closed
    assert "closed" in repr(raw)
    for call in (
        lambda: raw.decode(),
        lambda: raw.preview_bytes(),
        lambda: raw.capabilities,
        lambda: raw.format,
    ):
        with pytest.raises(flashraw.ClosedError):
            call()
    raw.close()  # idempotent
    # Frames outlive the file.
    assert frame.array.sum() > 0
    # Metadata read before closing stays available.
    assert raw.metadata.width > 0


def test_closing_borrowed_input_releases_it(mlv_path: pathlib.Path) -> None:
    import sys

    data = mlv_path.read_bytes()
    base = sys.getrefcount(data)
    raw = flashraw.open(data)
    assert raw.source == "borrowed"
    assert sys.getrefcount(data) == base + 1  # the decoder keeps its input alive
    raw.close()
    assert sys.getrefcount(data) == base


def test_limits(mlv_path: pathlib.Path) -> None:
    with pytest.raises(flashraw.LimitExceededError):
        flashraw.open(mlv_path, max_samples=1000).decode()
    for src in (mlv_path, mlv_path.read_bytes()):
        with pytest.raises(flashraw.LimitExceededError):
            flashraw.open(src, max_dimension=100)


def test_public_names() -> None:
    for name in flashraw.__all__:
        assert hasattr(flashraw, name), name
