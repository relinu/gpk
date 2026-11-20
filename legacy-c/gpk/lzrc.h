/*
 * LZRC – Kompressionskern von GPK.
 *
 * LZ77 mit großem Fenster (ganzer Block, bis 64 MiB), preisbasiertem
 * "optimalem" Parsing (dynamische Programmierung über bis zu 2048 Positionen)
 * und adaptiver binärer Arithmetikkodierung (Range Coder) aller Felder.
 * Die Modellierung (Zustandsautomat, 4 Repeat-Distanzen, Literal-Kontexte
 * lc/lp/pb, Distanz-Slots) folgt dem bewährten LZMA-Prinzip; der Code und
 * das Format sind eine eigenständige Implementierung.
 */
#ifndef LZRC_H
#define LZRC_H
#include <stddef.h>
#include <stdint.h>

typedef struct {
    int lc, lp, pb;   /* Literal-Kontextbits, Literal-Positionsbits, Positionsbits */
    int depth;        /* Suchtiefe Hash-Kette                                     */
    int nice;         /* "gut genug"-Länge: darüber sofort nehmen                 */
} lzrc_params;

void   lzrc_default_params(lzrc_params *p, int level);
size_t lzrc_bound(size_t n);
/* Liefert komprimierte Größe, 0 wenn nicht in cap passend (-> roh speichern). */
size_t lzrc_compress(const uint8_t *src, size_t n, uint8_t *dst, size_t cap, const lzrc_params *p);
/* Liefert 1 bei Erfolg, 0 bei korrupten Daten. rawsize muss bekannt sein. */
int    lzrc_decompress(const uint8_t *src, size_t n, uint8_t *dst, size_t rawsize);

#endif
