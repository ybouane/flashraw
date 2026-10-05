"""Typed, immutable views of FlashRaw metadata.

Every class here is a frozen :func:`dataclasses.dataclass`: attributes are typed, objects
compare by value, and :func:`dataclasses.asdict` turns any of them into plain dicts (e.g.
for JSON). Sequences are tuples. Enumerations are lowercase strings, as in the CLI and the JS
bindings (``layout="bayer"``, ``kind="truncated"``...). Orientations are EXIF values (1-8).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Dict, NamedTuple, Optional, Tuple

__all__ = [
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
]


def _tuple(v: Any) -> Tuple[Any, ...]:
    return tuple(v) if v is not None else ()


def _opt_tuple(v: Any) -> Optional[Tuple[Any, ...]]:
    return tuple(v) if v is not None else None


class Rect(NamedTuple):
    """A rectangle in pixel coordinates (``x``, ``y`` of the top-left corner)."""

    x: int
    y: int
    width: int
    height: int

    @property
    def right(self) -> int:
        """``x + width`` (exclusive)."""
        return self.x + self.width

    @property
    def bottom(self) -> int:
        """``y + height`` (exclusive)."""
        return self.y + self.height

    @property
    def slices(self) -> Tuple[slice, slice]:
        """``(rows, cols)`` slices for NumPy indexing: ``array[rect.slices]`` (a view)."""
        return (slice(self.y, self.y + self.height), slice(self.x, self.x + self.width))

    @classmethod
    def _from(cls, d: Dict[str, int]) -> "Rect":
        return cls(d["x"], d["y"], d["width"], d["height"])


@dataclass(frozen=True)
class CfaPattern:
    """Colour filter array pattern, anchored at sensor-buffer ``(0, 0)``.

    ``pattern`` lists the colours row by row (``"RGGB"`` for a 2x2 Bayer tile, 36 letters for
    X-Trans); letters are ``R G B C M Y W E``.
    """

    pattern: str
    width: int
    height: int

    def color_at(self, x: int, y: int) -> str:
        """Colour letter at sensor-buffer coordinates ``(x, y)``."""
        return self.pattern[(y % self.height) * self.width + (x % self.width)]

    def shifted(self, x: int, y: int) -> "CfaPattern":
        """The pattern as seen from an origin at ``(x, y)`` (e.g. a crop or region)."""
        p = "".join(self.color_at(x + i, y + j) for j in range(self.height) for i in range(self.width))
        return CfaPattern(p, self.width, self.height)

    @classmethod
    def _from(cls, d: Optional[Dict[str, Any]]) -> Optional["CfaPattern"]:
        return None if d is None else cls(d["pattern"], d["width"], d["height"])


@dataclass(frozen=True)
class BlackLevel:
    """Black level in raw sample units.

    ``values`` is a ``rows x cols x samples`` repeating pattern anchored at sensor-buffer
    ``(0, 0)``; ``row_deltas`` / ``col_deltas`` are per-row / per-column offsets (often empty).
    """

    rows: int
    cols: int
    samples: int
    values: Tuple[float, ...]
    mean: float
    row_deltas: Tuple[float, ...] = ()
    col_deltas: Tuple[float, ...] = ()

    def at(self, x: int, y: int, sample: int = 0) -> float:
        """Black level of sample ``sample`` at sensor-buffer coordinates ``(x, y)``."""
        rows, cols, s = max(self.rows, 1), max(self.cols, 1), max(self.samples, 1)
        i = ((y % rows) * cols + (x % cols)) * s + (sample % s)
        v = self.values[i] if i < len(self.values) else 0.0
        if y < len(self.row_deltas):
            v += self.row_deltas[y]
        if x < len(self.col_deltas):
            v += self.col_deltas[x]
        return v

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "BlackLevel":
        return cls(
            d["rows"],
            d["cols"],
            d["samples"],
            _tuple(d["values"]),
            d["mean"] if d["mean"] is not None else 0.0,
            _tuple(d.get("row_deltas")),
            _tuple(d.get("col_deltas")),
        )


@dataclass(frozen=True)
class ColorMatrix:
    """A colour matrix (``rows`` x ``len(values) // rows``, row-major) and its illuminant.

    ``illuminant`` is the EXIF LightSource code; ``temperature`` its correlated colour
    temperature in kelvin when known.
    """

    illuminant: int
    temperature: Optional[float]
    rows: int
    values: Tuple[float, ...]

    def as_array(self) -> Any:
        """The matrix as a ``float64`` NumPy array of shape ``(rows, cols)``."""
        import numpy as np

        return np.asarray(self.values, dtype=np.float64).reshape(self.rows, -1)

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "ColorMatrix":
        return cls(d["illuminant"], d["temperature"], d["rows"], _tuple(d["values"]))


@dataclass(frozen=True)
class ColorMatrices:
    """Colour calibration of the camera.

    ``color``: XYZ -> camera matrices; ``forward``: white-balanced camera -> XYZ (D50);
    ``calibration``: per-unit calibration; ``source``: ``"file"``, ``"camera-db"`` or
    ``"none"``.
    """

    color: Tuple[ColorMatrix, ...]
    forward: Tuple[ColorMatrix, ...]
    calibration: Tuple[ColorMatrix, ...]
    analog_balance: Tuple[float, ...]
    source: str

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "ColorMatrices":
        return cls(
            tuple(ColorMatrix._from(m) for m in d["color"]),
            tuple(ColorMatrix._from(m) for m in d["forward"]),
            tuple(ColorMatrix._from(m) for m in d["calibration"]),
            _tuple(d["analog_balance"]),
            d["source"],
        )


@dataclass(frozen=True)
class OrientationInfo:
    """Orientation records and how :attr:`Metadata.orientation` was chosen.

    ``exif``: the standard TIFF/EXIF tag; ``maker_note``: the vendor record; ``auto_rotate``:
    the camera's Auto Rotate setting when recorded; ``resolved``: the value used; ``source``:
    ``"exif"``, ``"maker-note"``, ``"camera-profile"`` or ``"default"``; ``policy``: the
    per-vendor rule applied.
    """

    exif: Optional[int]
    maker_note: Optional[int]
    auto_rotate: Optional[bool]
    resolved: int
    source: str
    policy: str

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "OrientationInfo":
        return cls(d["exif"], d["maker_note"], d["auto_rotate"], d["resolved"], d["source"], d["policy"])


@dataclass(frozen=True)
class PreviewInfo:
    """An embedded preview or thumbnail.

    ``kind``: ``"thumbnail"``, ``"preview"`` or ``"full-size"``; ``encoding``: ``"jpeg"``,
    ``"rgb8"``, ``"ycbcr422"``, ``"jpeg-xl"`` or ``"heif"``; ``offset`` / ``byte_length``:
    where the encoded bytes are in the file; ``orientation``: EXIF orientation still to apply.
    """

    index: int
    kind: str
    encoding: str
    mime_type: str
    width: int
    height: int
    byte_length: int
    offset: int
    orientation: int

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "PreviewInfo":
        return cls(
            d["index"],
            d["kind"],
            d["encoding"],
            d["mime_type"],
            d["width"],
            d["height"],
            d["byte_length"],
            d["offset"],
            d["orientation"],
        )


@dataclass(frozen=True)
class Camera:
    """Camera identification. ``canonical_*`` are the normalised names from the camera
    database (e.g. ``"Canon"`` / ``"EOS 5D Mark IV"``), ``make`` / ``model`` as stored."""

    name: str
    make: str
    model: str
    canonical_make: str
    canonical_model: str
    unique_model: Optional[str] = None
    serial: Optional[str] = None
    firmware: Optional[str] = None

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "Camera":
        return cls(
            d["name"],
            d["make"],
            d["model"],
            d["canonical_make"],
            d["canonical_model"],
            d["unique_model"],
            d["serial"],
            d["firmware"],
        )


@dataclass(frozen=True)
class Lens:
    """Lens information (``focal_range`` in mm, ``aperture_range`` as f-numbers)."""

    make: Optional[str] = None
    model: Optional[str] = None
    serial: Optional[str] = None
    focal_range: Optional[Tuple[float, float]] = None
    aperture_range: Optional[Tuple[float, float]] = None

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "Lens":
        return cls(d["make"], d["model"], d["serial"], _opt_tuple(d["focal_range"]), _opt_tuple(d["aperture_range"]))


@dataclass(frozen=True)
class Exposure:
    """Exposure settings (``exposure_time`` in seconds, ``focal_length`` in mm; ``flash``,
    ``metering_mode`` and ``exposure_program`` are the EXIF codes)."""

    iso: Optional[int] = None
    exposure_time: Optional[float] = None
    f_number: Optional[float] = None
    focal_length: Optional[float] = None
    focal_length_35mm: Optional[int] = None
    exposure_bias: Optional[float] = None
    flash: Optional[int] = None
    metering_mode: Optional[int] = None
    exposure_program: Optional[int] = None

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "Exposure":
        return cls(**d)


@dataclass(frozen=True)
class Gps:
    """GPS position (decimal degrees, metres)."""

    latitude: Optional[float] = None
    longitude: Optional[float] = None
    altitude: Optional[float] = None
    timestamp: Optional[str] = None

    @classmethod
    def _from(cls, d: Optional[Dict[str, Any]]) -> Optional["Gps"]:
        return None if d is None else cls(**d)


@dataclass(frozen=True)
class DngProfile:
    """Camera-profile tags present in a DNG (presence and dimensions, not the tables)."""

    name: Optional[str] = None
    embed_policy: Optional[int] = None
    hue_sat_map_dims: Optional[Tuple[int, int, int]] = None
    hue_sat_map_count: int = 0
    look_table_dims: Optional[Tuple[int, int, int]] = None
    tone_curve: bool = False
    baseline_exposure_offset: Optional[float] = None

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "DngProfile":
        return cls(
            d["name"],
            d["embed_policy"],
            _opt_tuple(d["hue_sat_map_dims"]),
            d["hue_sat_map_count"],
            _opt_tuple(d["look_table_dims"]),
            d["tone_curve"],
            d["baseline_exposure_offset"],
        )


@dataclass(frozen=True)
class DngInfo:
    """DNG rendering hints (``Metadata.dng``)."""

    baseline_exposure: Optional[float] = None
    baseline_noise: Optional[float] = None
    baseline_sharpness: Optional[float] = None
    linear_response_limit: Optional[float] = None
    as_shot_neutral: Optional[Tuple[float, ...]] = None
    as_shot_white_xy: Optional[Tuple[float, float]] = None
    default_user_crop: Optional[Tuple[float, float, float, float]] = None
    default_scale: Optional[Tuple[float, float]] = None
    noise_profile: Optional[Tuple[float, ...]] = None
    profile: DngProfile = field(default_factory=DngProfile)

    @classmethod
    def _from(cls, d: Optional[Dict[str, Any]]) -> Optional["DngInfo"]:
        if d is None:
            return None
        return cls(
            d["baseline_exposure"],
            d["baseline_noise"],
            d["baseline_sharpness"],
            d["linear_response_limit"],
            _opt_tuple(d["as_shot_neutral"]),
            _opt_tuple(d["as_shot_white_xy"]),
            _opt_tuple(d["default_user_crop"]),
            _opt_tuple(d["default_scale"]),
            _opt_tuple(d["noise_profile"]),
            DngProfile._from(d["profile"]),
        )


@dataclass(frozen=True)
class Opcode:
    """A DNG opcode (``list`` is 1, 2 or 3). ``applied`` tells whether decoding applies it
    (pixel opcodes) or leaves it to the caller (geometric ones, whose parameters are in
    ``params`` for ``WarpRectilinear`` and ``FixVignetteRadial``)."""

    list: int
    id: int
    name: str
    optional: bool
    preview_only: bool
    applied: bool
    params: Optional[Dict[str, Any]] = None

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "Opcode":
        return cls(d["list"], d["id"], d["name"], d["optional"], d["preview_only"], d["applied"], d["params"])


@dataclass(frozen=True)
class Metadata:
    """Normalised file metadata (no pixel decoding involved).

    ``width`` / ``height`` are the visible image size, ``raw_width`` / ``raw_height`` the
    sensor buffer. ``white_level`` has one value per CFA colour/sample; ``white_balance`` is
    the as-shot multipliers (or ``None``). ``extra`` holds format-specific key/values.
    """

    format: str
    camera: Camera
    lens: Lens
    exposure: Exposure
    orientation: int
    orientation_info: OrientationInfo
    width: int
    height: int
    raw_width: int
    raw_height: int
    bits_per_sample: int
    compression: str
    cfa: Optional[CfaPattern]
    layout: str
    black_level: BlackLevel
    white_level: Tuple[int, ...]
    white_balance: Optional[Tuple[float, ...]]
    color_matrices: ColorMatrices
    previews: Tuple[PreviewInfo, ...]
    software: Optional[str] = None
    artist: Optional[str] = None
    copyright: Optional[str] = None
    capture_time: Optional[str] = None
    gps: Optional[Gps] = None
    dng_version: Optional[str] = None
    dng: Optional[DngInfo] = None
    opcodes: Tuple[Opcode, ...] = ()
    extra: Dict[str, str] = field(default_factory=dict)

    @property
    def make(self) -> str:
        """Camera make as stored in the file."""
        return self.camera.make

    @property
    def model(self) -> str:
        """Camera model as stored in the file."""
        return self.camera.model

    @property
    def canonical_make(self) -> str:
        """Normalised camera make."""
        return self.camera.canonical_make

    @property
    def canonical_model(self) -> str:
        """Normalised camera model."""
        return self.camera.canonical_model

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "Metadata":
        return cls(
            format=d["format"],
            camera=Camera._from(d["camera"]),
            lens=Lens._from(d["lens"]),
            exposure=Exposure._from(d["exposure"]),
            orientation=d["orientation"],
            orientation_info=OrientationInfo._from(d["orientation_info"]),
            width=d["width"],
            height=d["height"],
            raw_width=d["raw_width"],
            raw_height=d["raw_height"],
            bits_per_sample=d["bits_per_sample"],
            compression=d["compression"],
            cfa=CfaPattern._from(d["cfa"]),
            layout=d["layout"],
            black_level=BlackLevel._from(d["black_level"]),
            white_level=_tuple(d["white_level"]),
            white_balance=_opt_tuple(d["white_balance"]),
            color_matrices=ColorMatrices._from(d["color_matrices"]),
            previews=tuple(PreviewInfo._from(p) for p in d["previews"]),
            software=d["software"],
            artist=d["artist"],
            copyright=d["copyright"],
            capture_time=d["capture_time"],
            gps=Gps._from(d["gps"]),
            dng_version=d["dng_version"],
            dng=DngInfo._from(d["dng"]),
            opcodes=tuple(Opcode._from(o) for o in d["opcodes"]),
            extra=dict(d["extra"]),
        )


@dataclass(frozen=True)
class FrameInfo:
    """Geometry and calibration of the sensor buffer, known before decoding.

    ``sample_format`` is ``"u16"`` or ``"f32"`` (floating-point DNG); ``black_from_masked``
    tells that the black level is measured from the masked areas at decode time.
    """

    width: int
    height: int
    channels: int
    bits_per_sample: int
    sample_format: str
    cfa: Optional[CfaPattern]
    layout: str
    active_area: Rect
    crop: Rect
    masked_areas: Tuple[Rect, ...]
    black_level: BlackLevel
    black_from_masked: bool
    white_level: Tuple[int, ...]
    white_balance: Optional[Tuple[float, ...]]
    color_matrices: ColorMatrices
    orientation: int
    pixel_aspect: Optional[float]
    compression: str

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "FrameInfo":
        return cls(
            d["width"],
            d["height"],
            d["channels"],
            d["bits_per_sample"],
            d["sample_format"],
            CfaPattern._from(d["cfa"]),
            d["layout"],
            Rect._from(d["active_area"]),
            Rect._from(d["crop"]),
            tuple(Rect._from(r) for r in d["masked_areas"]),
            BlackLevel._from(d["black_level"]),
            d["black_from_masked"],
            _tuple(d["white_level"]),
            _opt_tuple(d["white_balance"]),
            ColorMatrices._from(d["color_matrices"]),
            d["orientation"],
            d["pixel_aspect"],
            d["compression"],
        )


@dataclass(frozen=True)
class RandomAccess:
    """How the payload can be accessed independently: ``kind`` is ``"sequential"``,
    ``"rows"``, ``"strips"`` (``rows`` per strip), ``"tiles"`` (``width`` x ``height``) or
    ``"blocks"``."""

    kind: str
    rows: Optional[int] = None
    width: Optional[int] = None
    height: Optional[int] = None

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "RandomAccess":
        return cls(d["kind"], d.get("rows"), d.get("width"), d.get("height"))


@dataclass(frozen=True)
class Capabilities:
    """What a file supports: random access, number of tiles and previews, whether the
    payload can be unpacked on the GPU and whether the sensor data is floating-point."""

    random_access: RandomAccess
    tiles: int
    previews: int
    gpu_unpack: bool
    float: bool

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "Capabilities":
        return cls(RandomAccess._from(d["random_access"]), d["tiles"], d["previews"], d["gpu_unpack"], d["float"])


@dataclass(frozen=True)
class TileInfo:
    """An independently decodable tile or strip (see :meth:`RawFile.decode_tile`)."""

    index: int
    rect: Rect
    offset: int
    byte_length: int

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "TileInfo":
        return cls(d["index"], Rect._from(d["rect"]), d["offset"], d["byte_length"])


@dataclass(frozen=True)
class CorruptInfo:
    """Structured cause of corrupt or truncated sensor data.

    ``kind``: ``"truncated"`` or ``"invalid-unit"``; ``unit``: ``"strip"``, ``"tile"``,
    ``"plane"`` or ``"payload"`` with its ``index``. For truncation, ``expected`` bytes were
    declared at ``offset`` but only ``available`` exist in the ``file_length``-byte input.
    """

    kind: str
    unit: str
    index: Optional[int]
    message: str
    offset: Optional[int] = None
    expected: Optional[int] = None
    available: Optional[int] = None
    file_length: Optional[int] = None

    @property
    def missing(self) -> Optional[int]:
        """Bytes missing from a truncated unit."""
        if self.expected is None or self.available is None:
            return None
        return max(self.expected - self.available, 0)

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "CorruptInfo":
        return cls(
            d["kind"],
            d["unit"],
            d["index"],
            d["message"],
            d["offset"],
            d["expected"],
            d["available"],
            d["file_length"],
        )


@dataclass(frozen=True)
class DecodeWarning(CorruptInfo):
    """A repair made under ``corruption_policy="recover"``: the cause plus the number of
    samples that were zero-filled."""

    samples_filled: int = 0

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "DecodeWarning":
        return cls(
            d["kind"],
            d["unit"],
            d["index"],
            d["message"],
            d["offset"],
            d["expected"],
            d["available"],
            d["file_length"],
            d["samples_filled"],
        )


@dataclass(frozen=True)
class BackendReport:
    """Which backend ran: ``requested`` (``"auto"``, ``"cpu"``, ``"simd"``, ``"gpu"``),
    ``kind`` (``"cpu"`` or ``"gpu"``), ``decode`` (e.g. ``"CPU AVX2 x16 threads"``), the SIMD
    level and the worker threads available to the decode."""

    requested: str
    kind: str
    decode: str
    postprocess: Optional[str]
    simd: str
    threads: int
    gpu_adapter: Optional[str] = None

    @classmethod
    def _from(cls, d: Dict[str, Any]) -> "BackendReport":
        return cls(**d)


@dataclass(frozen=True)
class DecodeStats:
    """Timings of one decode.

    ``total_ms`` is the decode as measured by the library (including container parsing in
    ``open``); ``stages`` maps pipeline stages (``"entropy_decode"``, ``"unpack"``,
    ``"postprocess"``...) to milliseconds. ``call_ms`` is the wall-clock time of the native
    decode call (as ``flashraw benchmark`` measures it) and ``wall_ms`` that of the Python
    ``decode()`` call: ``wall_ms - call_ms`` is the cost of the Python binding.
    """

    backend: BackendReport
    total_ms: float
    stages: Dict[str, float]
    input_bytes: int
    output_bytes: int
    opcodes: Tuple[str, ...]
    call_ms: float = 0.0
    wall_ms: float = 0.0

    @classmethod
    def _from(cls, d: Dict[str, Any], wall_ms: float = 0.0) -> "DecodeStats":
        return cls(
            BackendReport._from(d["backend"]),
            d["total_ms"] if d["total_ms"] is not None else 0.0,
            dict(d["stages"]),
            d["input_bytes"],
            d["output_bytes"],
            _tuple(d["opcodes"]),
            d.get("call_ms", 0.0),
            wall_ms,
        )
