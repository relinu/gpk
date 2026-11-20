/* Benchmark: GLZ vs. zlib, bzip2, xz (LZMA2), zstd, LZ4/LZ4HC.
 * Output: CSV on stdout. Every round is verified with memcmp. */
#define _POSIX_C_SOURCE 199309L
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <zlib.h>
#include <bzlib.h>
#include <lzma.h>
#include "glz.h"

/* zstd / lz4: only the runtime libraries are available -> declare the prototypes ourselves */
size_t ZSTD_compress(void *, size_t, const void *, size_t, int);
size_t ZSTD_decompress(void *, size_t, const void *, size_t);
size_t ZSTD_compressBound(size_t);
unsigned ZSTD_isError(size_t);
int LZ4_compress_default(const char *, char *, int, int);
int LZ4_compress_HC(const char *, char *, int, int, int);
int LZ4_decompress_safe(const char *, char *, int, int);
int LZ4_compressBound(int);

typedef size_t (*cfn)(const unsigned char *, size_t, unsigned char *, size_t, int);
typedef size_t (*dfn)(const unsigned char *, size_t, unsigned char *, size_t);

static size_t c_glz(const unsigned char *s, size_t n, unsigned char *d, size_t c, int l) { return glz_compress(s, n, d, c, l, 0); }
static size_t c_glznf(const unsigned char *s, size_t n, unsigned char *d, size_t c, int l) { return glz_compress(s, n, d, c, l, GLZ_FLAG_NO_FILTER); }
static size_t d_glz(const unsigned char *s, size_t n, unsigned char *d, size_t c) { return glz_decompress(s, n, d, c); }

static size_t c_zlib(const unsigned char *s, size_t n, unsigned char *d, size_t c, int l) {
    uLongf dl = c; return compress2(d, &dl, s, n, l) == Z_OK ? dl : 0;
}
static size_t d_zlib(const unsigned char *s, size_t n, unsigned char *d, size_t c) {
    uLongf dl = c; return uncompress(d, &dl, s, n) == Z_OK ? dl : (size_t)-1;
}
static size_t c_bz2(const unsigned char *s, size_t n, unsigned char *d, size_t c, int l) {
    unsigned dl = c; return BZ2_bzBuffToBuffCompress((char *)d, &dl, (char *)s, n, l, 0, 30) == BZ_OK ? dl : 0;
}
static size_t d_bz2(const unsigned char *s, size_t n, unsigned char *d, size_t c) {
    unsigned dl = c; return BZ2_bzBuffToBuffDecompress((char *)d, &dl, (char *)s, n, 0, 0) == BZ_OK ? dl : (size_t)-1;
}
static size_t c_xz(const unsigned char *s, size_t n, unsigned char *d, size_t c, int l) {
    size_t pos = 0; return lzma_easy_buffer_encode(l, LZMA_CHECK_NONE, NULL, s, n, d, &pos, c) == LZMA_OK ? pos : 0;
}
static size_t d_xz(const unsigned char *s, size_t n, unsigned char *d, size_t c) {
    uint64_t mem = UINT64_MAX; size_t ip = 0, op = 0;
    return lzma_stream_buffer_decode(&mem, 0, NULL, s, &ip, n, d, &op, c) == LZMA_OK ? op : (size_t)-1;
}
static size_t c_zstd(const unsigned char *s, size_t n, unsigned char *d, size_t c, int l) {
    size_t r = ZSTD_compress(d, c, s, n, l); return ZSTD_isError(r) ? 0 : r;
}
static size_t d_zstd(const unsigned char *s, size_t n, unsigned char *d, size_t c) {
    size_t r = ZSTD_decompress(d, c, s, n); return ZSTD_isError(r) ? (size_t)-1 : r;
}
static size_t c_lz4(const unsigned char *s, size_t n, unsigned char *d, size_t c, int l) {
    int r = l == 0 ? LZ4_compress_default((const char *)s, (char *)d, (int)n, (int)c)
                   : LZ4_compress_HC((const char *)s, (char *)d, (int)n, (int)c, l);
    return r > 0 ? (size_t)r : 0;
}
static size_t d_lz4(const unsigned char *s, size_t n, unsigned char *d, size_t c) {
    int r = LZ4_decompress_safe((const char *)s, (char *)d, (int)n, (int)c); return r >= 0 ? (size_t)r : (size_t)-1;
}

typedef struct { const char *name; cfn c; dfn d; int level; } Codec;
static const Codec CODECS[] = {
    {"GLZ -1",          c_glz,   d_glz,  1},
    {"GLZ -5",          c_glz,   d_glz,  5},
    {"GLZ -9",          c_glz,   d_glz,  9},
    {"GLZ -9 (no filter)", c_glznf, d_glz, 9},
    {"LZ4",             c_lz4,   d_lz4,  0},
    {"LZ4HC -12",       c_lz4,   d_lz4,  12},
    {"zlib -6",         c_zlib,  d_zlib, 6},
    {"zlib -9",         c_zlib,  d_zlib, 9},
    {"zstd -3",         c_zstd,  d_zstd, 3},
    {"zstd -19",        c_zstd,  d_zstd, 19},
    {"bzip2 -9",        c_bz2,   d_bz2,  9},
    {"xz -6",           c_xz,    d_xz,   6},
};

static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }

int main(int argc, char **argv) {
    printf("file,codec,raw,compressed,ratio,comp_MBps,decomp_MBps,filters\n");
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb");
        if (!f) { perror(argv[a]); return 1; }
        fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        unsigned char *src = malloc(n), *back = malloc(n + 64);
        if (fread(src, 1, n, f) != n) { perror("read"); return 1; }
        fclose(f);
        size_t cap = n + n / 2 + (1 << 20);
        unsigned char *cmp = malloc(cap);
        const char *base = strrchr(argv[a], '/'); base = base ? base + 1 : argv[a];

        for (size_t k = 0; k < sizeof CODECS / sizeof *CODECS; k++) {
            const Codec *cd = &CODECS[k];
            size_t cs = 0; int reps = 0; double t0 = now(), tc;
            do { cs = cd->c(src, n, cmp, cap, cd->level); reps++; } while ((tc = now() - t0) < 0.3 && reps < 200);
            tc /= reps;
            char filt[64] = "";
            if (cd->c == c_glz) {
                const glz_stats *st = glz_last_stats();
                snprintf(filt, sizeof filt, "none=%u delta=%u shuf=%u shufdelta=%u stored=%u",
                         st->filter_count[0], st->filter_count[1], st->filter_count[2], st->filter_count[3], st->filter_count[4]);
            }
            if (!cs) { fprintf(stderr, "%s: %s compression failed\n", base, cd->name); return 1; }
            size_t ds = 0; reps = 0; t0 = now(); double td;
            do { ds = cd->d(cmp, cs, back, n + 64); reps++; } while ((td = now() - t0) < 0.3 && reps < 2000);
            td /= reps;
            if (ds != n || memcmp(src, back, n)) { fprintf(stderr, "%s: %s ROUNDTRIP ERROR\n", base, cd->name); return 1; }
            printf("%s,%s,%zu,%zu,%.4f,%.2f,%.2f,%s\n", base, cd->name, n, cs, (double)n / cs,
                   n / tc / 1e6, n / td / 1e6, filt);
            fflush(stdout);
            fprintf(stderr, "  %-12s %-22s ratio %6.3f  comp %8.1f MB/s  decomp %8.1f MB/s\n",
                    base, cd->name, (double)n / cs, n / tc / 1e6, n / td / 1e6);
        }
        free(src); free(back); free(cmp);
    }
    return 0;
}
