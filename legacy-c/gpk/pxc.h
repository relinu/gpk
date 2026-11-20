/*
 * PXC – lossless pixel coder for 8-bit images (1–4 channels, interleaved).
 *
 * Reversible color transform (R-G, B-G), prediction from neighboring pixels
 * (weighted by the previous error per predictor: MED, gradient, west, north and
 * more, plus a learning bias correction), context-adaptive binary arithmetic
 * coding of the residuals (context from local activity).
 */
#ifndef PXC_H
#define PXC_H
#include <stddef.h>
#include <stdint.h>

/* Encodes w*h pixels with ch channels (row stride w*ch). Returns a malloc buffer or NULL. */
uint8_t *pxc_encode(const uint8_t *px, uint32_t w, uint32_t h, int ch, size_t *outn);
/* Decodes into px (w*h*ch bytes). Returns 1 on success. */
int pxc_decode(const uint8_t *src, size_t n, uint8_t *px, uint32_t w, uint32_t h, int ch);

#endif
