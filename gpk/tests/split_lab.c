/* Feld-Split-Varianten für DDS testen: split_lab <datei.dds> */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../filters.h"
#include "../lzrc.h"
int main(int argc, char **argv) {
    FILE *fp = fopen(argv[1], "rb"); fseek(fp, 0, SEEK_END); size_t N = ftell(fp); fseek(fp, 0, SEEK_SET);
    unsigned char *A = malloc(N), *F = malloc(N), *Z = malloc(lzrc_bound(N));
    if (fread(A, 1, N, fp) != N) return 1;
    int bc3 = !memcmp(A + 84, "DXT5", 4);
    static const unsigned char v1[][7] = {{2,4,4},{3,2,2,4},{0}}, v3[][7] = {{4,2,6,4,4},{6,1,1,6,2,2,4},{5,2,6,2,2,4},{3,8,4,4},{0}};
    const unsigned char (*v)[7] = bc3 ? v3 : v1;
    lzrc_params p; lzrc_default_params(&p, 6);
    printf("%s %s: none=%zu\n", argv[1], bc3 ? "BC3" : "BC1", lzrc_compress(A, N, Z, lzrc_bound(N), &p));
    for (int i = 0; v[i][0]; i++) {
        fspec f; memset(&f, 0, sizeof f); f.id = FT_SPLIT; f.skip = 128; f.nfields = v[i][0]; memcpy(f.fields, v[i] + 1, v[i][0]);
        filter_encode(&f, A, F, N, 0);
        printf("  split {"); for (int k = 0; k < f.nfields; k++) printf("%d%s", f.fields[k], k + 1 < f.nfields ? "," : "");
        printf("}: %zu\n", lzrc_compress(F, N, Z, lzrc_bound(N), &p));
    }
}
