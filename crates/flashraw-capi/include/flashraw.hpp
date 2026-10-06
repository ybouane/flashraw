// FlashRaw C++ API: a header-only C++17 RAII wrapper over the C API in flashraw.h.
//
//   #include <flashraw.hpp>
//
//   flashraw::file raw = flashraw::file::open("photo.NEF");
//   std::cout << raw.camera() << " " << raw.raw_width() << "x" << raw.raw_height() << "\n";
//
//   flashraw::decode_options opts;
//   opts.output = flashraw::output_format::rgba8;
//   flashraw::frame img = raw.decode(opts);
//   flashraw::span<const std::uint8_t> rgba = img.u8();
//
// Errors are reported as exceptions derived from flashraw::error (one class per error kind,
// e.g. flashraw::corrupt_file_error). Handles are move-only and released automatically.
// Thread safety follows the C API: a const flashraw::file may be decoded from several
// threads at once; frames are independent of each other and of their file.
//
// Documentation: docs/c-api.md. Licensed under MIT OR Apache-2.0.

#ifndef FLASHRAW_HPP
#define FLASHRAW_HPP

#include "flashraw.h"

#include <array>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <type_traits>
#include <utility>
#include <vector>

namespace flashraw {

// ---------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------

/// Base class of every exception thrown by the wrapper.
class error : public std::runtime_error {
public:
    error(flashraw_status code, std::string kind, const std::string &message, std::string detail_json)
        : std::runtime_error(message), code_(code), kind_(std::move(kind)), detail_json_(std::move(detail_json)) {}

