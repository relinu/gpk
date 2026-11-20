/*
 * TCM – context-mixing core for text (scripts, JSON, localization, shaders).
 *
 * Bitwise prediction with several models (orders 0–4 and 6, word model,
 * match model), mixed by a learning weight network (logistic mixing) and
 * refined by two APM stages; binary arithmetic coder.
 * Clearly better than LZ on text, but slower (only used for text blocks).
 * Own implementation following the PAQ/lpaq principle.
 */
#ifndef TCM_H
#define TCM_H
#include <stddef.h>
#include <stdint.h>

size_t tcm_bound(size_t n);
/* Returns the compressed size or 0 if it does not fit into cap. */
size_t tcm_compress(const uint8_t *src, size_t n, uint8_t *dst, size_t cap);
/* Returns 1 on success. rawsize must be known. */
int    tcm_decompress(const uint8_t *src, size_t n, uint8_t *dst, size_t rawsize);

#endif
