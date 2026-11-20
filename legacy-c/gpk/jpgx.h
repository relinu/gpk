/*
 * JPGX – verlustfreie, bitgenaue Rekompression von JPEG-Dateien.
 *
 * Unterstützt: Baseline/Extended sequentiell (SOF0/SOF1), Huffman, 8 Bit,
 * 1–4 Komponenten, beliebige Abtastfaktoren, ein Scan, Restart-Intervalle.
 * Der Huffman-kodierte Scan wird zu DCT-Koeffizienten dekodiert und mit einem
 * Kontextmodell (Nachbarblöcke) arithmetisch neu kodiert. Beim Entpacken wird
 * der Original-Huffman-Strom exakt wieder erzeugt. Kopf und Ende der Datei
 * werden unverändert übernommen. Nicht unterstützte oder nicht exakt
 * reproduzierbare Dateien (progressiv, arithmetisch, mehrere Scans, unübliche
 * Kodierung) werden abgelehnt und bleiben unverändert.
 */
#ifndef JPGX_H
#define JPGX_H
#include <stddef.h>
#include <stdint.h>

/* Liefert umgewandelte Daten (malloc) oder NULL; Rekonstruktion ist vorab geprüft. */
uint8_t *jpgx_forward(const uint8_t *jpg, size_t n, size_t *tsize);
/* Rekonstruiert das Original (origsize Bytes). Liefert 1 bei Erfolg. */
int jpgx_inverse(const uint8_t *t, size_t tn, uint8_t *out, size_t origsize);

#endif
