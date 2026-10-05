/*
 * FlashRaw LibRaw-compatible API: version macros.
 *
 * Part of flashraw-libraw-compat, an independent implementation of a subset of the LibRaw
 * C API on top of FlashRaw (see docs/libraw-compat.md). Licensed under MIT OR Apache-2.0.
 * This file contains no LibRaw code.
 */
#ifndef FLASHRAW_LIBRAW_VERSION_H
#define FLASHRAW_LIBRAW_VERSION_H

/* The LibRaw API level this library implements (a subset of; see docs/libraw-compat.md). */
#define LIBRAW_MAJOR_VERSION 0
#define LIBRAW_MINOR_VERSION 22
#define LIBRAW_PATCH_VERSION 0
#define LIBRAW_VERSION_TAIL FlashRaw

/* Marker for code that wants to detect the FlashRaw implementation at compile time. */
#define LIBRAW_FLASHRAW_COMPAT 1

#define _LIBRAW_VERSION_MAKE(a, b, c, d) #a "." #b "." #c "-" #d
#define LIBRAW_VERSION_MAKE(a, b, c, d) _LIBRAW_VERSION_MAKE(a, b, c, d)
#define LIBRAW_VERSION_STR                                                                         \
    LIBRAW_VERSION_MAKE(LIBRAW_MAJOR_VERSION, LIBRAW_MINOR_VERSION, LIBRAW_PATCH_VERSION,         \
                        LIBRAW_VERSION_TAIL)

#define LIBRAW_MAKE_VERSION(major, minor, patch) (((major) << 16) | ((minor) << 8) | (patch))
#define LIBRAW_VERSION                                                                             \
    LIBRAW_MAKE_VERSION(LIBRAW_MAJOR_VERSION, LIBRAW_MINOR_VERSION, LIBRAW_PATCH_VERSION)

/* Run-time checks (work from C and C++). */
#define LIBRAW_CHECK_VERSION(major, minor, patch)                                                  \
    (libraw_versionNumber() >= LIBRAW_MAKE_VERSION(major, minor, patch))
#define LIBRAW_RUNTIME_CHECK_VERSION_EXACT()                                                       \
    ((libraw_versionNumber() & 0xffff00) == LIBRAW_MAKE_VERSION(LIBRAW_MAJOR_VERSION,             \
                                                                 LIBRAW_MINOR_VERSION, 0))
#define LIBRAW_RUNTIME_CHECK_VERSION_NOTLESS()                                                     \
    ((libraw_versionNumber() & 0xffff00) >=                                                       \
     LIBRAW_MAKE_VERSION(LIBRAW_MAJOR_VERSION, LIBRAW_MINOR_VERSION, 0))

/* Compile-time checks. */
#define LIBRAW_COMPILE_CHECK_VERSION(major, minor)                                                 \
    (LIBRAW_MAKE_VERSION(major, minor, 0) == (LIBRAW_VERSION & 0xffff00))
#define LIBRAW_COMPILE_CHECK_VERSION_NOTLESS(major, minor)                                         \
    (LIBRAW_MAKE_VERSION(major, minor, 0) <= (LIBRAW_VERSION & 0xffff00))

#endif /* FLASHRAW_LIBRAW_VERSION_H */