    /// The C status code (FLASHRAW_ERROR_*).
    flashraw_status code() const noexcept { return code_; }
    /// Kind name, e.g. "CorruptFile" (matches the Rust library's Error::kind()).
    const std::string &kind() const noexcept { return kind_; }
    /// Structured detail as JSON (e.g. the truncated strip), or an empty string.
    const std::string &detail_json() const noexcept { return detail_json_; }

private:
    flashraw_status code_;
    std::string kind_;
    std::string detail_json_;
};

#define FLASHRAW_HPP_ERROR(name)                                                                    \
    class name : public error {                                                                    \
    public:                                                                                        \
        using error::error;                                                                        \
    };
FLASHRAW_HPP_ERROR(unsupported_format_error)
FLASHRAW_HPP_ERROR(unsupported_compression_error)
FLASHRAW_HPP_ERROR(corrupt_file_error)
FLASHRAW_HPP_ERROR(invalid_metadata_error)
FLASHRAW_HPP_ERROR(camera_unsupported_error)
FLASHRAW_HPP_ERROR(gpu_unavailable_error)
FLASHRAW_HPP_ERROR(out_of_memory_error)
FLASHRAW_HPP_ERROR(limit_exceeded_error)
FLASHRAW_HPP_ERROR(not_available_error)
FLASHRAW_HPP_ERROR(invalid_argument_error)
FLASHRAW_HPP_ERROR(io_error)
FLASHRAW_HPP_ERROR(internal_error)
#undef FLASHRAW_HPP_ERROR

namespace detail {

inline std::string str(const char *p) { return p ? std::string(p) : std::string(); }

inline std::optional<std::string> opt_str(const char *p) {
    if (!p) return std::nullopt;
    return std::string(p);
}

/// Throw the exception matching `status`, using the thread's last error for details.
[[noreturn]] inline void throw_status(flashraw_status status) {
    std::string kind = str(flashraw_last_error_kind());
    std::string msg = str(flashraw_last_error_message());
    std::string detail = str(flashraw_last_error_detail_json());
    if (kind.empty()) kind = str(flashraw_error_kind_name(status));
    if (msg.empty()) msg = "flashraw: " + kind;
    switch (status) {
    case FLASHRAW_ERROR_UNSUPPORTED_FORMAT: throw unsupported_format_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_UNSUPPORTED_COMPRESSION: throw unsupported_compression_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_CORRUPT_FILE: throw corrupt_file_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_INVALID_METADATA: throw invalid_metadata_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_CAMERA_UNSUPPORTED: throw camera_unsupported_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_GPU_UNAVAILABLE: throw gpu_unavailable_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_OUT_OF_MEMORY: throw out_of_memory_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_LIMIT_EXCEEDED: throw limit_exceeded_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_NOT_AVAILABLE: throw not_available_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_INVALID_ARGUMENT: throw invalid_argument_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_IO: throw io_error(status, kind, msg, detail);
    case FLASHRAW_ERROR_INTERNAL: throw internal_error(status, kind, msg, detail);
    default: throw error(status, kind, msg, detail);
    }
}

inline void check(flashraw_status status) {
    if (status != FLASHRAW_OK) throw_status(status);
}

/// Take ownership of a string returned by a *_json() function.
inline std::string take_string(char *p) {
    if (!p) throw_status(flashraw_last_error_code());
    std::string s(p);
    flashraw_string_free(p);
    return s;
}

} // namespace detail

// ---------------------------------------------------------------------------------------
// Small value types
// ---------------------------------------------------------------------------------------

/// A non-owning view of contiguous elements (a C++17 stand-in for std::span).
template <class T> class span {
public:
    using element_type = T;
    using value_type = std::remove_cv_t<T>;
    using size_type = std::size_t;
    using iterator = T *;

    constexpr span() noexcept = default;
    constexpr span(T *data, std::size_t size) noexcept : data_(data), size_(size) {}

    constexpr T *data() const noexcept { return data_; }
    constexpr std::size_t size() const noexcept { return size_; }
    constexpr std::size_t size_bytes() const noexcept { return size_ * sizeof(T); }
    constexpr bool empty() const noexcept { return size_ == 0; }
    constexpr T &operator[](std::size_t i) const noexcept { return data_[i]; }
    constexpr T *begin() const noexcept { return data_; }
    constexpr T *end() const noexcept { return data_ + size_; }
    /// Elements [offset, offset + count).
    constexpr span subspan(std::size_t offset, std::size_t count) const noexcept { return span(data_ + offset, count); }

private:
    T *data_ = nullptr;
    std::size_t size_ = 0;
};

enum class backend : std::int32_t {
    automatic = FLASHRAW_BACKEND_AUTO,
    cpu = FLASHRAW_BACKEND_CPU,
    cpu_simd = FLASHRAW_BACKEND_CPU_SIMD,
    gpu = FLASHRAW_BACKEND_GPU,
};

enum class output_format : std::int32_t {
    raw_u16 = FLASHRAW_OUTPUT_RAW_U16,
    raw_f32 = FLASHRAW_OUTPUT_RAW_F32,
    normalized_f32 = FLASHRAW_OUTPUT_NORMALIZED_F32,
    linear_rgb_f32 = FLASHRAW_OUTPUT_LINEAR_RGB_F32,
    rgba8 = FLASHRAW_OUTPUT_RGBA8,
    rgb16 = FLASHRAW_OUTPUT_RGB16,
    linear_rgb16 = FLASHRAW_OUTPUT_LINEAR_RGB16,
    linear_rgb_f16 = FLASHRAW_OUTPUT_LINEAR_RGB_F16,
};

enum class demosaic : std::int32_t {
    bilinear = FLASHRAW_DEMOSAIC_BILINEAR,
    half_size = FLASHRAW_DEMOSAIC_HALF_SIZE,
};

enum class corruption_policy : std::int32_t {
    recover = FLASHRAW_CORRUPTION_RECOVER,
    strict = FLASHRAW_CORRUPTION_STRICT,
};

enum class sample_type : std::int32_t {
    u8 = FLASHRAW_SAMPLE_U8,
    u16 = FLASHRAW_SAMPLE_U16,
    f32 = FLASHRAW_SAMPLE_F32,
    /// IEEE half floats as `std::uint16_t` bit patterns (`frame::f16_bits()`).
    f16 = FLASHRAW_SAMPLE_F16,
};

enum class sensor_layout : std::int32_t {
    bayer = FLASHRAW_LAYOUT_BAYER,
    xtrans = FLASHRAW_LAYOUT_XTRANS,
    quad_bayer = FLASHRAW_LAYOUT_QUAD_BAYER,
    other_cfa = FLASHRAW_LAYOUT_OTHER_CFA,
    monochrome = FLASHRAW_LAYOUT_MONOCHROME,
    linear = FLASHRAW_LAYOUT_LINEAR,
    foveon = FLASHRAW_LAYOUT_FOVEON,
    unknown = FLASHRAW_LAYOUT_UNKNOWN,
};

enum class matrix_kind : std::int32_t {
    color = FLASHRAW_MATRIX_COLOR,
    forward = FLASHRAW_MATRIX_FORWARD,
    calibration = FLASHRAW_MATRIX_CALIBRATION,
};

using rect = flashraw_rect;

/// A CFA pattern: `colors[y * width + x]` is a flashraw_cfa_color value.
struct cfa_pattern {
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    std::vector<std::uint8_t> colors;

