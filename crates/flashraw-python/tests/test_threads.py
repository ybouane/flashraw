"""Decoding releases the GIL: Python threads keep running and decodes run in parallel."""

from __future__ import annotations

import os
import threading
import time
from concurrent.futures import ThreadPoolExecutor

import numpy as np
import pytest

import flashraw
from conftest import fixture_path


def test_other_threads_run_during_decode() -> None:
    """A pure-Python thread keeps running while the main thread is inside one decode() call.

    If the GIL were held for the call, the spinner could only run at its boundaries (at most
    one switch interval, 5 ms, out of a ~250 ms single-threaded decode).
    """
    raw = flashraw.open(fixture_path("canon-5d3-lossy.dng"))
    raw.decode("linear-rgb", threads=1)  # warm up
    ticks = 0
    stop = threading.Event()

    def spin() -> None:
        nonlocal ticks
        while not stop.is_set():
            ticks += 1

    t = threading.Thread(target=spin)
    t.start()
    try:
        time.sleep(0.05)
        t0, n0 = time.perf_counter(), ticks
        time.sleep(0.2)  # the spinner alone (sleep releases the GIL)
        rate = (ticks - n0) / (time.perf_counter() - t0)
        t0, n0 = time.perf_counter(), ticks
        raw.decode("linear-rgb", threads=1)
        elapsed, during = time.perf_counter() - t0, ticks - n0
    finally:
        stop.set()
        t.join()
    assert during > 0.3 * rate * elapsed, (during, rate, elapsed)


@pytest.mark.skipif((os.cpu_count() or 1) < 4, reason="needs at least 4 cores")
def test_parallel_decodes_are_faster_than_serial() -> None:
    data = fixture_path("canon-5d3-lossy.dng").read_bytes()
    raw = flashraw.open(data)
    ref = raw.decode("raw-u16", threads=1).array
    jobs = 8

    def one(_: int) -> np.ndarray:
        # One worker thread per decode: the parallelism comes from the Python threads.
        return raw.decode("raw-u16", threads=1).array

    def timed(fn: object) -> float:
        best = float("inf")
        for _ in range(3):
            t0 = time.perf_counter()
            fn()  # type: ignore[operator]
            best = min(best, time.perf_counter() - t0)
        return best

    serial = timed(lambda: [one(i) for i in range(jobs)])
    with ThreadPoolExecutor(max_workers=4) as pool:
        parallel = timed(lambda: list(pool.map(one, range(jobs))))
        results = list(pool.map(one, range(jobs)))
    for r in results:
        np.testing.assert_array_equal(r, ref)
    # 4 threads on >= 4 cores: expect well under the serial time (ideal 0.25).
    assert parallel < serial * 0.7, (serial, parallel)


def test_concurrent_decodes_of_many_files(fixture_name: str) -> None:
    path = fixture_path(fixture_name)
    ref = flashraw.open(path).decode().array
    with ThreadPoolExecutor(max_workers=4) as pool:
        outs = list(pool.map(lambda _: flashraw.imread(path, "raw-u16"), range(8)))
    for o in outs:
        np.testing.assert_array_equal(o, ref)


def test_close_waits_for_running_decodes() -> None:
    raw = flashraw.open(fixture_path("canon-5d3-lossy.dng"))
    errors: list = []
    results: list = []

    def work() -> None:
        try:
            for _ in range(5):
                results.append(raw.decode("linear-rgb").array.shape)
        except flashraw.ClosedError as e:
            errors.append(e)

    threads = [threading.Thread(target=work) for _ in range(4)]
    for t in threads:
        t.start()
    time.sleep(0.02)
    raw.close()
    for t in threads:
        t.join()
    # Every decode either completed or saw a clean ClosedError; nothing crashed.
    assert len(results) + len(errors) >= 4
    assert raw.closed
