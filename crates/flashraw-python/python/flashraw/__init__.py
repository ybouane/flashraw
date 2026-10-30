"""FlashRaw: fast, safe camera RAW decoding with zero-copy NumPy output.

>>> import flashraw
>>> with flashraw.open("photo.NEF") as raw:
...     print(raw.metadata.camera.name, raw.metadata.exposure.iso)
...     frame = raw.decode()                    # sensor data, uint16 (h, w)
...     rgba = raw.decode(output="rgba8").array # display-ready uint8 (h, w, 4)

Decoding releases the GIL (decode many files in parallel from Python threads) and uses
FlashRaw's own thread pool for each file. Pixel arrays are handed over without a copy: the
array's base object owns the decoder's buffer.
"""

from __future__ import annotations

import builtins
import os
import time
from typing import TYPE_CHECKING, Any, Dict, List, Optional, Tuple, Union

# Imported up front so the native module's first array creation does not pay for it.
import numpy as np
import numpy.typing as npt

from . import _flashraw
from ._errors import (
    ERROR_KINDS,
    CameraUnsupportedError,
    ClosedError,
    CorruptFileError,
    FlashRawError,
    GpuUnavailableError,
    InternalError,
    InvalidArgumentError,
    InvalidMetadataError,
    IoError,
    LimitExceededError,
    NotAvailableError,
    OutOfMemoryError,
    UnsupportedCompressionError,
    UnsupportedFormatError,
)
from ._types import (
    BackendReport,
    BlackLevel,
    Camera,
    Capabilities,
    CfaPattern,
    ColorMatrices,
    ColorMatrix,
    CorruptInfo,
    DecodeStats,
    DecodeWarning,
    DngInfo,
    DngProfile,
    Exposure,
    FrameInfo,
    Gps,
    Lens,
    Metadata,
    Opcode,
    OrientationInfo,
    PreviewInfo,
    RandomAccess,
    Rect,
    TileInfo,
)

if TYPE_CHECKING:
    from PIL import Image

__version__: str = _flashraw.__version__

