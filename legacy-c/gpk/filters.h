/* Reversible pre-filters for game files. */
#ifndef FILTERS_H
#define FILTERS_H
#include <stddef.h>
#include <stdint.h>

enum {
    FT_NONE = 0,
    FT_DELTA,      /* byte delta with stride s (audio, height maps, raw images)    */
    FT_SHUF,       /* separate byte planes (struct arrays, floats)                  */
    FT_SHUFDELTA,  /* separate byte planes + delta (vertex buffers, 16-bit data)    */
    FT_BCJ,        /* x86/x64: relative CALL/JMP targets -> absolute (EXE, DLL, SO) */
    FT_SPLIT,      /* split records into field streams (BC1/BC3 texture blocks)     */
    FT_WPRED,      /* prediction on 16/32-bit values + byte planes (floats, PCM)    */
    FT_X86S,       /* x86 with separate streams: code | jump targets (BCJ2 principle) */
    FT_COUNT
};

typedef struct {
    uint8_t id, stride, nfields, fields[6];
    uint32_t skip;          /* FT_SPLIT: header bytes at the start of the file that stay raw */
    /* FT_WPRED: stride = record size in bytes, fields[0] = word size (2|4),
     *           fields[1] = order (1: x-x', 2: x-2x'+x''), fields[2] = 1 -> planes by record */
} fspec;

extern const char *const FILTER_NAMES[FT_COUNT];

/* file_off: position of the piece in the file (for BCJ addresses and the record phase). */
void filter_encode(const fspec *f, const uint8_t *in, uint8_t *out, size_t n, uint64_t file_off);
/* in may be modified. */
void filter_decode(const fspec *f, uint8_t *in, uint8_t *out, size_t n, uint64_t file_off);

#endif
