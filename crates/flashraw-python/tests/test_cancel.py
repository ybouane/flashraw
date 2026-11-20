"""Cancelling a decode: a CancelToken stops it from another thread with CancelledError."""

from __future__ import annotations

import threading
import time

import numpy as np
import pytest

import flashraw
from conftest import fixture_path


@pytest.fixture(scope="module")
def raw() -> flashraw.RawFile:
    r = flashraw.open(fixture_path("canon-5d3-lossy.dng"))
    yield r
    r.close()


def test_token_state() -> None:
    t = flashraw.CancelToken()
    assert t.cancelled is False
    assert "cancelled=False" in repr(t)
    t.cancel()
    t.cancel()  # idempotent
    assert t.cancelled is True
    assert "cancelled=True" in repr(t)


def test_a_token_nobody_cancels_changes_nothing(raw: flashraw.RawFile) -> None:
    for output, kw in (("raw-u16", {}), ("rgba8", {"demosaic": "ahd"}), ("rgb16", {"threads": 1})):
        plain = raw.decode(output, **kw)
        token = flashraw.CancelToken()
        with_token = raw.decode(output, cancel=token, **kw)
        assert np.array_equal(plain.array, with_token.array), output
        assert not token.cancelled


def test_cancelled_before_the_decode(raw: flashraw.RawFile) -> None:
    token = flashraw.CancelToken()
    token.cancel()
    t0 = time.perf_counter()
    with pytest.raises(flashraw.CancelledError) as e:
        raw.decode("rgba8", cancel=token)
    assert time.perf_counter() - t0 < 0.1
    assert e.value.kind == "Cancelled"
    assert isinstance(e.value, flashraw.FlashRawError)
    assert "Cancelled" in flashraw.ERROR_KINDS
    # Other entry points that take decode options.
    with pytest.raises(flashraw.CancelledError):
        raw.decode_region((0, 0, 64, 64), cancel=token)
    with pytest.raises(flashraw.CancelledError):
        raw.decode_tile(0, cancel=token)
    # The file is still usable, and the next decode with a fresh token works.
    assert raw.decode("rgba8", cancel=flashraw.CancelToken()).array.shape[2] == 4


def test_cancelled_from_another_thread_while_it_runs(raw: flashraw.RawFile) -> None:
    raw.decode("linear-rgb", threads=1)  # warm up
    t0 = time.perf_counter()
    raw.decode("linear-rgb", threads=1)
    full = time.perf_counter() - t0

    token = flashraw.CancelToken()
    timer = threading.Timer(full / 8, token.cancel)
    timer.start()
    t0 = time.perf_counter()
    with pytest.raises(flashraw.CancelledError):
        raw.decode("linear-rgb", threads=1, cancel=token)
    took = time.perf_counter() - t0
    timer.join()
    assert took < full * 0.75, f"cancelled after {took * 1e3:.0f} ms, a full decode takes {full * 1e3:.0f} ms"


def test_one_token_stops_decodes_on_several_threads(raw: flashraw.RawFile) -> None:
    token = flashraw.CancelToken()
    results: list = []

    def work() -> None:
        try:
            while True:
                raw.decode("linear-rgb", threads=1, cancel=token)
        except flashraw.CancelledError as e:
            results.append(e)

    threads = [threading.Thread(target=work) for _ in range(4)]
    for t in threads:
        t.start()
    time.sleep(0.15)
    token.cancel()
    for t in threads:
        t.join(timeout=20)
        assert not t.is_alive()
    assert len(results) == 4
