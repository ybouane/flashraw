/*
 * A typical LibRaw C-API client: open -> metadata -> unpack -> raw statistics ->
 * dcraw_process -> make_mem_image -> thumbnail. Prints one key=value line per item so the
 * output of two builds can be compared line by line.
 *
 *   libraw_client [--no-process] FILE...
 *
 * Uses only the LibRaw C API and data structures (no FlashRaw extensions), so the same
 * source builds against any implementation of that API. Exit status: number of files whose
 * open or unpack failed (capped at 100).
 */
#include <libraw/libraw.h>

#include <stdint.h>
#include <stdio.h>
#include <string.h>

static uint64_t fnv(uint64_t h, const void *p, size_t n) {
    const unsigned char *b = (const unsigned char *)p;
    for (size_t i = 0; i < n; i++) {
        h ^= b[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

static void identify(const libraw_data_t *d) {
    const libraw_image_sizes_t *s = &d->sizes;
    const libraw_iparams_t *p = &d->idata;
    const libraw_colordata_t *c = &d->color;
    const libraw_imgother_t *o = &d->other;
    printf("make=%s\nmodel=%s\n", p->make, p->model);
    printf("colors=%d\nfilters=%08x\ncdesc=%s\n", p->colors, p->filters, p->cdesc);
    printf("raw_size=%ux%u\n", s->raw_width, s->raw_height);
    printf("visible=%ux%u+%u+%u\n", s->width, s->height, s->left_margin, s->top_margin);
    printf("isize=%ux%u\n", s->iwidth, s->iheight);
    printf("flip=%d\n", s->flip);
    printf("inset_crop0=%ux%u+%u+%u\n", s->raw_inset_crops[0].cwidth, s->raw_inset_crops[0].cheight,
           s->raw_inset_crops[0].cleft, s->raw_inset_crops[0].ctop);
    printf("open.black=%u\nopen.cblack=%u %u %u %u / %ux%u\n", c->black, c->cblack[0], c->cblack[1], c->cblack[2],
           c->cblack[3], c->cblack[4], c->cblack[5]);
    printf("open.maximum=%u\n", c->maximum);
    printf("cam_mul=%.4f %.4f %.4f %.4f\n", c->cam_mul[0], c->cam_mul[1], c->cam_mul[2], c->cam_mul[3]);
    printf("pre_mul=%.4f %.4f %.4f %.4f\n", c->pre_mul[0], c->pre_mul[1], c->pre_mul[2], c->pre_mul[3]);
    for (int i = 0; i < 3; i++)
        printf("rgb_cam[%d]=%.4f %.4f %.4f\n", i, c->rgb_cam[i][0], c->rgb_cam[i][1], c->rgb_cam[i][2]);
    for (int i = 0; i < 3; i++)
        printf("cam_xyz[%d]=%.4f %.4f %.4f\n", i, c->cam_xyz[i][0], c->cam_xyz[i][1], c->cam_xyz[i][2]);
    printf("iso=%.0f\nshutter=%.6f\naperture=%.1f\nfocal_len=%.1f\n", o->iso_speed, o->shutter, o->aperture,
           o->focal_len);
    printf("timestamp=%lld\n", (long long)o->timestamp);
    printf("thumb=%d %ux%u %u\n", (int)d->thumbnail.tformat, d->thumbnail.twidth, d->thumbnail.theight,
           d->thumbnail.tlength);
}

static void raw_stats(libraw_data_t *d) {
    const libraw_image_sizes_t *s = &d->sizes;
    const libraw_colordata_t *c = &d->color;
    printf("black=%u\ncblack=%u %u %u %u / %ux%u\n", c->black, c->cblack[0], c->cblack[1], c->cblack[2],
           c->cblack[3], c->cblack[4], c->cblack[5]);
    printf("maximum=%u\n", c->maximum);
    /* Effective black of the first 2x2 visible sites: black + per-colour + pattern part. */
    printf("eff_black=");
    for (int y = 0; y < 2; y++)
        for (int x = 0; x < 2; x++) {
            unsigned b = c->black + c->cblack[libraw_COLOR(d, y, x) & 3];
            if (c->cblack[4] && c->cblack[5]) b += c->cblack[6 + (y % c->cblack[4]) * c->cblack[5] + x % c->cblack[5]];
            printf(y + x ? " %u" : "%u", b);
        }
    printf("\n");
    printf("layout=%s\n", d->rawdata.raw_image ? "raw_image" : d->rawdata.color4_image ? "color4_image"
                                                     : d->rawdata.color3_image ? "color3_image"
                                                     : d->rawdata.float_image ? "float_image" : "none");
    if (d->rawdata.raw_image) {
        const ushort *r = d->rawdata.raw_image;
        const size_t pitch = s->raw_pitch / 2;
        uint64_t h = 0xcbf29ce484222325ULL;
        for (size_t y = 0; y < s->raw_height; y++) h = fnv(h, r + y * pitch, (size_t)s->raw_width * 2);
        printf("raw_hash=%016llx\n", (unsigned long long)h);
        unsigned mn = 65535, mx = 0;
        double sum[4] = {0, 0, 0, 0}, n[4] = {0, 0, 0, 0};
        for (int y = 0; y < s->height; y++)
            for (int x = 0; x < s->width; x++) {
                unsigned v = r[(size_t)(y + s->top_margin) * pitch + x + s->left_margin];
                int ci = libraw_COLOR(d, y, x) & 3;
                mn = v < mn ? v : mn;
                mx = v > mx ? v : mx;
                sum[ci] += v;
                n[ci] += 1;
            }
        printf("visible_min=%u\nvisible_max=%u\n", mn, mx);
        printf("visible_mean=%.1f %.1f %.1f %.1f\n", n[0] ? sum[0] / n[0] : 0, n[1] ? sum[1] / n[1] : 0,
               n[2] ? sum[2] / n[2] : 0, n[3] ? sum[3] / n[3] : 0);
        printf("color00=%d%d%d%d\n", libraw_COLOR(d, 0, 0), libraw_COLOR(d, 0, 1), libraw_COLOR(d, 1, 0),
               libraw_COLOR(d, 1, 1));
    } else if (d->rawdata.color4_image) {
        uint64_t h = 0xcbf29ce484222325ULL;
        const size_t pitch = s->raw_pitch / 8;
        for (size_t y = 0; y < s->raw_height; y++)
            h = fnv(h, d->rawdata.color4_image + y * pitch, (size_t)s->raw_width * 8);
        printf("color4_hash=%016llx\n", (unsigned long long)h);
    } else if (d->rawdata.color3_image) {
        uint64_t h = 0xcbf29ce484222325ULL;
        const size_t pitch = s->raw_pitch / 6;
        for (size_t y = 0; y < s->raw_height; y++)
            h = fnv(h, d->rawdata.color3_image + y * pitch, (size_t)s->raw_width * 6);
        printf("color3_hash=%016llx\n", (unsigned long long)h);
    }
}

static void process(libraw_data_t *d) {
    d->params.use_camera_wb = 1;
    int rc = libraw_dcraw_process(d);
    printf("process=%d\n", rc);
    if (rc != LIBRAW_SUCCESS) return;
    printf("processed_size=%dx%d\n", libraw_get_iwidth(d), libraw_get_iheight(d));
    printf("data_maximum=%u\n", d->color.data_maximum);
    int err = 0;
    libraw_processed_image_t *img = libraw_dcraw_make_mem_image(d, &err);
    printf("mem_image=%d\n", err);
    if (!img) return;
    double sum[3] = {0, 0, 0};
    size_t npx = (size_t)img->width * img->height;
    for (size_t i = 0; i < npx; i++)
        for (int c = 0; c < img->colors && c < 3; c++) sum[c] += img->data[i * img->colors + c];
    printf("mem_image_format=%d %ux%u colors=%u bits=%u size=%u\n", (int)img->type, img->width, img->height,
           img->colors, img->bits, img->data_size);
    printf("mem_image_mean=%.1f %.1f %.1f\n", npx ? sum[0] / npx : 0, npx ? sum[1] / npx : 0,
           npx ? sum[2] / npx : 0);
    libraw_dcraw_clear_mem(img);
}

static void thumbnail(libraw_data_t *d) {
    int rc = libraw_unpack_thumb(d);
    printf("unpack_thumb=%d\n", rc);
    if (rc != LIBRAW_SUCCESS) return;
    int err = 0;
    libraw_processed_image_t *t = libraw_dcraw_make_mem_thumb(d, &err);
    if (!t) {
        printf("mem_thumb=%d\n", err);
        return;
    }
    printf("mem_thumb=%d type=%d size=%u", err, (int)t->type, t->data_size);
    if (t->type == LIBRAW_IMAGE_JPEG && t->data_size > 2) printf(" soi=%02x%02x", t->data[0], t->data[1]);
    printf("\n");
    libraw_dcraw_clear_mem(t);
}

int main(int argc, char **argv) {
    int do_process = 1, failures = 0;
    printf("version=%s\n", libraw_version());
    if (libraw_versionNumber() < LIBRAW_MAKE_VERSION(0, 20, 0)) {
        fprintf(stderr, "LibRaw API older than 0.20\n");
        return 100;
    }
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--no-process")) {
            do_process = 0;
            continue;
        }
        libraw_data_t *d = libraw_init(0);
        if (!d) return 100;
        printf("file=%s\n", argv[i]);
        int rc = libraw_open_file(d, argv[i]);
        printf("open=%d\n", rc);
        if (rc != LIBRAW_SUCCESS) {
            printf("error=%s\n", libraw_strerror(rc));
            failures++;
            libraw_close(d);
            continue;
        }
        identify(d);
        rc = libraw_unpack(d);
        printf("unpack=%d\n", rc);
        if (rc != LIBRAW_SUCCESS) {
            printf("error=%s\n", libraw_strerror(rc));
            failures++;
            libraw_close(d);
            continue;
        }
        raw_stats(d);
        if (do_process) process(d);
        thumbnail(d);
        libraw_close(d);
    }
    return failures > 100 ? 100 : failures;
}
