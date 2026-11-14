/*
 * FlashRaw LibRaw-compatible API.
 *
 * Part of flashraw-libraw-compat, an independent implementation of a subset of the LibRaw
 * C API on top of FlashRaw (see docs/libraw-compat.md). Licensed under MIT OR Apache-2.0.
 * This file contains no LibRaw code.
 *
 * Programs written against the LibRaw C API (and the common parts of its C++ class) can
 * switch to FlashRaw by recompiling against this header and linking -lraw from this crate.
 * Functions outside the supported subset are declared and return LIBRAW_NOT_IMPLEMENTED.
 * Post-processing (libraw_dcraw_process) is FlashRaw's bilinear develop, not the full
 * LibRaw pipeline; see docs/libraw-compat.md for every deviation.
 *
 * Thread safety: one libraw_data_t per thread at a time (as with LibRaw); different handles
 * may be used concurrently.
 */
#ifndef FLASHRAW_LIBRAW_H
#define FLASHRAW_LIBRAW_H

#include <limits.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "libraw_const.h"
#include "libraw_types.h"
#include "libraw_version.h"

#ifdef __cplusplus
#include <new>
extern "C" {
#endif

DllDef const char *libraw_strerror(int errorcode);
DllDef const char *libraw_strprogress(enum LibRaw_progress);

/* Initialisation and loading */
DllDef libraw_data_t *libraw_init(unsigned int flags);
DllDef int libraw_open_file(libraw_data_t *, const char *);
#if defined(_WIN32) || defined(WIN32)
DllDef int libraw_open_wfile(libraw_data_t *, const wchar_t *);
#endif
DllDef int libraw_open_buffer(libraw_data_t *, const void *buffer, size_t size);
DllDef int libraw_open_bayer(libraw_data_t *lr, unsigned char *data, unsigned datalen,
                             ushort _raw_width, ushort _raw_height, ushort _left_margin,
                             ushort _top_margin, ushort _right_margin, ushort _bottom_margin,
                             unsigned char procflags, unsigned char bayer_battern,
                             unsigned unused_bits, unsigned otherflags, unsigned black_level);
DllDef int libraw_unpack(libraw_data_t *);
DllDef int libraw_unpack_thumb(libraw_data_t *);
DllDef int libraw_unpack_thumb_ex(libraw_data_t *, int);
DllDef void libraw_recycle_datastream(libraw_data_t *);
DllDef void libraw_recycle(libraw_data_t *);
DllDef void libraw_close(libraw_data_t *);
DllDef void libraw_subtract_black(libraw_data_t *);
DllDef int libraw_raw2image(libraw_data_t *);
DllDef void libraw_free_image(libraw_data_t *);

/* Version helpers */
DllDef const char *libraw_version(void);
DllDef int libraw_versionNumber(void);

/* Camera list (FlashRaw identifies cameras from file metadata; the list is empty) */
DllDef const char **libraw_cameraList(void);
DllDef int libraw_cameraCount(void);

/* Callbacks: accepted and stored, never invoked */
DllDef void default_data_callback(void *data, const char *file, const INT64 offset);
DllDef void libraw_set_exifparser_handler(libraw_data_t *, exif_parser_callback cb, void *datap);
DllDef void libraw_set_makernotes_handler(libraw_data_t *, exif_parser_callback cb, void *datap);
DllDef void libraw_set_dataerror_handler(libraw_data_t *, data_callback func, void *datap);
DllDef void libraw_set_progress_handler(libraw_data_t *, progress_callback cb, void *datap);

DllDef const char *libraw_unpack_function_name(libraw_data_t *lr);
DllDef int libraw_get_decoder_info(libraw_data_t *lr, libraw_decoder_info_t *d);
DllDef int libraw_COLOR(libraw_data_t *, int row, int col);
DllDef unsigned libraw_capabilities(void);
DllDef int libraw_adjust_to_raw_inset_crop(libraw_data_t *lr, unsigned mask, float maxcrop);

/* dcraw-style processing */
DllDef int libraw_adjust_sizes_info_only(libraw_data_t *);
DllDef int libraw_dcraw_ppm_tiff_writer(libraw_data_t *lr, const char *filename);
DllDef int libraw_dcraw_thumb_writer(libraw_data_t *lr, const char *fname);
DllDef int libraw_dcraw_process(libraw_data_t *lr);
DllDef libraw_processed_image_t *libraw_dcraw_make_mem_image(libraw_data_t *lr, int *errc);
DllDef libraw_processed_image_t *libraw_dcraw_make_mem_thumb(libraw_data_t *lr, int *errc);
DllDef void libraw_dcraw_clear_mem(libraw_processed_image_t *);

/* Parameter setters and getters */
DllDef void libraw_set_demosaic(libraw_data_t *lr, int value);
DllDef void libraw_set_output_color(libraw_data_t *lr, int value);
DllDef void libraw_set_adjust_maximum_thr(libraw_data_t *lr, float value);
DllDef void libraw_set_user_mul(libraw_data_t *lr, int index, float val);
DllDef void libraw_set_output_bps(libraw_data_t *lr, int value);
DllDef void libraw_set_gamma(libraw_data_t *lr, int index, float value);
DllDef void libraw_set_no_auto_bright(libraw_data_t *lr, int value);
DllDef void libraw_set_bright(libraw_data_t *lr, float value);
DllDef void libraw_set_highlight(libraw_data_t *lr, int value);
DllDef void libraw_set_fbdd_noiserd(libraw_data_t *lr, int value);
DllDef int libraw_get_raw_height(libraw_data_t *lr);
DllDef int libraw_get_raw_width(libraw_data_t *lr);
DllDef int libraw_get_iheight(libraw_data_t *lr);
DllDef int libraw_get_iwidth(libraw_data_t *lr);
DllDef float libraw_get_cam_mul(libraw_data_t *lr, int index);
DllDef float libraw_get_pre_mul(libraw_data_t *lr, int index);
DllDef float libraw_get_rgb_cam(libraw_data_t *lr, int index1, int index2);
DllDef int libraw_get_color_maximum(libraw_data_t *lr);
DllDef void libraw_set_output_tif(libraw_data_t *lr, int value);
DllDef libraw_iparams_t *libraw_get_iparams(libraw_data_t *lr);
DllDef libraw_lensinfo_t *libraw_get_lensinfo(libraw_data_t *lr);
DllDef libraw_imgother_t *libraw_get_imgother(libraw_data_t *lr);

/* FlashRaw extensions (not part of the LibRaw API) */

/* Size of the bitmap libraw_dcraw_make_mem_image() would return. */
DllDef void libraw_flashraw_mem_image_format(libraw_data_t *lr, int *width, int *height,
                                             int *colors, int *bps);
/* Version of the FlashRaw library underneath, e.g. "0.1.0". */
DllDef const char *libraw_flashraw_version(void);
/* Message of the last FlashRaw error (or recovered data error) on this handle; "" if none.
   Valid until the next call on the handle. */
DllDef const char *libraw_flashraw_last_error(libraw_data_t *lr);

#ifdef __cplusplus
} /* extern "C" */

