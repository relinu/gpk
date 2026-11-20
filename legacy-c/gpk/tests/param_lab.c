/* lc/lp/pb grid on a (filtered) file: param_lab <file> [wpred-stride] */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../filters.h"
#include "../lzrc.h"
int main(int argc, char **argv) {
    FILE *fp = fopen(argv[1], "rb"); fseek(fp, 0, SEEK_END); size_t n = ftell(fp); fseek(fp, 0, SEEK_SET);
    if (n > (4u << 20)) n = 4u << 20;
    unsigned char *a = malloc(n), *b = malloc(n), *z = malloc(lzrc_bound(n)); if (fread(a, 1, n, fp) != n) return 1;
    fspec f; memset(&f, 0, sizeof f);
    if (argc > 2 && !strcmp(argv[2], "bc3")) { f.id = FT_SPLIT; f.skip = 128; f.nfields = 4; f.fields[0] = 2; f.fields[1] = 6; f.fields[2] = 4; f.fields[3] = 4; }
    else if (argc > 2 && !strcmp(argv[2], "bc1")) { f.id = FT_SPLIT; f.skip = 128; f.nfields = 2; f.fields[0] = 4; f.fields[1] = 4; }
    else if (argc > 2) { f.id = FT_WPRED; f.stride = atoi(argv[2]); f.fields[0] = 4; f.fields[1] = 2; f.fields[2] = 1; }
    filter_encode(&f, a, b, n, 0);
    static const int P[][3] = {{3,0,2},{0,0,0},{1,0,0},{4,0,0},{3,0,0},{0,2,2},{1,2,2},{0,0,2},{2,0,2},{8,0,0},{0,4,4},{0,1,1},{0,2,1},{2,2,2},{0,3,3},{1,1,1}};
    for (unsigned i = 0; i < sizeof P / sizeof *P; i++) {
        lzrc_params p; lzrc_default_params(&p, 6); p.lc = P[i][0]; p.lp = P[i][1]; p.pb = P[i][2];
        printf("  lc=%d lp=%d pb=%d: %zu\n", p.lc, p.lp, p.pb, lzrc_compress(b, n, z, lzrc_bound(n), &p));
    }
}
