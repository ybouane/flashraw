"""Exceptions, truncated sensor data and malformed input."""

from __future__ import annotations

import errno
import pathlib
import pickle
import random

import numpy as np
import pytest

import flashraw
from conftest import EXPECTED, fixture_path


def test_hierarchy() -> None:
    kinds = {
        "UnsupportedFormat": flashraw.UnsupportedFormatError,
        "UnsupportedCompression": flashraw.UnsupportedCompressionError,
        "CorruptFile": flashraw.CorruptFileError,
        "InvalidMetadata": flashraw.InvalidMetadataError,
        "CameraUnsupported": flashraw.CameraUnsupportedError,
        "GpuUnavailable": flashraw.GpuUnavailableError,
        "OutOfMemory": flashraw.OutOfMemoryError,
        "LimitExceeded": flashraw.LimitExceededError,
        "NotAvailable": flashraw.NotAvailableError,
        "InvalidArgument": flashraw.InvalidArgumentError,
        "Io": flashraw.IoError,
        "Closed": flashraw.ClosedError,
        "Internal": flashraw.InternalError,
    }
    assert set(flashraw.ERROR_KINDS) == set(kinds)
    for kind, cls in kinds.items():
        assert issubclass(cls, flashraw.FlashRawError)
        assert cls.kind == kind
    assert issubclass(flashraw.InvalidArgumentError, ValueError)
    assert issubclass(flashraw.ClosedError, ValueError)
    assert issubclass(flashraw.IoError, OSError)
    assert issubclass(flashraw.OutOfMemoryError, MemoryError)


def test_unsupported_format() -> None:
    with pytest.raises(flashraw.UnsupportedFormatError) as e:
        flashraw.open(b"this is not a raw file" * 10)
    assert e.value.kind == "UnsupportedFormat"
    assert e.value.message == str(e.value)
    assert e.value.corrupt is None and e.value.truncation is None


def test_missing_file(tmp_path: pathlib.Path) -> None:
    with pytest.raises(flashraw.IoError) as e:
        flashraw.open(tmp_path / "missing.dng")
    assert isinstance(e.value, OSError)
    assert e.value.errno == errno.ENOENT
    with pytest.raises(OSError):
        flashraw.read_metadata(str(tmp_path / "missing.dng"))


def test_bad_arguments(mlv_path: pathlib.Path) -> None:
    raw = flashraw.open(mlv_path)
    bad = [
        dict(output="jpeg"),
        dict(output="gpu-rgba8"),
        dict(backend="quantum"),
        dict(demosaic="ahd"),
        dict(corruption_policy="lenient"),
        dict(threads=0),
        dict(crop="maybe"),
        dict(region=(0, 0, 0, 10)),
        dict(region=(1, 2, 3)),
        dict(region=(-1, 0, 4, 4)),
    ]
    for kw in bad:
        with pytest.raises(flashraw.InvalidArgumentError):
            raw.decode(**kw)  # type: ignore[arg-type]
        with pytest.raises(ValueError):
            raw.decode(**kw)  # type: ignore[arg-type]
    with pytest.raises(flashraw.FlashRawError):
        raw.decode(region=(5000, 5000, 10, 10))
    with pytest.raises(flashraw.FlashRawError):
        raw.decode_tile(10_000)
    with pytest.raises(flashraw.InvalidArgumentError):
        flashraw.open(12345)  # type: ignore[arg-type]
    with pytest.raises(flashraw.InvalidArgumentError):
        flashraw.open(object())  # type: ignore[arg-type]


def test_gpu_backend_without_gpu(mlv_path: pathlib.Path) -> None:
    if flashraw.gpu_available():
        pytest.skip("a GPU backend is available")
    raw = flashraw.open(mlv_path)
    try:
        f = raw.decode(backend="gpu")
    except flashraw.GpuUnavailableError:
        return
    # Some builds fall back to the CPU for raw outputs; the result must still be correct.
    np.testing.assert_array_equal(f.array, raw.decode().array)


