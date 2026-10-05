# flashraw-libraw-compat

A LibRaw-compatible C API implemented on [FlashRaw](../../README.md): programs written against
the LibRaw C API (or the common members of its `LibRaw` C++ class) switch to FlashRaw by
recompiling against `include/libraw/libraw.h` from this crate and linking `-lraw` from
`cargo build --release -p flashraw-libraw-compat`.

```c
#include <libraw/libraw.h>

libraw_data_t *lr = libraw_init(0);
if (libraw_open_file(lr, "photo.NEF") == LIBRAW_SUCCESS && libraw_unpack(lr) == LIBRAW_SUCCESS) {
    printf("%s %s, black %u, white %u\n", lr->idata.make, lr->idata.model,
           lr->color.black, lr->color.maximum);
    lr->params.use_camera_wb = 1;
    if (libraw_dcraw_process(lr) == LIBRAW_SUCCESS) {
        int err;
        libraw_processed_image_t *img = libraw_dcraw_make_mem_image(lr, &err);
        /* img->width x img->height x img->colors, img->bits per sample */
        libraw_dcraw_clear_mem(img);
    }
}
libraw_close(lr);
```

It is source compatible (same names, types and, for every structure but `libraw_data_t`, the
same layout), not binary compatible: rebuild the program. The supported subset, every
deviation from LibRaw's behaviour (processing is FlashRaw's bilinear develop, not LibRaw's full
pipeline), the verification results and the licensing notes are in
[docs/libraw-compat.md](../../docs/libraw-compat.md).

This crate contains no LibRaw code and is licensed MIT OR Apache-2.0 like the rest of FlashRaw.
