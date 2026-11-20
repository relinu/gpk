/* Umkehrbare Vorfilter für Spieldateien. */
#ifndef FILTERS_H
#define FILTERS_H
#include <stddef.h>
#include <stdint.h>

enum {
    FT_NONE = 0,
    FT_DELTA,      /* Byte-Delta mit Stride s (Audio, Heightmaps, Rohbilder)       */
    FT_SHUF,       /* Byte-Planes trennen (Struct-Arrays, Floats)                   */
    FT_SHUFDELTA,  /* Byte-Planes trennen + Delta (Vertex-Puffer, 16-Bit-Daten)     */
    FT_BCJ,        /* x86/x64: relative CALL/JMP-Ziele -> absolut (EXE, DLL, SO)    */
    FT_SPLIT,      /* Datensätze in Feld-Streams zerlegen (BC1/BC3-Texturblöcke)    */
    FT_WPRED,      /* Vorhersage auf 16/32-Bit-Werten + Byte-Planes (Floats, PCM)   */
    FT_X86S,       /* x86 mit getrennten Strömen: Code | Sprungziele (BCJ2-Prinzip) */
    FT_COUNT
};

typedef struct {
    uint8_t id, stride, nfields, fields[6];
    uint32_t skip;          /* FT_SPLIT: Header-Bytes am Dateianfang, die roh bleiben */
    /* FT_WPRED: stride = Datensatzgröße in Bytes, fields[0] = Wortgröße (2|4),
     *           fields[1] = Ordnung (1: x-x', 2: x-2x'+x''), fields[2] = 1 -> Planes nach Datensatz */
} fspec;

extern const char *const FILTER_NAMES[FT_COUNT];

/* file_off: Position des Stücks in der Datei (für BCJ-Adressen und Datensatz-Phase). */
void filter_encode(const fspec *f, const uint8_t *in, uint8_t *out, size_t n, uint64_t file_off);
/* in darf verändert werden. */
void filter_decode(const fspec *f, uint8_t *in, uint8_t *out, size_t n, uint64_t file_off);

#endif
