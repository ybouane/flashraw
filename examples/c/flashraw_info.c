/*
 * flashraw_info: print metadata of a RAW file, decode its sensor data and (optionally)
 * write a display-ready PPM. Exercises the main parts of the FlashRaw C API.
 *
 *   flashraw_info photo.NEF [out.ppm]
 *
 * Exit status: 0 on success, 1 on usage errors, 2 when FlashRaw reports an error.
 */
#include <flashraw.h>

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int fail(const char *what) {
    fprintf(stderr, "%s failed: [%s] %s\n", what, flashraw_last_error_kind(), flashraw_last_error_message());
    return 2;
}

static const char *or_dash(const char *s) { return s ? s : "-"; }

static void print_metadata(const flashraw_file *f) {
    printf("format:      %s\n", or_dash(flashraw_file_format_name(f)));
    printf("camera:      %s (make \"%s\", model \"%s\")\n", or_dash(flashraw_file_string(f, FLASHRAW_STRING_CAMERA)),
           or_dash(flashraw_file_make(f)), or_dash(flashraw_file_model(f)));
    printf("sensor:      %ux%u, %u sample(s)/pixel, %u bits\n", flashraw_file_raw_width(f), flashraw_file_raw_height(f),
           flashraw_file_cpp(f), flashraw_file_bits_per_sample(f));

    flashraw_rect a, c;
    if (flashraw_file_active_area(f, &a) == FLASHRAW_OK && flashraw_file_crop(f, &c) == FLASHRAW_OK)
        printf("active area: %ux%u+%u+%u, crop %ux%u+%u+%u\n", a.width, a.height, a.x, a.y, c.width, c.height, c.x, c.y);

    flashraw_cfa cfa;
    if (flashraw_file_cfa(f, &cfa)) {
        static const char letters[] = "RGBCMYWE";
        printf("cfa:         %ux%u ", cfa.width, cfa.height);
        for (uint32_t i = 0; i < cfa.width * cfa.height; i++) putchar(letters[cfa.colors[i] & 7]);
        putchar('\n');
    }

    float black[64];
    flashraw_black_level bl;
    size_t nb = flashraw_file_black_level(f, &bl, black, 64);
    printf("black:       %ux%u pattern, %zu value(s), mean %.1f\n", bl.rows, bl.cols, nb, bl.mean);
    printf("white:       %u\n", flashraw_file_white_level_max(f));

    float wb[4];
    if (flashraw_file_white_balance(f, wb))
        printf("wb:          %.3f %.3f %.3f %.3f%s\n", wb[0], wb[1], wb[2], wb[3],
               flashraw_file_white_balance_applied(f) ? " (applied by the camera)" : "");
    if (flashraw_file_color_matrix_count(f, FLASHRAW_MATRIX_COLOR) > 0) {
        flashraw_color_matrix m;
        if (flashraw_file_color_matrix(f, FLASHRAW_MATRIX_COLOR, 0, &m) == FLASHRAW_OK)
            printf("matrix:      %ux%u, illuminant %u, first row %.4f %.4f %.4f\n", m.rows, m.cols, m.illuminant,
                   m.values[0], m.values[1], m.values[2]);
    }

    uint32_t iso;
    double t;
    float fnum;
    if (flashraw_file_iso(f, &iso)) printf("iso:         %u\n", iso);
    if (flashraw_file_exposure_time(f, &t)) printf("exposure:    %g s\n", t);
    if (flashraw_file_f_number(f, &fnum)) printf("aperture:    f/%.1f\n", fnum);
    printf("lens:        %s\n", or_dash(flashraw_file_string(f, FLASHRAW_STRING_LENS_MODEL)));
    printf("captured:    %s\n", or_dash(flashraw_file_string(f, FLASHRAW_STRING_CAPTURE_TIME)));
    printf("orientation: %u\n", flashraw_file_orientation(f));

    size_t np = flashraw_file_preview_count(f);
    for (size_t i = 0; i < np; i++) {
        flashraw_preview_info p;
        p.size = sizeof p;
        if (flashraw_file_preview_info(f, i, &p) != FLASHRAW_OK) continue;
        size_t len = 0;
        const uint8_t *bytes = flashraw_file_preview_bytes(f, i, &len);
        printf("preview %zu:   %ux%u, encoding %d, %" PRIu64 " bytes%s\n", i, p.width, p.height, (int)p.encoding,
               p.length, bytes && len >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 ? " (JPEG)" : "");
    }
}

