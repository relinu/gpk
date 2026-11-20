/*
 * JPGX – lossless, bit-exact recompression of JPEG files.
 *
 * Supported: baseline/extended sequential (SOF0/SOF1), Huffman, 8 bits,
 * 1–4 components, any sampling factors, one scan, restart intervals.
 * The Huffman-coded scan is decoded into DCT coefficients and re-encoded
 * arithmetically with a context model (neighboring blocks). On unpacking the
 * original Huffman stream is recreated exactly. Head and tail of the file are
 * copied unchanged. Unsupported or not exactly reproducible files
 * (progressive, arithmetic, several scans, unusual coding) are rejected and
 * stay unchanged.
 */
#ifndef JPGX_H
#define JPGX_H
#include <stddef.h>
#include <stdint.h>

/* Returns the transformed data (malloc) or NULL; the reconstruction is checked up front. */
uint8_t *jpgx_forward(const uint8_t *jpg, size_t n, size_t *tsize);
/* Reconstructs the original (origsize bytes). Returns 1 on success. */
int jpgx_inverse(const uint8_t *t, size_t tn, uint8_t *out, size_t origsize);

#endif
