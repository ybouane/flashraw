"""Exception hierarchy of :mod:`flashraw`.

Every error raised by FlashRaw is a :class:`FlashRawError`; the subclass tells what went
wrong (``kind`` mirrors the error kinds of the Rust library, the CLI and the JS bindings).
A few subclasses also derive from the matching built-in exception, so generic handlers keep
working: :class:`InvalidArgumentError` and :class:`ClosedError` are :class:`ValueError`,
:class:`IoError` is an :class:`OSError` and :class:`OutOfMemoryError` a :class:`MemoryError`.
"""

from __future__ import annotations

from typing import Any, Dict, Optional, Type

from ._types import CorruptInfo

__all__ = [
    "FlashRawError",
    "UnsupportedFormatError",
    "UnsupportedCompressionError",
    "CorruptFileError",
    "InvalidMetadataError",
    "CameraUnsupportedError",
    "GpuUnavailableError",
    "OutOfMemoryError",
    "LimitExceededError",
    "NotAvailableError",
    "InvalidArgumentError",
    "IoError",
    "ClosedError",
    "InternalError",
    "ERROR_KINDS",
]


class FlashRawError(Exception):
    """Base class of every FlashRaw error.

    Attributes:
        kind: Machine-friendly error kind (e.g. ``"CorruptFile"``).
        corrupt: Structured cause of a :class:`CorruptFileError` (which strip/tile is
            truncated and by how much), or ``None``.
    """

    kind: str = "Internal"
    corrupt: Optional[CorruptInfo] = None

    @property
    def message(self) -> str:
        """The error message."""
        return str(self)

    @property
    def truncation(self) -> Optional[CorruptInfo]:
        """:attr:`corrupt` when the cause is truncated sensor data, else ``None``."""
        c = self.corrupt
        return c if c is not None and c.kind == "truncated" else None


class UnsupportedFormatError(FlashRawError):
    """The bytes do not look like any RAW format FlashRaw understands."""

    kind = "UnsupportedFormat"


class UnsupportedCompressionError(FlashRawError):
    """The container was understood but the payload compression is not supported."""

    kind = "UnsupportedCompression"


class CorruptFileError(FlashRawError):
    """The file is structurally invalid (truncated, bad offsets, corrupt entropy data...).

    :attr:`corrupt` describes truncated or undecodable sensor data when the cause is known.
    """

    kind = "CorruptFile"


class InvalidMetadataError(FlashRawError):
    """Metadata required for decoding is missing or nonsensical."""

    kind = "InvalidMetadata"


class CameraUnsupportedError(FlashRawError):
    """The format is supported but this camera or mode is not."""

    kind = "CameraUnsupported"


class GpuUnavailableError(FlashRawError):
    """A GPU backend was requested but is not available."""

    kind = "GpuUnavailable"


class OutOfMemoryError(FlashRawError, MemoryError):
    """An allocation failed or would exceed the configured limits."""

    kind = "OutOfMemory"


class LimitExceededError(FlashRawError):
    """Image dimensions or other values exceed the configured safety limits."""

    kind = "LimitExceeded"


class NotAvailableError(FlashRawError):
    """The requested operation is not possible for this file (e.g. no preview, no tiles)."""

    kind = "NotAvailable"


class InvalidArgumentError(FlashRawError, ValueError):
    """Invalid caller-provided arguments (unknown output, empty region...)."""

    kind = "InvalidArgument"


class IoError(FlashRawError, OSError):
    """Reading the input failed (``errno`` is set when the OS reported one)."""

    kind = "Io"


class ClosedError(FlashRawError, ValueError):
    """The :class:`~flashraw.RawFile` was closed."""

    kind = "Closed"


class InternalError(FlashRawError):
    """An internal error (a bug in FlashRaw); please report it with the input file."""

    kind = "Internal"


_CLASSES: Dict[str, Type[FlashRawError]] = {
    c.kind: c
    for c in (
        UnsupportedFormatError,
        UnsupportedCompressionError,
        CorruptFileError,
        InvalidMetadataError,
        CameraUnsupportedError,
        GpuUnavailableError,
        OutOfMemoryError,
        LimitExceededError,
        NotAvailableError,
        InvalidArgumentError,
        IoError,
        ClosedError,
        InternalError,
    )
}

ERROR_KINDS = tuple(_CLASSES)
"""Every error kind, in declaration order."""


def _make(kind: str, message: str, corrupt: Optional[Dict[str, Any]] = None, errno: Optional[int] = None) -> FlashRawError:
    """Build the exception for an error of ``kind`` (called by the native module)."""
    cls = _CLASSES.get(kind, InternalError)
    if cls is IoError and errno is not None:
        exc: FlashRawError = IoError(errno, message)
    else:
        exc = cls(message)
    if corrupt is not None:
        exc.corrupt = CorruptInfo._from(corrupt)
    return exc