__all__ = [
    "__version__",
    "open",
    "RawFile",
    "Frame",
    "Preview",
    "read_metadata",
    "supports",
    "detect_format",
    "imread",
    "gpu_available",
    "build_info",
    "icc_supported",
    "icc_profile",
    "OUTPUTS",
    "COLOR_SPACES",
    "BACKENDS",
    # metadata types
    "Rect",
    "CfaPattern",
    "BlackLevel",
    "ColorMatrix",
    "ColorMatrices",
    "OrientationInfo",
    "PreviewInfo",
    "Camera",
    "Lens",
    "Exposure",
    "Gps",
    "DngProfile",
    "DngInfo",
    "Opcode",
    "Metadata",
    "FrameInfo",
    "RandomAccess",
    "Capabilities",
    "TileInfo",
    "CorruptInfo",
    "DecodeWarning",
    "BackendReport",
    "DecodeStats",
    # errors
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

#: Colour spaces accepted by ``color_space=``.
COLOR_SPACES: Tuple[str, ...] = ("srgb", "display-p3", "adobe-rgb", "prophoto", "rec2020")

OUTPUTS: Tuple[str, ...] = (
    "raw-u16",
    "raw-f32",
    "normalized",
    "linear-rgb",
    "linear-rgb16",
    "linear-rgb-f16",
    "rgba8",
    "rgb16",
)
"""Output formats accepted by :meth:`RawFile.decode`."""

BACKENDS: Tuple[str, ...] = ("auto", "cpu", "simd", "gpu")
"""Backends accepted by :meth:`RawFile.decode`."""

#: Anything :func:`open` accepts: a path, a bytes-like object or a binary file object.
Source = Union[str, "os.PathLike[str]", bytes, bytearray, memoryview, Any]
#: A rectangle: :class:`Rect`, ``(x, y, width, height)`` or ``{"x", "y", "width", "height"}``.
RectLike = Union[Rect, Tuple[int, int, int, int], Dict[str, int]]
#: A preview selector: ``"largest"``, ``"smallest"``, an index or a :class:`PreviewInfo`.
PreviewSelector = Union[str, int, PreviewInfo, None]


def _rect_tuple(r: Optional[RectLike]) -> Optional[Tuple[int, int, int, int]]:
    if r is None:
        return None
    if isinstance(r, dict):
        return (int(r["x"]), int(r["y"]), int(r["width"]), int(r["height"]))
    try:
        x, y, w, h = r
    except (TypeError, ValueError):
        raise InvalidArgumentError("`region` must be a Rect, (x, y, width, height) or a dict") from None
    if min(x, y, w, h) < 0:
        raise InvalidArgumentError(f"region {tuple(r)} has negative values")
    return (int(x), int(y), int(w), int(h))


def _selector(which: PreviewSelector) -> Union[str, int, None]:
    if isinstance(which, PreviewInfo):
        return which.index
    return which


class Frame:
    """A decoded frame: the pixels as a NumPy array plus everything needed to interpret them.

    ``array`` has shape ``(height, width)`` for one sample per pixel (CFA / monochrome raw
    data) or ``(height, width, channels)`` (3 for the RGB outputs, 4 for RGBA8, 3 for linear
    raw DNG / Foveon data), with dtype ``uint16`` (``raw-u16``, ``rgb16``, ``linear-rgb16``),
    ``float16`` (``linear-rgb-f16``), ``float32`` (``raw-f32``, ``normalized``,
    ``linear-rgb``) or ``uint8`` (``rgba8``). The array owns the decoder's buffer (no copy was
    made); it stays valid after the :class:`RawFile` is closed.

    Rectangles (``active_area``, ``crop``, ``masked_areas``) are in frame coordinates;
    ``origin`` is the frame's position in the sensor buffer (non-zero for region and tile
    decodes, and for ``crop="active"``). The CFA pattern is anchored at sensor-buffer
    ``(0, 0)``: use :meth:`color_at` for frame coordinates.
    """

    __slots__ = (
        "array",
        "output",
        "width",
        "height",
        "channels",
        "bits_per_sample",
        "cfa",
        "layout",
        "active_area",
        "crop",
        "origin",
        "masked_areas",
        "black_level",
        "white_level",
        "white_balance",
        "white_balance_applied",
        "color_matrices",
        "orientation",
        "stats",
        "warnings",
        "source",
        "color_space",
        "icc_profile",
        "metadata",
        "_data_address",
    )

    array: "npt.NDArray[Any]"
    output: str
    width: int
    height: int
    channels: int
    bits_per_sample: int
    cfa: Optional[CfaPattern]
    layout: str
    active_area: Rect
    crop: Rect
    origin: Tuple[int, int]
    masked_areas: Tuple[Rect, ...]
    black_level: BlackLevel
    white_level: Tuple[int, ...]
    white_balance: Optional[Tuple[float, ...]]
    #: True when the camera already applied ``white_balance`` to the samples (see
    #: :attr:`Metadata.white_balance_applied`).
    white_balance_applied: bool
    color_matrices: ColorMatrices
    orientation: int
    stats: DecodeStats
    warnings: Tuple[DecodeWarning, ...]
    #: ``"sensor"``, or ``"preview"`` when ``preview_fallback`` returned the embedded preview.
    source: str
    #: Colour space of the RGB outputs: the requested ``color_space`` (``"srgb"``,
    #: ``"display-p3"``, ...), ``"custom"`` after an ``output_profile``, ``"srgb"`` for raw outputs.
    color_space: str
    #: ICC profile (bytes) describing the RGB samples, for tagging files: a display-curve
    #: profile for ``"rgba8"`` / ``"rgb16"``, a linear one for the linear outputs, or the
    #: ``output_profile`` that was used. ``None`` for raw outputs and preview frames.
    icc_profile: Optional[bytes]
    metadata: Metadata

    def __init__(self, array: "npt.NDArray[Any]", info: Dict[str, Any], metadata: Metadata, wall_ms: float = 0.0) -> None:
        self.array = array
        self.output = info["output"]
        self.width = info["width"]
        self.height = info["height"]
        self.channels = info["channels"]
        self.bits_per_sample = info["bits_per_sample"]
        self.cfa = CfaPattern._from(info["cfa"])
        self.layout = info["layout"]
        self.active_area = Rect._from(info["active_area"])
        self.crop = Rect._from(info["crop"])
        self.origin = tuple(info["origin"])
        self.masked_areas = tuple(Rect._from(r) for r in info["masked_areas"])
        self.black_level = BlackLevel._from(info["black_level"])
        self.white_level = tuple(info["white_level"])
        wb = info["white_balance"]
        self.white_balance = tuple(wb) if wb is not None else None
        self.white_balance_applied = bool(info.get("white_balance_applied", False))
        self.color_matrices = ColorMatrices._from(info["color_matrices"])
        self.orientation = info["orientation"]
        self.stats = DecodeStats._from(info["stats"], wall_ms)
        self.warnings = tuple(DecodeWarning._from(w) for w in info["warnings"])
        self.source = info.get("source", "sensor")
        self.color_space = info.get("color_space", "srgb")
        self.icc_profile = info.get("icc_profile")
        self.metadata = metadata
        self._data_address = info.get("data_address", 0)

    @property
    def incomplete(self) -> bool:
        """True when some samples hold no decoded data (corrupt or truncated input repaired
        under ``corruption_policy="recover"``; see :attr:`warnings`)."""
        return bool(self.warnings)

    @property
    def dtype(self) -> "np.dtype[Any]":
        """The array's dtype."""
        return self.array.dtype

    @property
    def shape(self) -> Tuple[int, ...]:
        """The array's shape."""
        return self.array.shape

    def active_view(self) -> "npt.NDArray[Any]":
        """A view (no copy) of the array restricted to :attr:`active_area`."""
        return self.array[self.active_area.slices]

    def crop_view(self) -> "npt.NDArray[Any]":
        """A view (no copy) of the array restricted to the recommended :attr:`crop`."""
        return self.array[self.crop.slices]

    def color_at(self, x: int, y: int) -> Optional[str]:
        """CFA colour letter at frame coordinates ``(x, y)`` (``None`` without a CFA)."""
        if self.cfa is None:
            return None
        return self.cfa.color_at(x + self.origin[0], y + self.origin[1])

    def to_pil(self) -> "Image.Image":
        """The frame as a Pillow image (``rgba8`` output only; requires Pillow).

        The image is built from the array's memory; the frame's orientation is already
        applied for ``rgba8``.
        """
        if self.output != "rgba8":
            raise InvalidArgumentError(f'`to_pil()` needs output="rgba8" (this frame is "{self.output}")')
        try:
            from PIL import Image
        except ImportError as e:  # pragma: no cover - depends on the environment
            raise ImportError("`to_pil()` requires Pillow (pip install pillow)") from e
        return Image.frombuffer("RGBA", (self.width, self.height), self.array, "raw", "RGBA", 0, 1)

    def __array__(self, dtype: Any = None, copy: Optional[bool] = None) -> "npt.NDArray[Any]":
        a = self.array
        if dtype is not None and a.dtype != dtype:
            if copy is False:
                raise ValueError("a dtype conversion needs a copy")
            return a.astype(dtype)
        return a.copy() if copy else a

    def __repr__(self) -> str:
        return (
            f"<flashraw.Frame {self.output} {self.width}x{self.height}x{self.channels} "
            f"{self.array.dtype} in {self.stats.total_ms:.1f} ms"
            f"{' (incomplete)' if self.warnings else ''}>"
        )


class Preview:
    """An embedded preview: its description (:attr:`info`) and either its encoded bytes
    (:attr:`data`, usually JPEG) or decoded RGB pixels (:attr:`array`, ``(h, w, 3)`` uint8,
    orientation not applied)."""

    __slots__ = ("info", "data", "array")

    def __init__(self, info: PreviewInfo, data: Optional[bytes] = None, array: Optional["npt.NDArray[Any]"] = None) -> None:
        self.info = info
        self.data = data
        self.array = array

    def to_pil(self) -> "Image.Image":
        """Open the preview with Pillow (requires Pillow)."""
        from PIL import Image

        if self.array is not None:
            return Image.fromarray(self.array)
        import io

        return Image.open(io.BytesIO(self.data or b""))

    def __repr__(self) -> str:
        what = f"{len(self.data)} bytes" if self.data is not None else "decoded"
        return f"<flashraw.Preview {self.info.kind} {self.info.encoding} {self.info.width}x{self.info.height} {what}>"


class RawFile:
    """An opened RAW file. Use :func:`open` to create one; close it (or use ``with``) to
    release the input early.

    Metadata and previews are available immediately; the sensor data is decoded on demand
    with :meth:`decode`. A :class:`RawFile` can be shared between threads: concurrent
    decodes of the same file run in parallel.
    """

    __slots__ = ("_native", "_metadata", "_frame_info", "__weakref__")

    def __init__(
        self,
        source: Source,
        *,
        max_samples: Optional[int] = None,
        max_dimension: Optional[int] = None,
    ) -> None:
        self._native = _flashraw._RawFile(source, max_samples, max_dimension)
        self._metadata: Optional[Metadata] = None
        self._frame_info: Optional[FrameInfo] = None

    # -- lifecycle ------------------------------------------------------------------------

    def close(self) -> None:
        """Release the input and the decoder (waits for running decodes; idempotent).
        Frames and previews already returned stay valid."""
        self._native.close()

    @property
    def closed(self) -> bool:
        """True after :meth:`close`."""
        return bool(self._native.closed)

    def __enter__(self) -> "RawFile":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    # -- description ----------------------------------------------------------------------

    @property
    def metadata(self) -> Metadata:
        """Normalised metadata (camera, lens, exposure, orientation, CFA, levels, white
        balance, colour matrices, previews, DNG extras)."""
        if self._metadata is None:
            self._metadata = Metadata._from(self._native.metadata())
        return self._metadata

    @property
    def frame_info(self) -> FrameInfo:
        """Geometry and calibration of the sensor buffer (known before decoding)."""
        if self._frame_info is None:
            self._frame_info = FrameInfo._from(self._native.frame_info())
        return self._frame_info

    @property
    def capabilities(self) -> Capabilities:
        """What the file supports (random access, tiles, previews, float data)."""
        return Capabilities._from(self._native.capabilities())

    @property
    def tiles(self) -> List[TileInfo]:
        """Independently decodable tiles/strips (see :meth:`decode_tile`)."""
        return [TileInfo._from(t) for t in self._native.tiles()]

    @property
    def format(self) -> str:
        """Container format name (e.g. ``"NEF"``, ``"DNG"``)."""
        return str(self._native.format)

    @property
    def byte_length(self) -> int:
        """Size of the input in bytes."""
        return int(self._native.byte_length)

    @property
    def parse_ms(self) -> float:
        """Milliseconds spent parsing the container in :func:`open`."""
        return float(self._native.parse_ms)

    @property
    def source(self) -> str:
        """How the input is held: ``"borrowed"`` (zero-copy view of a ``bytes`` object),
        ``"mapped"`` (memory-mapped path) or ``"copied"`` (a mutable buffer copied once)."""
        return str(self._native.source)

    # -- previews -------------------------------------------------------------------------

    @property
    def previews(self) -> Tuple[PreviewInfo, ...]:
        """All embedded previews and thumbnails."""
        return self.metadata.previews

    def preview_bytes(self, which: PreviewSelector = "largest") -> bytes:
        """The encoded bytes (usually JPEG) of an embedded preview: ``"largest"`` (default),
        ``"smallest"``, an index into :attr:`previews` or a :class:`PreviewInfo`.

        Raises :class:`NotAvailableError` when the file has no such preview.
        """
        data, _ = self._native.preview_bytes(_selector(which))
        return data

    def preview(self, which: PreviewSelector = "largest", *, decode: bool = False) -> Optional[Preview]:
        """An embedded preview with its description, or ``None`` when the file has none.

        With ``decode=True`` the preview is decoded to an ``(h, w, 3)`` uint8 array (JPEG
        and uncompressed RGB previews; orientation not applied, see ``info.orientation``).
        """
        if not self.previews:
            return None
        sel = _selector(which)
        if decode:
            array, info = self._native.decode_preview(sel)
            return Preview(PreviewInfo._from(info), array=array)
        data, info = self._native.preview_bytes(sel)
        return Preview(PreviewInfo._from(info), data=data)

    def thumbnail(self, *, decode: bool = False) -> Optional[Preview]:
        """The smallest embedded preview (see :meth:`preview`)."""
        return self.preview("smallest", decode=decode)

    # -- decoding -------------------------------------------------------------------------

    def decode(
        self,
        output: str = "raw-u16",
        *,
        backend: str = "auto",
        threads: Optional[int] = None,
        region: Optional[RectLike] = None,
        crop: Union[bool, str] = True,
        demosaic: str = "bilinear",
        apply_opcodes: bool = True,
        corruption_policy: str = "recover",
        exposure: float = 0.0,
        black_point: float = 0.0,
        white_point: float = 1.0,
        preview_fallback: bool = False,
        color_space: str = "srgb",
        output_profile: Union[bytes, "os.PathLike[str]", str, None] = None,
        intent: str = "perceptual",
        black_point_compensation: bool = False,
    ) -> Frame:
        """Decode the sensor data. The GIL is released while decoding.

        Args:
            output: ``"raw-u16"`` (sensor samples as stored, uint16), ``"raw-f32"``
                (as float32), ``"normalized"`` (black-subtracted, white-normalised float32
                in [0, 1], CFA layout kept), ``"linear-rgb"`` (demosaiced, white-balanced
                linear sRGB primaries, float32 ``(h, w, 3)``), ``"linear-rgb-f16"`` (the same
                as float16, half the memory), ``"linear-rgb16"`` (the same clipped to [0, 1] as
                uint16), ``"rgba8"`` (display-ready sRGB with orientation applied, uint8
                ``(h, w, 4)``) or ``"rgb16"`` (display-ready sRGB with orientation applied,
                uint16 ``(h, w, 3)``). Linear outputs keep the sensor orientation
                (:attr:`Frame.orientation`).
            backend: ``"auto"``, ``"cpu"`` (scalar), ``"simd"`` or ``"gpu"``.
            threads: worker threads for this decode (``None``: all cores; ``1``:
                single-threaded).
            region: decode only this rectangle (sensor-buffer coordinates).
            crop: RGB outputs: crop to the recommended crop (default ``True``). Raw outputs
                are never cropped by ``True``; ``"active"`` crops them to the active area.
            demosaic: ``"bilinear"`` or ``"half-size"`` (2x2 binning) for RGB outputs.
            apply_opcodes: apply DNG opcode lists (bad pixels, gain maps...).
            corruption_policy: ``"recover"`` (default; zero-fill what is missing and report it
                in :attr:`Frame.warnings`) or ``"strict"`` (raise :class:`CorruptFileError`).
            exposure: RGB outputs: exposure in EV, applied to linear values after the colour
                conversion (a convenience control; no tone curve or highlight recovery).
            black_point: RGB outputs: linear value (after exposure) mapped to black.
            white_point: RGB outputs: linear value (after exposure) mapped to white. Integer
                outputs clip outside [black_point, white_point]; float outputs keep the values.
            preview_fallback: RGB outputs (``"rgba8"``, ``"linear-rgb"``) of files whose
                sensor data cannot be decoded (:class:`UnsupportedCompressionError`, e.g.
                a compression mode FlashRaw does not implement): return the embedded full-size preview instead of
                raising. The frame then has ``source == "preview"`` and a
                ``"preview-fallback"`` warning. Never applies to raw outputs or regions.
            color_space: RGB outputs: ``"srgb"`` (default), ``"display-p3"``, ``"adobe-rgb"``,
                ``"prophoto"`` or ``"rec2020"``. The linear outputs get the primaries, the
                display outputs (``"rgba8"``, ``"rgb16"``) also the space's transfer curve;
                :attr:`Frame.icc_profile` is the matching ICC profile.
            output_profile: RGB outputs: convert to this ICC profile (the bytes, or a path to
                a ``.icc`` file) instead of ``color_space``. Needs :func:`icc_supported`.
                Matrix/TRC profiles cost nothing extra; profiles with lookup tables take a
                second, slower pass (and only the display outputs). Invalid profiles raise
                :class:`InvalidArgumentError`.
            intent: rendering intent for ``output_profile``: ``"perceptual"`` (default),
                ``"relative"``, ``"saturation"`` or ``"absolute"``.
            black_point_compensation: black point compensation for ``output_profile``.
        """
        t0 = time.perf_counter()
        array, info = self._native.decode(
            output,
            backend,
            threads,
            _rect_tuple(region),
            crop,
            demosaic,
            apply_opcodes,
            corruption_policy,
            None,
            exposure,
            black_point,
            white_point,
            preview_fallback,
            color_space,
            _profile_bytes(output_profile),
            intent,
            black_point_compensation,
        )
        return Frame(array, info, self.metadata, (time.perf_counter() - t0) * 1e3)

    def decode_region(self, region: RectLike, output: str = "raw-u16", **options: Any) -> Frame:
        """Decode only ``region`` (sensor-buffer coordinates); see :meth:`decode`."""
        return self.decode(output, region=region, **options)

    def decode_tile(
        self,
        index: int,
        output: str = "raw-u16",
        *,
        backend: str = "auto",
        threads: Optional[int] = None,
        demosaic: str = "bilinear",
        apply_opcodes: bool = True,
        corruption_policy: str = "recover",
        exposure: float = 0.0,
        black_point: float = 0.0,
        white_point: float = 1.0,
        color_space: str = "srgb",
        output_profile: Union[bytes, "os.PathLike[str]", str, None] = None,
        intent: str = "perceptual",
        black_point_compensation: bool = False,
    ) -> Frame:
        """Decode one independently decodable tile/strip (see :attr:`tiles`)."""
        t0 = time.perf_counter()
        array, info = self._native.decode(
            output,
            backend,
            threads,
            None,
            True,
            demosaic,
            apply_opcodes,
            corruption_policy,
            int(index),
            exposure,
            black_point,
            white_point,
            False,
            color_space,
            _profile_bytes(output_profile),
            intent,
            black_point_compensation,
        )
        return Frame(array, info, self.metadata, (time.perf_counter() - t0) * 1e3)

    def __repr__(self) -> str:
        if self.closed:
            return "<flashraw.RawFile (closed)>"
        m = self.metadata
        return f"<flashraw.RawFile {m.format} {m.camera.name} {m.width}x{m.height}>"


def _profile_bytes(profile: Union[bytes, "os.PathLike[str]", str, None]) -> Optional[bytes]:
    """The bytes of an ICC profile given as bytes-like or as a file path."""
    if profile is None:
        return None
    if isinstance(profile, (str, os.PathLike)):
        with builtins.open(profile, "rb") as f:
            return f.read()
    return bytes(profile)


def icc_supported() -> bool:
    """True when this build converts to arbitrary ICC profiles (``output_profile=``). The
    built-in colour spaces and their profiles are always available."""
    return bool(_flashraw.icc_supported())


def icc_profile(color_space: str = "srgb", *, linear: bool = False) -> bytes:
    """The standard ICC profile (bytes) of a built-in colour space; ``linear=True`` gives
    the variant with a linear transfer curve, for the linear outputs."""
    return bytes(_flashraw.icc_profile(color_space, linear))


def open(  # noqa: A001 - mirrors the built-in for files
    source: Source,
    *,
    max_samples: Optional[int] = None,
    max_dimension: Optional[int] = None,
) -> RawFile:
    """Open a RAW file.

    ``source`` is a path (``str`` / :class:`os.PathLike`; memory-mapped), a bytes-like
    object (``bytes`` and whole-``bytes`` memoryviews are used without a copy; mutable
    buffers such as ``bytearray`` are copied once) or a binary file object (its ``read()``
    result is used without a further copy).

    ``max_samples`` / ``max_dimension`` tighten the safety limits for untrusted input.
    Raises a :class:`FlashRawError` subclass (e.g. :class:`UnsupportedFormatError`).
    """
    return RawFile(source, max_samples=max_samples, max_dimension=max_dimension)


def read_metadata(
    source: Source,
    *,
    max_samples: Optional[int] = None,
    max_dimension: Optional[int] = None,
) -> Metadata:
    """Metadata of a RAW file without decoding the sensor data (see :func:`open`)."""
    return Metadata._from(_flashraw.read_metadata(source, max_samples, max_dimension))


def detect_format(data: Source) -> Optional[str]:
    """The container format name (e.g. ``"CR3"``) when ``data`` (bytes-like or a path)
    looks like a supported RAW file, else ``None``. Cheap: no full parse."""
    return _flashraw.detect_format(data)


def supports(data: Source) -> bool:
    """True when ``data`` (bytes-like or a path) looks like a supported RAW file."""
    return detect_format(data) is not None


def imread(source: Source, output: str = "rgba8", **options: Any) -> "npt.NDArray[Any]":
    """Decode a RAW file straight to a NumPy array (default: display-ready RGBA8).

    Equivalent to ``open(source).decode(output, **options).array``; the file is closed
    before returning. See :meth:`RawFile.decode` for ``output`` and the options.
    """
    with RawFile(source) as raw:
        return raw.decode(output, **options).array


def gpu_available() -> bool:
    """Whether a GPU backend is compiled in and an adapter is available."""
    return bool(_flashraw.gpu_available())


def build_info() -> Dict[str, Any]:
    """Version, compiled features, SIMD level and default thread count."""
    return dict(_flashraw.build_info())


# Present the public classes as members of `flashraw` (reprs, docs, pickling).
for _name in __all__:
    _obj = globals()[_name]
    if isinstance(_obj, type) and _obj.__module__.startswith("flashraw."):
        _obj.__module__ = __name__
del _name, _obj