static int decode_raw(const flashraw_file *f) {
    flashraw_decode_options o;
    flashraw_decode_options_init(&o); /* raw 16-bit samples, all cores */
    flashraw_frame *fr = NULL;
    if (flashraw_decode(f, &o, &fr) != FLASHRAW_OK) return fail("raw decode");

    uint32_t w = flashraw_frame_width(fr), h = flashraw_frame_height(fr), cpp = flashraw_frame_cpp(fr);
    const uint16_t *px = (const uint16_t *)flashraw_frame_data(fr);
    flashraw_rect a;
    flashraw_frame_active_area(fr, &a);
    uint16_t mn = UINT16_MAX, mx = 0;
    double sum = 0;
    for (uint32_t y = a.y; y < a.y + a.height; y++)
        for (uint32_t x = a.x * cpp; x < (a.x + a.width) * cpp; x++) {
            uint16_t v = px[(size_t)y * w * cpp + x];
            mn = v < mn ? v : mn;
            mx = v > mx ? v : mx;
            sum += v;
        }
    double n = (double)a.width * a.height * cpp;
    flashraw_decode_stats st;
    st.size = sizeof st;
    flashraw_frame_stats(fr, &st);
    printf("raw:         %ux%ux%u u16 (%zu bytes) via %s in %.1f ms\n", w, h, cpp, flashraw_frame_data_len(fr),
           flashraw_frame_backend(fr), st.total_ms);
    printf("active:      min %u max %u mean %.1f\n", mn, mx, n > 0 ? sum / n : 0.0);
    if (flashraw_frame_is_incomplete(fr))
        for (size_t i = 0; i < flashraw_frame_warning_count(fr); i++)
            printf("warning:     %s\n", flashraw_frame_warning(fr, i));
    flashraw_frame_free(fr);
    return 0;
}

static int write_ppm(const flashraw_file *f, const char *path) {
    flashraw_decode_options o;
    flashraw_decode_options_init(&o);
    o.output = FLASHRAW_OUTPUT_RGBA8; /* demosaiced, white-balanced sRGB, oriented */
    flashraw_frame *fr = NULL;
    if (flashraw_decode(f, &o, &fr) != FLASHRAW_OK) return fail("rgba8 decode");
    uint32_t w = flashraw_frame_width(fr), h = flashraw_frame_height(fr);
    const uint8_t *rgba = (const uint8_t *)flashraw_frame_data(fr);
    FILE *out = fopen(path, "wb");
    if (!out) {
        perror(path);
        flashraw_frame_free(fr);
        return 2;
    }
    fprintf(out, "P6\n%u %u\n255\n", w, h);
    for (size_t i = 0; i < (size_t)w * h; i++) fwrite(rgba + i * 4, 1, 3, out);
    fclose(out);
    printf("wrote:       %s (%ux%u)\n", path, w, h);
    flashraw_frame_free(fr);
    return 0;
}

int main(int argc, char **argv) {
    printf("FlashRaw %s (ABI %u)\n", flashraw_version(), flashraw_abi_version());
    if (flashraw_abi_version() != FLASHRAW_ABI_VERSION) {
        fprintf(stderr, "ABI mismatch: header %d, library %u\n", FLASHRAW_ABI_VERSION, flashraw_abi_version());
        return 2;
    }
    if (argc < 2) {
        fprintf(stderr, "usage: %s FILE [OUT.ppm]\n", argv[0]);
        return 1;
    }
    flashraw_file *f = NULL;
    if (flashraw_open_path(argv[1], NULL, &f) != FLASHRAW_OK) return fail("open");
    print_metadata(f);

    char *json = flashraw_file_metadata_json(f);
    if (json) {
        printf("json:        %zu bytes of metadata\n", strlen(json));
        flashraw_string_free(json);
    }
    int rc = decode_raw(f);
    if (rc == 0 && argc > 2) rc = write_ppm(f, argv[2]);
    flashraw_file_free(f);
    return rc;
}
