/* TCM-Test: tcm_test <datei>... -> Größe, Tempo, Roundtrip */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "../tcm.h"
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }
int main(int argc, char **argv) {
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        unsigned char *s = malloc(n), *c = malloc(tcm_bound(n)), *d = malloc(n + 1);
        if (fread(s, 1, n, f) != n) return 1; fclose(f);
        double t0 = now(); size_t cs = tcm_compress(s, n, c, tcm_bound(n)); double tc = now() - t0;
        t0 = now(); int ok = tcm_decompress(c, cs, d, n) && !memcmp(s, d, n); double td = now() - t0;
        printf("%-40s %10zu -> %9zu (%.3f bpc)  pack %.1f MB/s  entpack %.1f MB/s  %s\n", strrchr(argv[a], '/') ? strrchr(argv[a], '/') + 1 : argv[a],
               n, cs, 8.0 * cs / n, n / tc / 1e6, n / td / 1e6, ok ? "OK" : "FEHLER");
    }
}
