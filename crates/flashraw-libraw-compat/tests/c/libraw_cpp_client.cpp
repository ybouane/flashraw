// A typical C++ client of the LibRaw class interface: open, unpack, process, write to memory.
//
//   libraw_cpp_client FILE
#include <libraw/libraw.h>

#include <cstdio>
#include <vector>

int main(int argc, char **argv) {
    if (argc < 2) {
        std::fprintf(stderr, "usage: %s FILE\n", argv[0]);
        return 1;
    }
    LibRaw rp;
    rp.imgdata.params.use_camera_wb = 1;
    rp.imgdata.params.half_size = 1;
    int rc = rp.open_file(argv[1]);
    if (rc != LIBRAW_SUCCESS) {
        std::fprintf(stderr, "open: %s\n", LibRaw::strerror(rc));
        return 2;
    }
    std::printf("camera=%s %s\n", rp.imgdata.idata.make, rp.imgdata.idata.model);
    if ((rc = rp.unpack()) != LIBRAW_SUCCESS || (rc = rp.dcraw_process()) != LIBRAW_SUCCESS) {
        std::fprintf(stderr, "decode: %s\n", LibRaw::strerror(rc));
        return 2;
    }
    int w = 0, h = 0, colors = 0, bps = 0;
    rp.get_mem_image_format(&w, &h, &colors, &bps);
    std::printf("output=%dx%d colors=%d bps=%d\n", w, h, colors, bps);
    std::vector<unsigned char> bgr(static_cast<size_t>(w) * h * colors * (bps / 8));
    rc = rp.copy_mem_image(bgr.data(), w * colors * (bps / 8), 1);
    if (rc != LIBRAW_SUCCESS) return 2;
    libraw_processed_image_t *img = rp.dcraw_make_mem_image(&rc);
    if (!img || img->width != w || img->height != h) return 2;
    // copy_mem_image with bgr=1 swaps red and blue.
    if (colors == 3 && (bgr[0] != img->data[2] || bgr[2] != img->data[0])) return 3;
    LibRaw::dcraw_clear_mem(img);
    std::printf("version=%s ok\n", LibRaw::version());
    return 0;
}
