/*
 * TCM – Context-Mixing-Kern für Text (Skripte, JSON, Lokalisierung, Shader).
 *
 * Bitweise Vorhersage mit mehreren Modellen (Ordnung 0–4 und 6, Wortmodell,
 * Match-Modell), gemischt durch ein lernendes Gewichtsnetz (logistisches
 * Mischen) und nachkorrigiert durch zwei APM-Stufen; binärer Arithmetik-Coder.
 * Deutlich besser als LZ auf Text, dafür langsamer (wird nur für Textblöcke
 * verwendet). Eigene Implementierung nach dem PAQ-/lpaq-Prinzip.
 */
#ifndef TCM_H
#define TCM_H
#include <stddef.h>
#include <stdint.h>

size_t tcm_bound(size_t n);
/* Liefert komprimierte Größe oder 0, wenn nicht in cap passend. */
size_t tcm_compress(const uint8_t *src, size_t n, uint8_t *dst, size_t cap);
/* Liefert 1 bei Erfolg. rawsize muss bekannt sein. */
int    tcm_decompress(const uint8_t *src, size_t n, uint8_t *dst, size_t rawsize);

#endif