    /// Colour at sensor-buffer coordinates (the pattern repeats).
    std::uint8_t at(std::uint32_t x, std::uint32_t y) const { return colors[(y % height) * width + (x % width)]; }
    /// Pattern as letters, e.g. "RGGB".
    std::string letters() const {
        static const char names[] = "RGBCMYWE";
        std::string s;
        for (auto c : colors) s.push_back(c < 8 ? names[c] : '?');
        return s;
    }
};

/// Black level: `values[((y % rows) * cols + (x % cols)) * samples + (c % samples)]`.
struct black_level {
    std::uint32_t rows = 1;
    std::uint32_t cols = 1;
    std::uint32_t samples = 1;
    float mean = 0.0f;
    bool has_deltas = false;
    std::vector<float> values;
};

struct color_matrix {
    std::uint32_t illuminant = 0;
    std::uint32_t rows = 0;
    std::uint32_t cols = 0;
    std::vector<float> values;
};

struct preview_info {
    std::size_t index = 0;
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    flashraw_preview_encoding encoding = FLASHRAW_PREVIEW_UNKNOWN;
    flashraw_preview_kind kind = FLASHRAW_PREVIEW_KIND_UNKNOWN;
    std::uint32_t orientation = 1;
    std::uint64_t offset = 0;
    std::uint64_t length = 0;
};

struct decode_stats {
    std::uint32_t threads = 0;
    double total_ms = 0.0;
    std::uint64_t input_bytes = 0;
    std::uint64_t output_bytes = 0;
    /// Milliseconds per flashraw_stage value.
    std::array<double, 11> stage_ms{};
};

struct open_options {
    /// Maximum width or height (0 = library default).
    std::uint32_t max_dimension = 0;
    /// Maximum number of samples (0 = library default).
    std::uint64_t max_samples = 0;

    flashraw_open_options to_c() const {
        flashraw_open_options o;
        detail::check(flashraw_open_options_init(&o));
        o.max_dimension = max_dimension;
        o.max_samples = max_samples;
        return o;
    }
};

struct decode_options {
    flashraw::backend backend = flashraw::backend::automatic;
    flashraw::output_format output = flashraw::output_format::raw_u16;
    flashraw::demosaic demosaic = flashraw::demosaic::bilinear;
    flashraw::corruption_policy corruption = flashraw::corruption_policy::recover;
    /// Worker threads (0 = all cores).
    unsigned threads = 0;
    /// Crop RGB outputs to the default crop.
    bool crop = true;
    /// Apply DNG opcode lists.
    bool apply_opcodes = true;
    /// Decode only this region (sensor-buffer coordinates).
    std::optional<flashraw::rect> region;
    /// Limits (0 = the file's open limits).
    std::uint32_t max_dimension = 0;
    std::uint64_t max_samples = 0;
    /// RGB outputs: exposure in EV and black/white points in linear units.
    float exposure = 0.0f;
    float black_point = 0.0f;
    float white_point = 1.0f;

