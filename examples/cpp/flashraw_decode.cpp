// flashraw_decode: the C++ wrapper in action. Prints metadata, decodes the sensor data on
// several threads at once (one file, independent frames), and optionally writes an 8-bit
// PPM and the largest embedded preview.
//
//   flashraw_decode photo.CR3 [out.ppm [preview.jpg]]
//
// Exit status: 0 on success, 1 on usage errors, 2 when FlashRaw reports an error.

#include <flashraw.hpp>

#include <algorithm>
#include <cstdio>
#include <fstream>
#include <future>
#include <iostream>
#include <vector>

int main(int argc, char **argv) {
    std::cout << "FlashRaw " << flashraw::version() << "\n";
    if (argc < 2) {
        std::cerr << "usage: " << argv[0] << " FILE [OUT.ppm [PREVIEW]]\n";
        return 1;
    }
    try {
        const flashraw::file raw = flashraw::file::open(argv[1]);
        std::cout << raw.format_name() << " " << raw.camera() << ", sensor " << raw.raw_width() << "x"
                  << raw.raw_height() << ", " << raw.bits_per_sample() << " bits\n";
        if (auto cfa = raw.cfa()) std::cout << "CFA " << cfa->letters() << "\n";
        auto black = raw.black_level();
        std::cout << "black mean " << black.mean << ", white " << raw.white_level().front() << "\n";
        if (auto iso = raw.iso()) std::cout << "ISO " << *iso << "\n";
        if (auto t = raw.exposure_time()) std::cout << "exposure " << *t << " s\n";
        if (auto wb = raw.white_balance())
            std::cout << "WB " << (*wb)[0] << " " << (*wb)[1] << " " << (*wb)[2] << "\n";
        for (const auto &m : raw.color_matrices())
            std::cout << "colour matrix " << m.rows << "x" << m.cols << " (illuminant " << m.illuminant << ")\n";
        for (const auto &p : raw.previews())
            std::cout << "preview " << p.index << ": " << p.width << "x" << p.height << ", " << p.length << " bytes\n";

        // A const file may be decoded from several threads at once.
        std::vector<std::future<flashraw::frame>> jobs;
        for (unsigned i = 0; i < 2; ++i)
            jobs.push_back(std::async(std::launch::async, [&raw, i] {
                flashraw::decode_options o;
                o.threads = 1 + i;
                return raw.decode(o);
            }));
        std::vector<flashraw::frame> frames;
        for (auto &j : jobs) frames.push_back(j.get());
        auto a = frames[0].u16();
        auto b = frames[1].u16();
        if (a.size() != b.size() || !std::equal(a.begin(), a.end(), b.begin())) {
            std::cerr << "concurrent decodes differ\n";
            return 2;
        }
        const flashraw::frame &f = frames[0];
        auto area = f.active_area();
        std::uint16_t mx = 0;
        for (std::uint32_t y = area.y; y < area.y + area.height; ++y) {
            auto row = a.subspan((std::size_t(y) * f.width() + area.x) * f.cpp(), std::size_t(area.width) * f.cpp());
            mx = std::max(mx, *std::max_element(row.begin(), row.end()));
        }
        std::cout << "raw " << f.width() << "x" << f.height() << "x" << f.cpp() << " via " << f.backend()
                  << ", active-area max " << mx << ", " << f.stats().total_ms << " ms\n";

        // Wrong element type: a typed exception, not undefined behaviour.
        try {
            (void)f.f32();
            std::cerr << "expected an exception\n";
            return 2;
        } catch (const flashraw::invalid_argument_error &) {
        }

        if (argc > 2) {
            flashraw::decode_options o;
            o.output = flashraw::output_format::rgba8;
            flashraw::frame img = raw.decode(o);
            std::ofstream out(argv[2], std::ios::binary);
            out << "P6\n" << img.width() << " " << img.height() << "\n255\n";
            auto px = img.u8();
            for (std::size_t i = 0; i < px.size(); i += 4) out.write(reinterpret_cast<const char *>(&px[i]), 3);
            std::cout << "wrote " << argv[2] << " (" << img.width() << "x" << img.height() << ")\n";
        }
        if (argc > 3) {
            if (auto i = raw.largest_preview()) {
                auto bytes = raw.preview_bytes(*i);
                std::ofstream(argv[3], std::ios::binary)
                    .write(reinterpret_cast<const char *>(bytes.data()), std::streamsize(bytes.size()));
                std::cout << "wrote " << argv[3] << " (" << bytes.size() << " bytes)\n";
            }
        }
    } catch (const flashraw::error &e) {
        std::cerr << "flashraw error [" << e.kind() << "]: " << e.what() << "\n";
        return 2;
    }

    // Errors map to typed exceptions.
    try {
        const char junk[] = "not a raw file";
        flashraw::file::open_memory(junk, sizeof junk);
        std::cerr << "expected an exception\n";
        return 2;
    } catch (const flashraw::unsupported_format_error &e) {
        std::cout << "junk input rejected: " << e.kind() << "\n";
    }
    return 0;
}