/*
 * Header-only C++ class with the commonly used members of LibRaw's class. `imgdata` is a
 * reference to the C handle (member access such as `rp.imgdata.sizes.width` works
 * unchanged). Methods outside the subset return LIBRAW_NOT_IMPLEMENTED.
 */
class LibRaw {
public:
    libraw_data_t &imgdata;

    explicit LibRaw(unsigned int flags = LIBRAW_OPTIONS_NONE) : imgdata(*init_or_throw(flags)) {}
    ~LibRaw() { libraw_close(&imgdata); }
    LibRaw(const LibRaw &) = delete;
    LibRaw &operator=(const LibRaw &) = delete;

    libraw_output_params_t *output_params_ptr() { return &imgdata.params; }

    int open_file(const char *fname) { return libraw_open_file(&imgdata, fname); }
#if defined(_WIN32) || defined(WIN32)
    int open_file(const wchar_t *fname) { return libraw_open_wfile(&imgdata, fname); }
#endif
    int open_buffer(const void *buffer, size_t size) { return libraw_open_buffer(&imgdata, buffer, size); }
    int unpack() { return libraw_unpack(&imgdata); }
    int unpack_thumb() { return libraw_unpack_thumb(&imgdata); }
    int unpack_thumb_ex(int i) { return libraw_unpack_thumb_ex(&imgdata, i); }
    int error_count() { return 0; }
    void recycle_datastream() { libraw_recycle_datastream(&imgdata); }
    void recycle() { libraw_recycle(&imgdata); }
    int adjust_sizes_info_only() { return libraw_adjust_sizes_info_only(&imgdata); }
    int subtract_black() {
        libraw_subtract_black(&imgdata);
        return 0;
    }
    int raw2image() { return libraw_raw2image(&imgdata); }
    int raw2image_ex(int do_subtract_black) {
        int rc = libraw_raw2image(&imgdata);
        if (rc == LIBRAW_SUCCESS && do_subtract_black) libraw_subtract_black(&imgdata);
        return rc;
    }
    void free_image() { libraw_free_image(&imgdata); }
    int dcraw_process() { return libraw_dcraw_process(&imgdata); }
    int dcraw_ppm_tiff_writer(const char *outfile) { return libraw_dcraw_ppm_tiff_writer(&imgdata, outfile); }
    int dcraw_thumb_writer(const char *fname) { return libraw_dcraw_thumb_writer(&imgdata, fname); }
    libraw_processed_image_t *dcraw_make_mem_image(int *errcode = NULL) {
        return libraw_dcraw_make_mem_image(&imgdata, errcode);
    }
    libraw_processed_image_t *dcraw_make_mem_thumb(int *errcode = NULL) {
        return libraw_dcraw_make_mem_thumb(&imgdata, errcode);
    }
    static void dcraw_clear_mem(libraw_processed_image_t *p) { libraw_dcraw_clear_mem(p); }