    flashraw_decode_options to_c() const {
        flashraw_decode_options o;
        detail::check(flashraw_decode_options_init(&o));
        o.backend = static_cast<std::int32_t>(backend);
        o.output = static_cast<std::int32_t>(output);
        o.demosaic = static_cast<std::int32_t>(demosaic);
        o.corruption_policy = static_cast<std::int32_t>(corruption);
        o.threads = threads;
        o.crop = crop ? 1 : 0;
        o.apply_opcodes = apply_opcodes ? 1 : 0;
        if (region) {
            o.use_region = 1;
            o.region = *region;
        }
        o.max_dimension = max_dimension;
        o.max_samples = max_samples;
        o.exposure = exposure;
        o.black_point = black_point;
        o.white_point = white_point;
        return o;
    }
};

namespace detail {

template <class Handle, class F> std::optional<cfa_pattern> cfa(const Handle *h, F get) {
    flashraw_cfa c{};
    if (!get(h, &c)) return std::nullopt;
    cfa_pattern p;
    p.width = c.width;
    p.height = c.height;
    p.colors.assign(c.colors, c.colors + c.width * c.height);
    return p;
}

template <class Handle, class F> black_level black(const Handle *h, F get) {
    flashraw_black_level info{};
    std::size_t n = get(h, &info, nullptr, 0);
    black_level b;
    b.values.resize(n);
    get(h, &info, b.values.data(), b.values.size());
    b.rows = info.rows;
    b.cols = info.cols;
    b.samples = info.samples;
    b.mean = info.mean;
    b.has_deltas = info.has_deltas != 0;
    return b;
}

template <class Handle, class F> std::vector<std::uint32_t> white(const Handle *h, F get) {
    std::vector<std::uint32_t> v(get(h, nullptr, 0));
    get(h, v.data(), v.size());
    return v;
}

template <class Handle, class F> std::optional<std::array<float, 4>> wb(const Handle *h, F get) {
    std::array<float, 4> c{};
    if (!get(h, c.data())) return std::nullopt;
    return c;
}

template <class Handle, class Count, class Get>
std::vector<color_matrix> matrices(const Handle *h, matrix_kind kind, Count count, Get get) {
    std::vector<color_matrix> out;
    std::size_t n = count(h, static_cast<std::int32_t>(kind));
    for (std::size_t i = 0; i < n; ++i) {
        flashraw_color_matrix m{};
        if (get(h, static_cast<std::int32_t>(kind), i, &m) != FLASHRAW_OK) continue; // oversized: JSON only
        color_matrix c;
        c.illuminant = m.illuminant;
        c.rows = m.rows;
        c.cols = m.cols;
        c.values.assign(m.values, m.values + m.rows * m.cols);
        out.push_back(std::move(c));
    }
    return out;
}

struct file_deleter {
    void operator()(flashraw_file *f) const noexcept { flashraw_file_free(f); }
};
struct frame_deleter {
    void operator()(flashraw_frame *f) const noexcept { flashraw_frame_free(f); }
};

} // namespace detail

// ---------------------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------------------

/// A decoded frame. Owns its samples; independent of the file it came from.
class frame {
public:
    explicit frame(flashraw_frame *f) : f_(f) {}

    /// The underlying C handle (still owned by this object).
    const flashraw_frame *get() const noexcept { return f_.get(); }
    /// Give up ownership of the C handle.
    flashraw_frame *release() noexcept { return f_.release(); }

    std::uint32_t width() const { return flashraw_frame_width(h()); }
    std::uint32_t height() const { return flashraw_frame_height(h()); }
    /// Samples per pixel.
    std::uint32_t cpp() const { return flashraw_frame_cpp(h()); }
    flashraw::sample_type sample_type() const { return static_cast<flashraw::sample_type>(flashraw_frame_sample_type(h())); }
    flashraw::output_format output() const {
        return static_cast<flashraw::output_format>(flashraw_frame_output_format(h()));
    }
    std::uint32_t bits_per_sample() const { return flashraw_frame_bits_per_sample(h()); }
    /// EXIF orientation still to apply (1 for RGBA8).
    std::uint32_t orientation() const { return flashraw_frame_orientation(h()); }
    flashraw::sensor_layout sensor_layout() const {
        return static_cast<flashraw::sensor_layout>(flashraw_frame_sensor_layout(h()));
    }

