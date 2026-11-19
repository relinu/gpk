/* zstdtool c <level> <ein> <aus> | zstdtool d <ein> <aus>  – zstd mit 128-MiB-Fenster (--long=27) */
#include <stdio.h>
#include <stdlib.h>
typedef struct ZSTD_CCtx_s ZSTD_CCtx; typedef struct ZSTD_DCtx_s ZSTD_DCtx;
ZSTD_CCtx *ZSTD_createCCtx(void); size_t ZSTD_CCtx_setParameter(ZSTD_CCtx *, int, int);
size_t ZSTD_compress2(ZSTD_CCtx *, void *, size_t, const void *, size_t); size_t ZSTD_compressBound(size_t);
ZSTD_DCtx *ZSTD_createDCtx(void); size_t ZSTD_DCtx_setParameter(ZSTD_DCtx *, int, int);
size_t ZSTD_decompressDCtx(ZSTD_DCtx *, void *, size_t, const void *, size_t);
unsigned long long ZSTD_getFrameContentSize(const void *, size_t); unsigned ZSTD_isError(size_t);
static unsigned char *slurp(const char *p, size_t *n) {
    FILE *f = fopen(p, "rb"); if (!f) { perror(p); exit(1); }
    fseek(f, 0, SEEK_END); *n = ftell(f); fseek(f, 0, SEEK_SET);
    unsigned char *b = malloc(*n + 1); if (fread(b, 1, *n, f) != *n) exit(1); fclose(f); return b;
}
static void dump(const char *p, const void *b, size_t n) { FILE *f = fopen(p, "wb"); fwrite(b, 1, n, f); fclose(f); }
int main(int argc, char **argv) {
    size_t n;
    if (argc == 5 && argv[1][0] == 'c') {
        unsigned char *s = slurp(argv[3], &n); size_t cap = ZSTD_compressBound(n); unsigned char *d = malloc(cap);
        ZSTD_CCtx *c = ZSTD_createCCtx();
        ZSTD_CCtx_setParameter(c, 100, atoi(argv[2]));   /* compressionLevel */
        ZSTD_CCtx_setParameter(c, 101, 27);              /* windowLog = 128 MiB */
        ZSTD_CCtx_setParameter(c, 160, 1);               /* Long Distance Matching */
        size_t r = ZSTD_compress2(c, d, cap, s, n); if (ZSTD_isError(r)) return 1; dump(argv[4], d, r); return 0;
    }
    if (argc == 4 && argv[1][0] == 'd') {
        unsigned char *s = slurp(argv[2], &n); unsigned long long u = ZSTD_getFrameContentSize(s, n);
        unsigned char *d = malloc(u + 1); ZSTD_DCtx *x = ZSTD_createDCtx(); ZSTD_DCtx_setParameter(x, 100, 27);
        size_t r = ZSTD_decompressDCtx(x, d, u, s, n); if (ZSTD_isError(r) || r != u) return 1; dump(argv[3], d, r); return 0;
    }
    fprintf(stderr, "zstdtool c <level> <in> <out> | zstdtool d <in> <out>\n"); return 2;
}