# --- truncated sensor data ------------------------------------------------------------------


def _cut(chdk_bytes: bytes) -> bytes:
    return chdk_bytes[: len(chdk_bytes) - 1000]  # 1000 bytes = 800 10-bit samples


def test_complete_file_has_no_warnings(chdk_bytes: bytes) -> None:
    for policy in ("recover", "strict"):
        f = flashraw.open(chdk_bytes).decode(corruption_policy=policy)
        assert not f.incomplete and f.warnings == ()


def test_recover_reports_truncation(chdk_bytes: bytes) -> None:
    cut = _cut(chdk_bytes)
    f = flashraw.open(cut).decode(apply_opcodes=False)
    assert f.incomplete
    assert len(f.warnings) == 1
    w = f.warnings[0]
    assert isinstance(w, flashraw.DecodeWarning) and isinstance(w, flashraw.CorruptInfo)
    assert (w.kind, w.unit, w.index) == ("truncated", "strip", 0)
    assert w.missing == 1000 and w.expected - w.available == 1000  # type: ignore[operator]
    assert w.file_length == len(cut)
    assert w.samples_filled == 800
    assert w.message.startswith("recovered truncated strip 0:")
    assert (f.array.reshape(-1)[-800:] == 0).all()
    ref = flashraw.open(chdk_bytes).decode(apply_opcodes=False).array
    np.testing.assert_array_equal(f.array.reshape(-1)[:1000], ref.reshape(-1)[:1000])


def test_strict_raises_structured_truncation(chdk_bytes: bytes) -> None:
    cut = _cut(chdk_bytes)
    with pytest.raises(flashraw.CorruptFileError) as e:
        flashraw.open(cut).decode(corruption_policy="strict")
    err = e.value
    assert err.kind == "CorruptFile"
    assert "truncated image data: strip 0 expected" in str(err)
    t = err.truncation
    assert t is not None and t is err.corrupt
    assert (t.kind, t.unit, t.index) == ("truncated", "strip", 0)
    assert t.missing == 1000
    assert t.file_length == len(cut)
    assert t.offset is not None and t.offset + t.available == len(cut)  # type: ignore[operator]


# --- undecodable sensor data: preview fallback ---------------------------------------------


def test_preview_fallback() -> None:
    # Synthetic Nikon High Efficiency NEF (JPEG XS payload, 64x48 RGB preview in IFD0).
    raw = flashraw.open(fixture_path("nikon-he-synthetic.nef"))
    with pytest.raises(flashraw.UnsupportedCompressionError):
        raw.decode("rgba8")
    f = raw.decode("rgba8", preview_fallback=True)
    assert f.source == "preview"
    assert f.incomplete and len(f.warnings) == 1
    assert f.warnings[0].kind == "preview-fallback"
    assert "embedded full-size preview" in f.warnings[0].message
    assert f.array.shape == (48, 64, 4)
    assert tuple(f.array[0, 0]) == (0, 100, 200, 255)
    with pytest.raises(flashraw.UnsupportedCompressionError):
        raw.decode("raw-u16", preview_fallback=True)
    lin = raw.decode("linear-rgb", preview_fallback=True)
    assert lin.source == "preview" and lin.array.shape == (48, 64, 3)


def test_decodable_file_reports_sensor_source(chdk_bytes: bytes) -> None:
    f = flashraw.open(chdk_bytes).decode("rgba8", demosaic="half-size", preview_fallback=True)
    assert f.source == "sensor" and f.warnings == ()


def test_metadata_and_regions_do_not_need_missing_data(chdk_bytes: bytes) -> None:
    cut = _cut(chdk_bytes)
    assert flashraw.read_metadata(cut).width > 0
    f = flashraw.open(cut).decode(region=(0, 0, 64, 64), corruption_policy="strict")
    assert f.warnings == ()


def test_errors_pickle() -> None:
    try:
        flashraw.open(b"garbage" * 20)
    except flashraw.FlashRawError as e:
        e2 = pickle.loads(pickle.dumps(e))
        assert type(e2) is type(e) and str(e2) == str(e)