    /// Raw sample bytes.
    span<const std::uint8_t> bytes() const {
        return span<const std::uint8_t>(static_cast<const std::uint8_t *>(flashraw_frame_data(h())), flashraw_frame_data_len(h()));
    }
    /// Samples as T (std::uint8_t, std::uint16_t or float); throws invalid_argument_error
    /// when T does not match sample_type().
    template <class T> span<const T> samples() const {
        static_assert(std::is_same<T, std::uint8_t>::value || std::is_same<T, std::uint16_t>::value ||
                          std::is_same<T, float>::value,
                      "samples<T>: T must be std::uint8_t, std::uint16_t or float");
        flashraw::sample_type want = std::is_same<T, std::uint8_t>::value    ? flashraw::sample_type::u8
                                     : std::is_same<T, std::uint16_t>::value ? flashraw::sample_type::u16
                                                                             : flashraw::sample_type::f32;
        if (sample_type() != want)
            throw invalid_argument_error(FLASHRAW_ERROR_INVALID_ARGUMENT, "InvalidArgument",
                                         "flashraw::frame::samples: element type does not match the frame", "");
        return span<const T>(static_cast<const T *>(flashraw_frame_data(h())), flashraw_frame_sample_count(h()));
    }
    span<const std::uint8_t> u8() const { return samples<std::uint8_t>(); }
    span<const std::uint16_t> u16() const { return samples<std::uint16_t>(); }
    span<const float> f32() const { return samples<float>(); }
    /// Half-float samples (`output_format::linear_rgb_f16`) as IEEE binary16 bit patterns;
    /// throws invalid_argument_error for other frames.
    span<const std::uint16_t> f16_bits() const {
        if (sample_type() != flashraw::sample_type::f16)
            throw invalid_argument_error(FLASHRAW_ERROR_INVALID_ARGUMENT, "InvalidArgument",
                                         "flashraw::frame::f16_bits: the frame does not hold half floats", "");
        return span<const std::uint16_t>(static_cast<const std::uint16_t *>(flashraw_frame_data(h())),
                                         flashraw_frame_sample_count(h()));
    }

    rect active_area() const {
        rect r{};
        detail::check(flashraw_frame_active_area(h(), &r));
        return r;
    }
    rect crop() const {
        rect r{};
        detail::check(flashraw_frame_crop(h(), &r));
        return r;
    }
    /// Origin in sensor-buffer coordinates (non-zero for region decodes).
    std::pair<std::uint32_t, std::uint32_t> origin() const {
        std::uint32_t x = 0, y = 0;
        detail::check(flashraw_frame_origin(h(), &x, &y));
        return {x, y};
    }
    std::optional<cfa_pattern> cfa() const { return detail::cfa(h(), flashraw_frame_cfa); }
    /// CFA colour at frame coordinates, or -1 without a CFA.
    int color_at(std::uint32_t x, std::uint32_t y) const { return flashraw_frame_color_at(h(), x, y); }
    flashraw::black_level black_level() const { return detail::black(h(), flashraw_frame_black_level); }
    std::vector<std::uint32_t> white_level() const { return detail::white(h(), flashraw_frame_white_level); }
    std::optional<std::array<float, 4>> white_balance() const { return detail::wb(h(), flashraw_frame_white_balance); }
    std::vector<color_matrix> color_matrices(matrix_kind kind = matrix_kind::color) const {
        return detail::matrices(h(), kind, flashraw_frame_color_matrix_count, flashraw_frame_color_matrix);
    }

