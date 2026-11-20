/*
 * GLZ – "Game LZ": a compressor for game assets.
 *
 * Design goals (typical for games):
 *   - Asymmetric: compression happens once at build time, decompression
 *     on every load -> decompression must be fast.
 *   - Independent blocks (default 1 MiB) -> streaming, parallel unpacking
 *     and random access in package files.
 *   - Automatic pre-filters for binary data (textures, vertex buffers,
 *     audio, height maps): stride detection + delta / byte-plane shuffle.
 *   - Back end: LZ77 (hash chains, lazy matching, 2 repeat offsets)
 *     + canonical Huffman (max. 11 bits, table-driven decoding).
 */
#ifndef GLZ_H
#define GLZ_H
#include <stddef.h>

#define GLZ_FLAG_NO_FILTER 1u   /* disable the pre-filters (ablation)     */

#ifdef __cplusplus
extern "C" {
#endif

/* Maximum size of the compressed data for n input bytes. */
size_t glz_bound(size_t n);

/* level 1..9. Returns the compressed size or 0 on error. */
size_t glz_compress(const void *src, size_t n, void *dst, size_t cap,
                    int level, unsigned flags);

/* Returns the decompressed size or (size_t)-1 on error/corrupt data. */
size_t glz_decompress(const void *src, size_t n, void *dst, size_t cap);

/* Reads the original size from the header ((size_t)-1 on error). */
size_t glz_decompressed_size(const void *src, size_t n);

/* Statistics of the last compression: how often each filter was chosen. */
typedef struct { unsigned filter_count[5]; unsigned last_stride; } glz_stats;
const glz_stats *glz_last_stats(void);

#ifdef __cplusplus
}
#endif
#endif
