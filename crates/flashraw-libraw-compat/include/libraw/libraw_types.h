/*
 * FlashRaw LibRaw-compatible API: data structures.
 *
 * Part of flashraw-libraw-compat, an independent implementation of a subset of the LibRaw
 * C API on top of FlashRaw (see docs/libraw-compat.md). Licensed under MIT OR Apache-2.0.
 * This file contains no LibRaw code.
 *
 * SOURCE compatibility only: the structures below use the published member names and
 * types, so client code compiles unchanged, but libraw_data_t omits the vendor maker-note
 * block (`makernotes`) and is therefore NOT layout-compatible with LibRaw's. Rebuild your
 * program against these headers; never load this library into a binary compiled against
 * LibRaw's own headers.
 *
 * The Rust side (crates/flashraw-libraw-compat/src/types.rs) mirrors every structure here;
 * a test compiles this header and checks sizes and offsets on each platform.
 */
#ifndef FLASHRAW_LIBRAW_TYPES_H
#define FLASHRAW_LIBRAW_TYPES_H

#include <stddef.h>
#include <sys/types.h>
#include <time.h>

#include "libraw_const.h"
#include "libraw_version.h"

#ifdef __cplusplus
extern "C" {
#endif

#ifdef _WIN32
typedef __int64 INT64;
typedef unsigned __int64 UINT64;
#else
typedef long long INT64;
typedef unsigned long long UINT64;
#endif

typedef unsigned char uchar;
typedef unsigned short ushort;

#ifndef DllDef
#define DllDef
#endif

typedef struct {
    const char *decoder_name;
    unsigned decoder_flags;
} libraw_decoder_info_t;

typedef struct {
    unsigned mix_green;
    unsigned raw_color;
    unsigned zero_is_bad;
    ushort shrink;
    ushort fuji_width;
} libraw_internal_output_params_t;

/* Callback types. The set_*_handler functions accept and store these; this implementation
   never invokes them (see docs/libraw-compat.md). */
typedef void (*memory_callback)(void *data, const char *file, const char *where);
typedef void (*exif_parser_callback)(void *context, int tag, int type, int len, unsigned int ord,
                                     void *ifp, INT64 base);
typedef void (*data_callback)(void *data, const char *file, const INT64 offset);
typedef int (*progress_callback)(void *data, enum LibRaw_progress stage, int iteration,
                                 int expected);

/* Result of libraw_dcraw_make_mem_image() / libraw_dcraw_make_mem_thumb(). */
typedef struct {
    enum LibRaw_image_formats type;
    ushort height, width, colors, bits;
    unsigned int data_size;
    unsigned char data[1];
} libraw_processed_image_t;

typedef struct {
    char guard[4];
    char make[64];
    char model[64];
    char software[64];
    char normalized_make[64];
    char normalized_model[64];
    unsigned maker_index;
    unsigned raw_count;
    unsigned dng_version;
    unsigned is_foveon;
    int colors;
    unsigned filters;
    char xtrans[6][6];
    char xtrans_abs[6][6];
    char cdesc[5];
    unsigned xmplen;
    char *xmpdata;
} libraw_iparams_t;

typedef struct {
    ushort cleft, ctop, cwidth, cheight;
} libraw_raw_inset_crop_t;

typedef struct {
    ushort raw_height, raw_width, height, width, top_margin, left_margin;
    ushort iheight, iwidth;
    unsigned raw_pitch;
    double pixel_aspect;
    int flip;
    int mask[8][4];
    ushort raw_aspect;
    libraw_raw_inset_crop_t raw_inset_crops[2];
} libraw_image_sizes_t;

typedef struct {
    short t, l, b, r;
} libraw_area_t;

struct ph1_t {
    int format, key_off, tag_21a;
    int t_black, split_col, black_col, split_row, black_row;
    float tag_210;
};

typedef struct {
    unsigned parsedfields;
    ushort illuminant;
    float calibration[4][4];
    float colormatrix[4][3];
    float forwardmatrix[3][4];
} libraw_dng_color_t;

typedef struct {
    unsigned len;
    void *data;
} libraw_dng_rawopcode_t;

typedef struct {
    unsigned parsedfields;
    unsigned dng_cblack[LIBRAW_CBLACK_SIZE];
    unsigned dng_black;
    float dng_fcblack[LIBRAW_CBLACK_SIZE];
    float dng_fblack;
    unsigned dng_whitelevel[4];
    ushort default_crop[4];
    float user_crop[4];
    unsigned preview_colorspace;
    float analogbalance[4];
    float asshotneutral[4];
    float baseline_exposure;
    float LinearResponseLimit;
    libraw_dng_rawopcode_t rawopcodes[3];
} libraw_dng_levels_t;

typedef struct {
    float romm_cam[9];
} libraw_P1_color_t;

typedef struct {
    ushort curve[0x10000];
    unsigned cblack[LIBRAW_CBLACK_SIZE];
    unsigned black;
    unsigned data_maximum;
    unsigned maximum;
    unsigned linear_max[4];
    float fmaximum;
    float fnorm;
    ushort white[8][8];
    float cam_mul[4];
    float pre_mul[4];
    float cmatrix[3][4];
    float ccm[3][4];
    float rgb_cam[3][4];
    float cam_xyz[4][3];
    struct ph1_t phase_one_data;
    float flash_used;
    float canon_ev;
    char model2[64];
    char UniqueCameraModel[64];
    char LocalizedCameraModel[64];
    char ImageUniqueID[64];
    char RawDataUniqueID[17];
    char OriginalRawFileName[64];
    void *profile;
    unsigned profile_length;
    unsigned black_stat[8];
    libraw_dng_color_t dng_color[2];
    libraw_dng_levels_t dng_levels;
    int WB_Coeffs[256][4];
    float WBCT_Coeffs[64][5];
    int as_shot_wb_applied;
    libraw_P1_color_t P1_color[2];
    unsigned raw_bps;
    int ExifColorSpace;
} libraw_colordata_t;

typedef struct {
    enum LibRaw_thumbnail_formats tformat;
    ushort twidth, theight;
    unsigned tlength;
    int tcolors;
    char *thumb;
} libraw_thumbnail_t;

typedef struct {
    enum LibRaw_internal_thumbnail_formats tformat;
    ushort twidth, theight, tflip;
    unsigned tlength;
    unsigned tmisc;
    INT64 toffset;
} libraw_thumbnail_item_t;

typedef struct {
    int thumbcount;
    libraw_thumbnail_item_t thumblist[LIBRAW_THUMBNAIL_MAXCOUNT];
} libraw_thumbnail_list_t;

typedef struct {
    float latitude[3];
    float longitude[3];
    float gpstimestamp[3];
    float altitude;
    char altref, latref, longref, gpsstatus;
    char gpsparsed;
} libraw_gps_info_t;

typedef struct {
    float iso_speed;
    float shutter;
    float aperture;
    float focal_len;
    time_t timestamp;
    unsigned shot_order;
    unsigned gpsdata[32];
    libraw_gps_info_t parsed_gps;
    char desc[512], artist[64];
    float analogbalance[4];
} libraw_imgother_t;

typedef struct {
    unsigned greybox[4];
    unsigned cropbox[4];
    double aber[4];
    double gamm[6];
    float user_mul[4];
    float bright;
    float threshold;
    int half_size;
    int four_color_rgb;
    int highlight;
    int use_auto_wb;
    int use_camera_wb;
    int use_camera_matrix;
    int output_color;
    char *output_profile;
    char *camera_profile;
    char *bad_pixels;
    char *dark_frame;
    int output_bps;
    int output_tiff;
    int output_flags;
    int user_flip;
    int user_qual;
    int user_black;
    int user_cblack[4];
    int user_sat;
    int med_passes;
    float auto_bright_thr;
    float adjust_maximum_thr;
    int no_auto_bright;
    int use_fuji_rotate;
    int use_p1_correction;
    int green_matching;
    int dcb_iterations;
    int dcb_enhance_fl;
    int fbdd_noiserd;
    int exp_correc;
    float exp_shift;
    float exp_preser;
    int no_auto_scale;
    int no_interpolation;
} libraw_output_params_t;

typedef struct {
    int use_rawspeed;
    int use_dngsdk;
    unsigned options;
    unsigned shot_select;
    unsigned specials;
    unsigned max_raw_memory_mb;
    int sony_arw2_posterization_thr;
    float coolscan_nef_gamma;
    char p4shot_order[5];
    char **custom_camera_strings;
} libraw_raw_unpack_params_t;

typedef struct {
    void *raw_alloc;
    ushort *raw_image;
    ushort (*color4_image)[4];
    ushort (*color3_image)[3];
    float *float_image;
    float (*float3_image)[3];
    float (*float4_image)[4];
    short (*ph1_cblack)[2];
    short (*ph1_rblack)[2];
    libraw_iparams_t iparams;
    libraw_image_sizes_t sizes;
    libraw_internal_output_params_t ioparams;
    libraw_colordata_t color;
} libraw_rawdata_t;

typedef struct {
    UINT64 LensID;
    char Lens[128];
    ushort LensFormat;
    ushort LensMount;
    UINT64 CamID;
    ushort CameraFormat;
    ushort CameraMount;
    char body[64];
    short FocalType;
    char LensFeatures_pre[16], LensFeatures_suf[16];
    float MinFocal, MaxFocal;
    float MaxAp4MinFocal, MaxAp4MaxFocal, MinAp4MinFocal, MinAp4MaxFocal;
    float MaxAp, MinAp;
    float CurFocal, CurAp;
    float MaxAp4CurFocal, MinAp4CurFocal;
    float MinFocusDistance;
    float FocusRangeIndex;
    float LensFStops;
    UINT64 TeleconverterID;
    char Teleconverter[128];
    UINT64 AdapterID;
    char Adapter[128];
    UINT64 AttachmentID;
    char Attachment[128];
    ushort FocalUnits;
    float FocalLengthIn35mmFormat;
} libraw_makernotes_lens_t;

typedef struct {
    float EffectiveMaxAp;
    uchar LensIDNumber, LensFStops, MCUVersion, LensType;
} libraw_nikonlens_t;

typedef struct {
    float MinFocal, MaxFocal, MaxAp4MinFocal, MaxAp4MaxFocal;
} libraw_dnglens_t;

typedef struct {
    float MinFocal, MaxFocal, MaxAp4MinFocal, MaxAp4MaxFocal, EXIF_MaxAp;
    char LensMake[128], Lens[128], LensSerial[128], InternalLensSerial[128];
    ushort FocalLengthIn35mmFormat;
    libraw_nikonlens_t nikon;
    libraw_dnglens_t dng;
    libraw_makernotes_lens_t makernotes;
} libraw_lensinfo_t;

typedef struct {
    short DriveMode;
    short FocusMode;
    short MeteringMode;
    short AFPoint;
    short ExposureMode;
    short ExposureProgram;
    short ImageStabilization;
    char BodySerial[64];
    char InternalBodySerial[64];
} libraw_shootinginfo_t;

/* The processor handle. Members are filled by libraw_open_*() (metadata), libraw_unpack()
   (rawdata), libraw_unpack_thumb() (thumbnail) and libraw_dcraw_process() (image). The
   vendor `makernotes` block of LibRaw is not provided. */
typedef struct {
    ushort (*image)[4];
    libraw_image_sizes_t sizes;
    libraw_iparams_t idata;
    libraw_lensinfo_t lens;
    libraw_shootinginfo_t shootinginfo;
    libraw_output_params_t params;
    libraw_raw_unpack_params_t rawparams;
    unsigned int progress_flags;
    unsigned int process_warnings;
    libraw_colordata_t color;
    libraw_imgother_t other;
    libraw_thumbnail_t thumbnail;
    libraw_thumbnail_list_t thumbs_list;
    libraw_rawdata_t rawdata;
    void *parent_class;
} libraw_data_t;

#ifdef __cplusplus
}
#endif

#endif /* FLASHRAW_LIBRAW_TYPES_H */
