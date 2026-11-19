/* Mini-Fuzzer: verfälschte GLZ-Daten dürfen nie zu Speicherfehlern führen (mit -fsanitize bauen). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "glz.h"
int main(int argc, char **argv) {
    srand(42); int ok = 0, rej = 0;
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        if (n > 300000) n = 300000;
        unsigned char *src = malloc(n); if (fread(src, 1, n, f) != n) return 1; fclose(f);
        size_t cap = glz_bound(n); unsigned char *c = malloc(cap), *d = malloc(n), *m = malloc(cap);
        size_t cs = glz_compress(src, n, c, cap, 3, 0);
        if (glz_decompress(c, cs, d, n) != n || memcmp(src, d, n)) { printf("Roundtrip-Fehler\n"); return 1; }
        for (int it = 0; it < 300; it++) {
            memcpy(m, c, cs); int flips = 1 + rand() % 8;
            for (int k = 0; k < flips; k++) m[12 + rand() % (cs - 12)] ^= (unsigned char)(1 + rand() % 255);
            size_t mn = (rand() % 4 == 0) ? 12 + rand() % (cs - 12) : cs;   /* manchmal abschneiden */
            if (glz_decompress(m, mn, d, n) == (size_t)-1) rej++; else ok++;
        }
        free(src); free(c); free(d); free(m);
    }
    printf("fuzz fertig: %d abgelehnt, %d dekodiert (ohne Absturz)\n", rej, ok);
    return 0;
}