    /// True when some samples were zero-filled while recovering from corrupt input.
    bool is_incomplete() const { return flashraw_frame_is_incomplete(h()); }
    std::vector<std::string> warnings() const {
        std::vector<std::string> w;
        std::size_t n = flashraw_frame_warning_count(h());
        for (std::size_t i = 0; i < n; ++i) w.push_back(detail::str(flashraw_frame_warning(h(), i)));
        return w;
    }
    decode_stats stats() const {
        flashraw_decode_stats s{};
        s.size = sizeof s;
        detail::check(flashraw_frame_stats(h(), &s));
        decode_stats d;
        d.threads = s.threads;
        d.total_ms = s.total_ms;
        d.input_bytes = s.input_bytes;
        d.output_bytes = s.output_bytes;
        for (std::size_t i = 0; i < d.stage_ms.size(); ++i) d.stage_ms[i] = s.stage_ms[i];
        return d;
    }
    /// Backend that decoded the frame, e.g. "CPU AVX2 x16 threads".
    std::string backend() const { return detail::str(flashraw_frame_backend(h())); }
    /// Full description as JSON (geometry, calibration, statistics, warnings).
    std::string json() const { return detail::take_string(flashraw_frame_json(h())); }

private:
    const flashraw_frame *h() const {
        if (!f_)
            throw invalid_argument_error(FLASHRAW_ERROR_INVALID_ARGUMENT, "InvalidArgument",
                                         "flashraw::frame used after move", "");
        return f_.get();
    }
    std::unique_ptr<flashraw_frame, detail::frame_deleter> f_;
};

// ---------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------

/// An opened RAW file: metadata and previews immediately, pixels via decode().
class file {
public:
    /// Open a file from disk (memory-mapped). `path` is UTF-8.
    static file open(const std::string &path, const open_options &opts = {}) {
        flashraw_open_options o = opts.to_c();
        flashraw_file *f = nullptr;
        detail::check(flashraw_open_path(path.c_str(), &o, &f));
        return file(f);
    }
    /// Open bytes in memory without copying: `data` must outlive the returned object.
    static file open_memory(const void *data, std::size_t len, const open_options &opts = {}) {
        flashraw_open_options o = opts.to_c();
        flashraw_file *f = nullptr;
        detail::check(flashraw_open_memory(static_cast<const std::uint8_t *>(data), len, &o, &f));
        return file(f);
    }
    /// Open a copy of bytes in memory (`data` may be released afterwards).
    static file open_memory_copy(const void *data, std::size_t len, const open_options &opts = {}) {
        flashraw_open_options o = opts.to_c();
        flashraw_file *f = nullptr;
        detail::check(flashraw_open_memory_copy(static_cast<const std::uint8_t *>(data), len, &o, &f));
        return file(f);
    }

    explicit file(flashraw_file *f) : f_(f) {}

    /// The underlying C handle (still owned by this object).
    const flashraw_file *get() const noexcept { return f_.get(); }
    /// Give up ownership of the C handle.
    flashraw_file *release() noexcept { return f_.release(); }

    /// A metadata string (flashraw_string_key), or nullopt when not recorded.
    std::optional<std::string> string(flashraw_string_key key) const {
        return detail::opt_str(flashraw_file_string(h(), static_cast<std::int32_t>(key)));
    }
    std::string make() const { return detail::str(flashraw_file_make(h())); }
    std::string model() const { return detail::str(flashraw_file_model(h())); }
    std::string canonical_make() const { return detail::str(flashraw_file_canonical_make(h())); }
    std::string canonical_model() const { return detail::str(flashraw_file_canonical_model(h())); }
    /// "<canonical make> <canonical model>".
    std::string camera() const { return string(FLASHRAW_STRING_CAMERA).value_or(std::string()); }
    /// "DNG", "NEF", "CR3", ...
    std::string format_name() const { return detail::str(flashraw_file_format_name(h())); }
    std::optional<std::string> lens() const { return string(FLASHRAW_STRING_LENS_MODEL); }
    std::optional<std::string> capture_time() const { return string(FLASHRAW_STRING_CAPTURE_TIME); }

    std::uint32_t raw_width() const { return flashraw_file_raw_width(h()); }
    std::uint32_t raw_height() const { return flashraw_file_raw_height(h()); }
    /// Size after the default crop.
    std::uint32_t width() const { return flashraw_file_width(h()); }
    std::uint32_t height() const { return flashraw_file_height(h()); }
    std::uint32_t cpp() const { return flashraw_file_cpp(h()); }
    std::uint32_t bits_per_sample() const { return flashraw_file_bits_per_sample(h()); }
    bool is_float() const { return flashraw_file_is_float(h()); }
    std::uint32_t orientation() const { return flashraw_file_orientation(h()); }
    flashraw::sensor_layout sensor_layout() const {
        return static_cast<flashraw::sensor_layout>(flashraw_file_sensor_layout(h()));
    }
    rect active_area() const {
        rect r{};
        detail::check(flashraw_file_active_area(h(), &r));
        return r;
    }
    rect crop() const {
        rect r{};
        detail::check(flashraw_file_crop(h(), &r));
        return r;
    }
    std::optional<cfa_pattern> cfa() const { return detail::cfa(h(), flashraw_file_cfa); }
    flashraw::black_level black_level() const { return detail::black(h(), flashraw_file_black_level); }
    std::vector<std::uint32_t> white_level() const { return detail::white(h(), flashraw_file_white_level); }
    std::optional<std::array<float, 4>> white_balance() const { return detail::wb(h(), flashraw_file_white_balance); }
    std::vector<color_matrix> color_matrices(matrix_kind kind = matrix_kind::color) const {
        return detail::matrices(h(), kind, flashraw_file_color_matrix_count, flashraw_file_color_matrix);
    }

