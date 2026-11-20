/*
 * GLZ – "Game LZ": ein Kompressor für Spiel-Assets.
 *
 * Designziele (typisch für Spiele):
 *   - Asymmetrisch: Kompression passiert einmal beim Build, Dekompression
 *     bei jedem Ladevorgang -> Dekompression muss schnell sein.
 *   - Unabhängige Blöcke (Standard 1 MiB) -> Streaming, paralleles Entpacken
 *     und wahlfreier Zugriff in Paketdateien.
 *   - Automatische Vorfilter für Binärdaten (Texturen, Vertex-Puffer,
 *     Audio, Heightmaps): Stride-Erkennung + Delta / Byte-Plane-Shuffle.
 *   - Backend: LZ77 (Hash-Ketten, Lazy Matching, 2 Repeat-Offsets)
 *     + kanonisches Huffman (max. 11 Bit, tabellenbasiert dekodiert).
 */
#ifndef GLZ_H
#define GLZ_H
#include <stddef.h>

#define GLZ_FLAG_NO_FILTER 1u   /* Vorfilter deaktivieren (Ablation)      */

#ifdef __cplusplus
extern "C" {
#endif

/* Maximale Größe der komprimierten Daten für n Eingabebytes. */
size_t glz_bound(size_t n);

/* level 1..9. Liefert komprimierte Größe oder 0 bei Fehler. */
size_t glz_compress(const void *src, size_t n, void *dst, size_t cap,
                    int level, unsigned flags);

/* Liefert dekomprimierte Größe oder (size_t)-1 bei Fehler/korrupten Daten. */
size_t glz_decompress(const void *src, size_t n, void *dst, size_t cap);

/* Liest die Originalgröße aus dem Header ((size_t)-1 bei Fehler). */
size_t glz_decompressed_size(const void *src, size_t n);

/* Statistik der letzten Kompression: wie oft wurde welcher Filter gewählt. */
typedef struct { unsigned filter_count[5]; unsigned last_stride; } glz_stats;
const glz_stats *glz_last_stats(void);

#ifdef __cplusplus
}
#endif
#endif
