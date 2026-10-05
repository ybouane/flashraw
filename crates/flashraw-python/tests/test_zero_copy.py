"""No copies between the decoder and NumPy, and none of `bytes` inputs."""

from __future__ import annotations

import gc
import pathlib
import sys

import numpy as np
import pytest

import flashraw


@pytest.mark.parametrize("output", flashraw.OUTPUTS)
def test_array_wraps_the_decoder_buffer(mlv_path: pathlib.Path, output: str) -> None:
    with flashraw.open(mlv_path) as raw:
        f = raw.decode(output)
    a = f.array
    # The array does not own its memory: its base object holds the Rust allocation...
    assert not a.flags.owndata
    assert a.base is not None and not isinstance(a.base, np.ndarray)
    # ...and it points at exactly the buffer the decoder filled (no copy in between).
    assert a.ctypes.data == f._data_address
    assert a.__array_interface__["data"][0] == f._data_address
    # Writable: callers may process in place.
    assert a.flags.writeable
    # Accessing the array again returns the same object (no conversion on access).
    assert f.array is a


def test_array_outlives_frame_and_file(mlv_path: pathlib.Path) -> None:
    raw = flashraw.open(mlv_path)
    f = raw.decode()
    a = f.array
    checksum = int(a.sum(dtype=np.uint64))
    address = a.ctypes.data
    del f
    raw.close()
    del raw
    gc.collect()
    assert a.ctypes.data == address
    assert int(a.sum(dtype=np.uint64)) == checksum


def test_base_keeps_buffer_alive(mlv_path: pathlib.Path) -> None:
    a = flashraw.open(mlv_path).decode().array
    view = a[100:200, 100:200]
    expected = view.copy()
    del a
    gc.collect()
    # The view keeps the owner (and so the Rust buffer) alive.
    np.testing.assert_array_equal(view, expected)
    assert not view.flags.owndata


def test_bytes_input_is_borrowed(mlv_path: pathlib.Path) -> None:
    data = mlv_path.read_bytes()
    address = np.frombuffer(data, dtype=np.uint8).ctypes.data
    with flashraw.open(data) as raw:
        assert raw.source == "borrowed"
        assert raw._native.source_address == address
    view = memoryview(data)
    with flashraw.open(view) as raw:
        assert raw._native.source_address == address


def test_mutable_input_is_copied(mlv_path: pathlib.Path) -> None:
    data = bytearray(mlv_path.read_bytes())
    address = np.frombuffer(data, dtype=np.uint8).ctypes.data
    with flashraw.open(data) as raw:
        assert raw.source == "copied"
        assert raw._native.source_address != address
        ref = raw.decode().array
        # Changing the caller's buffer does not affect the opened file.
        data[:] = bytes(len(data))
        np.testing.assert_array_equal(raw.decode().array, ref)


def test_frame_memory_is_released(mlv_path: pathlib.Path) -> None:
    """Decoding repeatedly does not accumulate memory: arrays free the Rust buffers."""
    resource = pytest.importorskip("resource")
    with flashraw.open(mlv_path) as raw:
        for _ in range(3):
            raw.decode("linear-rgb")
        gc.collect()
        before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        for _ in range(20):
            raw.decode("linear-rgb")  # ~25 MB each
        gc.collect()
        after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # Peak RSS grows by far less than 20 retained frames would need.
    unit = 1 if sys.platform == "darwin" else 1024  # bytes on macOS, KiB on Linux
    assert (after - before) * unit < 200 * 1024 * 1024