    void get_mem_image_format(int *width, int *height, int *colors, int *bps) {
        libraw_flashraw_mem_image_format(&imgdata, width, height, colors, bps);
    }
    /* Copy the processed bitmap into caller memory (`stride` bytes per row; BGR order if
       `bgr` is non-zero). */
    int copy_mem_image(void *scan0, int stride, int bgr) {
        int err = 0;
        libraw_processed_image_t *img = libraw_dcraw_make_mem_image(&imgdata, &err);
        if (!img) return err ? err : LIBRAW_OUT_OF_ORDER_CALL;
        const int bytes = img->bits / 8, px = img->colors * bytes, row = img->width * px;
        if (stride < row) {
            libraw_dcraw_clear_mem(img);
            return LIBRAW_BAD_CROP;
        }
        for (int y = 0; y < img->height; y++) {
            unsigned char *dst = static_cast<unsigned char *>(scan0) + (size_t)y * stride;
            const unsigned char *src = img->data + (size_t)y * row;
            if (!bgr || img->colors < 3) {
                memcpy(dst, src, row);
            } else {
                for (int x = 0; x < img->width; x++)
                    for (int c = 0; c < 3; c++)
                        memcpy(dst + x * px + (2 - c) * bytes, src + x * px + c * bytes, bytes);
            }
        }
        libraw_dcraw_clear_mem(img);
        return LIBRAW_SUCCESS;
    }

    int COLOR(int row, int col) { return libraw_COLOR(&imgdata, row, col); }
    int get_decoder_info(libraw_decoder_info_t *d) { return libraw_get_decoder_info(&imgdata, d); }
    const char *unpack_function_name() { return libraw_unpack_function_name(&imgdata); }
    int adjust_to_raw_inset_crop(unsigned mask, float maxcrop = 0.55f) {
        return libraw_adjust_to_raw_inset_crop(&imgdata, mask, maxcrop);
    }
    void set_exifparser_handler(exif_parser_callback cb, void *data) {
        libraw_set_exifparser_handler(&imgdata, cb, data);
    }
    void set_makernotes_handler(exif_parser_callback cb, void *data) {
        libraw_set_makernotes_handler(&imgdata, cb, data);
    }
    void set_dataerror_handler(data_callback func, void *data) { libraw_set_dataerror_handler(&imgdata, func, data); }
    void set_progress_handler(progress_callback cb, void *data) { libraw_set_progress_handler(&imgdata, cb, data); }

    static const char *version() { return libraw_version(); }
    static int versionNumber() { return libraw_versionNumber(); }
    static const char **cameraList() { return libraw_cameraList(); }
    static int cameraCount() { return libraw_cameraCount(); }
    static const char *strprogress(enum LibRaw_progress p) { return libraw_strprogress(p); }
    static const char *strerror(int e) { return libraw_strerror(e); }
    static unsigned capabilities() { return libraw_capabilities(); }

private:
    static libraw_data_t *init_or_throw(unsigned int flags) {
        libraw_data_t *d = libraw_init(flags);
        if (!d) throw std::bad_alloc();
        return d;
    }
};
#endif /* __cplusplus */

#endif /* FLASHRAW_LIBRAW_H */
