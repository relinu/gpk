#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../pngx.h"
int main(int argc, char **argv) {
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        unsigned char *d = malloc(n), *o = malloc(n); if (fread(d, 1, n, f) != n) return 1; fclose(f);
        size_t tn; unsigned char *t = pngx_forward(d, n, &tn);
        int ok = t && pngx_inverse(t, tn, o, n) && !memcmp(o, d, n);
        printf("%-60s %8zu -> %8zu  %s\n", argv[a], n, t ? tn : 0, t ? (ok ? "OK" : "FEHLER") : "nicht reproduzierbar");
        free(d); free(o); free(t);
    }
}
