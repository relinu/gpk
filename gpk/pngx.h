/* PNG-Rekompression: IDAT-Deflate-Strom bitgenau reproduzierbar entpacken. */
#ifndef PNGX_H
#define PNGX_H
#include <stddef.h>
#include <stdint.h>
/* Liefert umgewandelte Daten (malloc) oder NULL, wenn nicht bitgenau reproduzierbar. */
uint8_t *pngx_forward(const uint8_t *png, size_t n, size_t *tsize);
/* Rekonstruiert das Original-PNG; liefert 1 bei Erfolg. out muss origsize Bytes fassen. */
int pngx_inverse(const uint8_t *t, size_t tn, uint8_t *out, size_t origsize);
#endif