# --- malformed input never crashes ----------------------------------------------------------

# Capitalised words an error message may start with (acronyms and identifiers with another
# capital or a digit, such as NEF, CR2 or FlashRaw, are accepted without being listed).
_PROPER_NOUNS = {"Canon", "Fuji", "Fujifilm", "Hasselblad", "Huffman", "Kodak", "Leaf", "Light", "Nikon"}
_PROPER_NOUNS |= {"Olympus", "Panasonic", "Pentax", "Phase", "Pillow", "Samsung", "Sigma", "Sinar", "Sony"}


def _check_style(message: str) -> None:
    """Error messages start in lower case (bar proper nouns), with no trailing period or space."""
    assert message and message == message.strip(), repr(message)
    assert not message.endswith(".") or message.endswith("..."), repr(message)
    word = message.split(" ")[0].rstrip(":,;")
    if word[:1].isupper() and not any(c.isupper() or c.isdigit() for c in word[1:]):
        assert word in _PROPER_NOUNS, repr(message)


def test_message_style(mlv_path: pathlib.Path) -> None:
    _check_style("corrupt file: truncated header")
    _check_style("NEF has no raw image IFD")
    for bad in ("Corrupt file", "corrupt file.", " corrupt", "corrupt "):
        with pytest.raises(AssertionError):
            _check_style(bad)
    with pytest.raises(flashraw.InvalidArgumentError) as e:
        flashraw.open(mlv_path).decode("nope")
    _check_style(str(e.value))


def _try_all(data: bytes) -> None:
    """Open, read metadata, decode raw and RGB: every failure must be a FlashRawError
    whose message follows the style."""
    try:
        raw = flashraw.open(data, max_samples=50_000_000)
    except flashraw.FlashRawError as e:
        _check_style(str(e))
        return
    with raw:
        try:
            raw.metadata
            raw.frame_info
            raw.capabilities
            raw.tiles
        except flashraw.FlashRawError as e:
            _check_style(str(e))
            return
        for output in ("raw-u16", "rgba8"):
            for policy in ("recover", "strict"):
                try:
                    f = raw.decode(output, corruption_policy=policy)
                    assert f.array.size == f.width * f.height * f.channels
                    for w in f.warnings:
                        _check_style(w.message)
                except flashraw.FlashRawError as e:
                    _check_style(str(e))
        for p in raw.previews:
            try:
                raw.preview_bytes(p)
                raw.preview(p, decode=True)
            except flashraw.FlashRawError as e:
                _check_style(str(e))


@pytest.mark.parametrize("name", sorted(EXPECTED))
def test_truncated_at_many_points(name: str) -> None:
    data = fixture_path(name).read_bytes()
    n = len(data)
    cuts = sorted({0, 1, 8, 16, 64, 256, 1024, 4096, n // 3, n // 2, n - 4096, n - 1} | set(range(0, 4096, 397)))
    for c in cuts:
        _try_all(data[: max(c, 0)])


@pytest.mark.parametrize("name", sorted(EXPECTED))
def test_random_corruption(name: str) -> None:
    data = bytearray(fixture_path(name).read_bytes())
    rng = random.Random(1234)
    header = min(len(data), 64 * 1024)
    for _ in range(12):
        d = bytearray(data)
        for _ in range(rng.randint(1, 32)):
            i = rng.randrange(header)  # mostly the TIFF structure, where damage matters most
            d[i] = rng.randrange(256)
        _try_all(bytes(d))


def test_small_garbage_inputs() -> None:
    rng = random.Random(42)
    prefixes = [b"II*\x00", b"MM\x00*", b"FUJIFILMCCD-RAW ", b"\x00\x00\x00\x18ftypcrx ", b"IIRO", b"\x00MRM"]
    for p in prefixes:
        for size in (0, 8, 64, 512, 4096):
            _try_all(p + bytes(rng.randrange(256) for _ in range(size)))
