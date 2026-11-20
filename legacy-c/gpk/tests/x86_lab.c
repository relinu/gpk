/* x86-Labor: kein Filter vs. BCJ vs. x86-Split, je Datei mit LZRC (Stufe 9) */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../filters.h"
#include "../lzrc.h"
int main(int argc, char **argv) {
    size_t tot[3] = {0}, raw = 0;
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        unsigned char *d = malloc(n), *t = malloc(n), *z = malloc(lzrc_bound(n)), *r = malloc(n);
        if (fread(d, 1, n, f) != n) return 1; fclose(f); raw += n;
        int ids[3] = {FT_NONE, FT_BCJ, FT_X86S};
        for (int k = 0; k < 3; k++) {
            fspec fs; memset(&fs, 0, sizeof fs); fs.id = ids[k];
            filter_encode(&fs, d, t, n, 0);
            lzrc_params p; lzrc_default_params(&p, 9);
            tot[k] += lzrc_compress(t, n, z, lzrc_bound(n), &p);
            filter_decode(&fs, t, r, n, 0);
            if (memcmp(d, r, n)) { printf("ROUNDTRIP-FEHLER %s\n", argv[a]); return 1; }
        }
        free(d); free(t); free(z); free(r);
    }
    printf("roh %.2f MB | kein Filter %.3f MB | BCJ %.3f MB | x86-Split %.3f MB (%+.1f %% ggü. BCJ)\n",
           raw / 1e6, tot[0] / 1e6, tot[1] / 1e6, tot[2] / 1e6, 100.0 * ((double)tot[2] / tot[1] - 1));
}
