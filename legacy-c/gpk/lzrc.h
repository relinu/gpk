/*
 * LZRC – compression core of GPK.
 *
 * LZ77 with a large window (whole block, up to 64 MiB), price-based
 * "optimal" parsing (dynamic programming over up to 2048 positions)
 * and adaptive binary arithmetic coding (range coder) of all fields.
 * The modeling (state machine, 4 repeat distances, literal contexts
 * lc/lp/pb, distance slots) follows the proven LZMA principle; the code and
 * the format are an independent implementation.
 */
#ifndef LZRC_H
#define LZRC_H
#include <stddef.h>
#include <stdint.h>

typedef struct {
    int lc, lp, pb;   /* literal context bits, literal position bits, position bits */
    int depth;        /* search depth of the match finder                         */
    int nice;         /* "good enough" length: take it immediately above this     */
} lzrc_params;

void   lzrc_default_params(lzrc_params *p, int level);
size_t lzrc_bound(size_t n);
/* Returns the compressed size, 0 if it does not fit into cap (-> store raw). */
size_t lzrc_compress(const uint8_t *src, size_t n, uint8_t *dst, size_t cap, const lzrc_params *p);
/* Returns 1 on success, 0 on corrupt data. rawsize must be known. */
int    lzrc_decompress(const uint8_t *src, size_t n, uint8_t *dst, size_t rawsize);

#endif