    std::optional<std::uint32_t> iso() const { return opt(flashraw_file_iso); }
    /// Seconds.
    std::optional<double> exposure_time() const { return opt(flashraw_file_exposure_time); }
    std::optional<float> f_number() const { return opt(flashraw_file_f_number); }
    /// Millimetres.
    std::optional<float> focal_length() const { return opt(flashraw_file_focal_length); }

    std::vector<preview_info> previews() const {
        std::vector<preview_info> out;
        std::size_t n = flashraw_file_preview_count(h());
        for (std::size_t i = 0; i < n; ++i) {
            flashraw_preview_info p{};
            p.size = sizeof p;
            detail::check(flashraw_file_preview_info(h(), i, &p));
            preview_info q;
            q.index = i;
            q.width = p.width;
            q.height = p.height;
            q.encoding = static_cast<flashraw_preview_encoding>(p.encoding);
            q.kind = static_cast<flashraw_preview_kind>(p.kind);
            q.orientation = p.orientation;
            q.offset = p.offset;
            q.length = p.length;
            out.push_back(q);
        }
        return out;
    }
    /// Index of the largest preview, if any.
    std::optional<std::size_t> largest_preview() const {
        std::int64_t i = flashraw_file_largest_preview(h());
        if (i < 0) return std::nullopt;
        return static_cast<std::size_t>(i);
    }
    /// Bytes of preview `index` (e.g. a JPEG file), zero-copy: valid while this file lives.
    span<const std::uint8_t> preview_bytes(std::size_t index) const {
        std::size_t len = 0;
        const std::uint8_t *p = flashraw_file_preview_bytes(h(), index, &len);
        if (!p) detail::throw_status(flashraw_last_error_code());
        return span<const std::uint8_t>(p, len);
    }

    /// All metadata as JSON.
    std::string metadata_json() const { return detail::take_string(flashraw_file_metadata_json(h())); }

    /// Decode the sensor data. Safe to call concurrently on the same file.
    frame decode(const decode_options &opts = {}) const {
        flashraw_decode_options o = opts.to_c();
        flashraw_frame *fr = nullptr;
        detail::check(flashraw_decode(h(), &o, &fr));
        return frame(fr);
    }

private:
    const flashraw_file *h() const {
        if (!f_)
            throw invalid_argument_error(FLASHRAW_ERROR_INVALID_ARGUMENT, "InvalidArgument",
                                         "flashraw::file used after move", "");
        return f_.get();
    }
    template <class T> std::optional<T> opt(bool (*getter)(const flashraw_file *, T *)) const {
        T v{};
        if (!getter(h(), &v)) return std::nullopt;
        return v;
    }
    std::unique_ptr<flashraw_file, detail::file_deleter> f_;
};

// ---------------------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------------------

/// Library version, e.g. "0.1.0".
inline std::string version() { return detail::str(flashraw_version()); }

/// True when the bytes look like a supported RAW file.
inline bool supports(const void *data, std::size_t len) {
    return flashraw_supports(static_cast<const std::uint8_t *>(data), len);
}

/// Detected format name ("DNG", "NEF", ...), or nullopt.
inline std::optional<std::string> detect_format(const void *data, std::size_t len) {
    return detail::opt_str(flashraw_detect_format(static_cast<const std::uint8_t *>(data), len));
}

/// True when the GPU backend is compiled in and an adapter is available.
inline bool gpu_available() { return flashraw_gpu_available(); }

} // namespace flashraw

#endif // FLASHRAW_HPP
