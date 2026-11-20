/* Filter lab: filter_lab <file> -> size after LZRC for many filter variants */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../filters.h"
#include "../lzrc.h"
static unsigned char *A, *F, *Z, *D, *T; static size_t N; static size_t best = (size_t)-1; static char bestname[64];
static void run(const char *name, fspec f) {
    filter_encode(&f, A, F, N, 0);
    lzrc_params p; lzrc_default_params(&p, 6);
    size_t cs = lzrc_compress(F, N, Z, lzrc_bound(N), &p);
    memcpy(T, F, N); filter_decode(&f, T, D, N, 0);
    int ok = !memcmp(A, D, N);
    printf("  %-34s %9zu  %s\n", name, cs, ok ? "" : "ROUNDTRIP ERROR");
    if (cs < best) { best = cs; snprintf(bestname, sizeof bestname, "%s", name); }
}
int main(int argc, char **argv) {
    FILE *fp = fopen(argv[1], "rb"); fseek(fp, 0, SEEK_END); N = ftell(fp); fseek(fp, 0, SEEK_SET);
    A = malloc(N); F = malloc(N); D = malloc(N); T = malloc(N); Z = malloc(lzrc_bound(N));
    if (fread(A, 1, N, fp) != N) return 1;
    int S = argc > 2 ? atoi(argv[2]) : 4;
    printf("%s (%zu bytes), record %d\n", argv[1], N, S);
    fspec f; char nm[64];
    memset(&f, 0, sizeof f); run("none", f);
    memset(&f, 0, sizeof f); f.id = FT_DELTA; f.stride = S; run("byte-delta(S)", f);
    memset(&f, 0, sizeof f); f.id = FT_SHUFDELTA; f.stride = S; run("shuffle+delta(S)", f);
    memset(&f, 0, sizeof f); f.id = FT_SHUF; f.stride = S; run("shuffle(S)", f);
    for (int W = 2; W <= 4; W += 2) for (int o = 1; o <= 2; o++) for (int pl = 0; pl <= 1; pl++) {
        if (S % W) continue;
        memset(&f, 0, sizeof f); f.id = FT_WPRED; f.stride = S; f.fields[0] = W; f.fields[1] = o; f.fields[2] = pl;
        snprintf(nm, sizeof nm, "wpred W=%d ord=%d planes=%s", W, o, pl ? "record" : "word"); run(nm, f);
    }
    printf("  -> bester: %s\n", bestname);
}
