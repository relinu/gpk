/* PNG-Rekompression testen: beide Varianten, Größe + LZRC-Schätzung + Roundtrip */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../pngx.h"
#include "../lzrc.h"
static size_t lz(const unsigned char *d, size_t n) { unsigned char *z = malloc(lzrc_bound(n)); lzrc_params p; lzrc_default_params(&p, 9);
    size_t r = lzrc_compress(d, n, z, lzrc_bound(n), &p); free(z); return r; }
int main(int argc, char **argv) {
    size_t T[3] = {0};
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        unsigned char *d = malloc(n), *o = malloc(n); if (fread(d, 1, n, f) != n) return 1; fclose(f);
        size_t t1n = 0, t2n = 0; unsigned char *t1 = pngx_forward_mode(d, n, 1, &t1n), *t2 = pngx_forward_mode(d, n, 2, &t2n);
        int ok1 = !t1 || (pngx_inverse(t1, t1n, o, n) && !memcmp(o, d, n));
        int ok2 = !t2 || (pngx_inverse(t2, t2n, o, n) && !memcmp(o, d, n));
        size_t l1 = t1 ? lz(t1, t1n) : n, l2 = t2 ? lz(t2, t2n) : n;
        T[0] += n; T[1] += l1 < n ? l1 : n; T[2] += (l2 < l1 ? l2 : l1) < n ? (l2 < l1 ? l2 : l1) : n;
        printf("%-50s %8zu | Rohdaten+LZRC %8zu %s | Pixel-Coder %8zu %s\n", strrchr(argv[a], '/') + 1, n, l1, ok1 ? "ok" : "FEHLER", t2 ? l2 : 0, t2 ? (ok2 ? "ok" : "FEHLER") : "n/a");
        free(d); free(o); free(t1); free(t2);
    }
    printf("SUMME: Original %zu | bisher %zu | mit Pixel-Coder (jeweils besser) %zu (%+.1f %%)\n", T[0], T[1], T[2], 100.0 * ((double)T[2] / T[1] - 1));
}
