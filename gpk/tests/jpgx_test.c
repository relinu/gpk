/* JPEG-Rekompression testen: Größe, Vergleich mit LZRC, bitgenauer Roundtrip */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../jpgx.h"
#include "../lzrc.h"
int main(int argc, char **argv) {
    size_t to = 0, tj = 0, tl = 0; int nok = 0, nno = 0, nbad = 0;
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); if (!f) continue; fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        unsigned char *d = malloc(n), *o = malloc(n), *z = malloc(lzrc_bound(n)); if (fread(d, 1, n, f) != n) return 1; fclose(f);
        lzrc_params p; lzrc_default_params(&p, 6); size_t lz = lzrc_compress(d, n, z, lzrc_bound(n), &p);
        size_t tn = 0; unsigned char *t = jpgx_forward(d, n, &tn);
        const char *st = "nicht unterstützt";
        if (t) { int ok = jpgx_inverse(t, tn, o, n) && !memcmp(o, d, n); st = ok ? "OK" : "FEHLER"; if (ok) nok++; else nbad++; } else nno++;
        to += n; tl += lz < n ? lz : n; tj += t ? tn : (lz < n ? lz : n);
        printf("%-58s %9zu | LZRC %9zu | JPGX %9zu (%+5.1f %%) %s\n", strrchr(argv[a], '/') ? strrchr(argv[a], '/') + 1 : argv[a], n, lz, t ? tn : 0, t ? 100.0 * ((double)tn / n - 1) : 0.0, st);
        free(d); free(o); free(z); free(t);
    }
    printf("SUMME: %d OK, %d nicht unterstützt, %d FEHLER | Original %zu | LZRC %zu | mit JPGX %zu (%+.1f %% ggü. Original)\n", nok, nno, nbad, to, tl, tj, 100.0 * ((double)tj / to - 1));
    return nbad != 0;
}
