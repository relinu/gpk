/* PNG recompression: inflate the IDAT Deflate stream if it is reproducible bit-exactly. */
#ifndef PNGX_H
#define PNGX_H
#include <stddef.h>
#include <stdint.h>
/* Returns the transformed data (malloc) or NULL if not reproducible bit-exactly. */
uint8_t *pngx_forward(const uint8_t *png, size_t n, size_t *tsize);
/* mode 1: inflated raw data (PNGX), mode 2: pixel coder (PNGY, 8 bits only, not interlaced). */
uint8_t *pngx_forward_mode(const uint8_t *png, size_t n, int mode, size_t *tsize);
/* Channels of an 8-bit PNG without interlacing (for filter estimates), otherwise 0. */
int pngx_channels(const uint8_t *png, size_t n);
/* Reconstructs the original PNG; returns 1 on success. out must hold origsize bytes. */
int pngx_inverse(const uint8_t *t, size_t tn, uint8_t *out, size_t origsize);
#endif
