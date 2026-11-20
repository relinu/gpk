/*
 * PXC – verlustfreier Pixel-Coder für 8-Bit-Bilder (1–4 Kanäle, verschachtelt).
 *
 * Umkehrbare Farbtransformation (R-G, B-G), Vorhersage aus Nachbarpixeln
 * (Auswahl zwischen MED, Gradient, West und Nord je Kontext nach bisherigem
 * Fehler, plus lernende Bias-Korrektur), kontextadaptive binäre
 * Arithmetikkodierung der Reste (Kontext aus lokaler Aktivität).
 */
#ifndef PXC_H
#define PXC_H
#include <stddef.h>
#include <stdint.h>

/* Kodiert w*h Pixel mit ch Kanälen (Zeilenabstand w*ch). Liefert malloc-Puffer oder NULL. */
uint8_t *pxc_encode(const uint8_t *px, uint32_t w, uint32_t h, int ch, size_t *outn);
/* Dekodiert in px (w*h*ch Bytes). Liefert 1 bei Erfolg. */
int pxc_decode(const uint8_t *src, size_t n, uint8_t *px, uint32_t w, uint32_t h, int ch);

#endif
