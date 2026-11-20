/* Size estimate for x86 split variants (encoder only, LZRC level 9). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include "../lzrc.h"
typedef uint8_t u8; typedef uint32_t u32;
static size_t pack(const u8 *d, size_t n) { static u8 *z; static size_t c; if (lzrc_bound(n) > c) { c = lzrc_bound(n); z = realloc(z, c); }
    lzrc_params p; lzrc_default_params(&p, 9); return n ? lzrc_compress(d, n, z, c, &p) : 0; }
static u32 rd32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }
static u32 absv(u32 v, u32 pos) { if ((v >> 24) && (v >> 24) != 0xFF) return v; v = (v + pos) & 0x1FFFFFF; if (v & 0x1000000) v |= 0xFE000000; return v; }
/* mode bits: 1=include Jcc, 2=big endian, 4=selective (only 00/FF) + bit mask, 8=compress streams separately, 16=CALL/JMP in separate streams */
static size_t variant(const u8 *in, size_t n, int mode) {
    u8 *mainb = malloc(n), *ops = malloc(n), *ops2 = malloc(n), *bits = calloc(n / 8 + 1, 1);
    size_t i = 0, mp = 0, op = 0, op2 = 0, nb = 0;
    while (i < n) {
        size_t ol = 0;
        if ((in[i] & 0xFE) == 0xE8 && i + 5 <= n) ol = 1;
        else if ((mode & 1) && in[i] == 0x0F && i + 6 <= n && (in[i + 1] & 0xF0) == 0x80) ol = 2;
        if (!ol) { mainb[mp++] = in[i++]; continue; }
        u32 v = rd32(in + i + ol);
        int take = 1;
        if (mode & 4) { take = (v >> 24) == 0 || (v >> 24) == 0xFF; if (take) bits[nb / 8] |= (u8)(1 << (nb % 8)); nb++; }
        memcpy(mainb + mp, in + i, ol); mp += ol;
        if (!take) { memcpy(mainb + mp, in + i + ol, 4); mp += 4; i += ol + 4; continue; }
        v = absv(v, (u32)(i + ol + 4));
        u8 *dst = ((mode & 16) && in[i] != 0xE8) ? ops2 + op2 : ops + op;
        if (mode & 2) { dst[0] = v >> 24; dst[1] = v >> 16; dst[2] = v >> 8; dst[3] = v; } else memcpy(dst, &v, 4);
        if ((mode & 16) && in[i] != 0xE8) op2 += 4; else op += 4;
        i += ol + 4;
    }
    size_t bl = (mode & 4) ? nb / 8 + 1 : 0, r;
    if (mode & 8) r = pack(mainb, mp) + pack(ops, op) + pack(ops2, op2) + pack(bits, bl);
    else { u8 *all = malloc(n + bl + 8); memcpy(all, mainb, mp); memcpy(all + mp, ops, op); memcpy(all + mp + op, ops2, op2); memcpy(all + mp + op + op2, bits, bl);
           r = pack(all, mp + op + op2 + bl); free(all); }
    free(mainb); free(ops); free(ops2); free(bits);
    return r;
}
int main(int argc, char **argv) {
    static const int M[] = {3, 1, 2, 7, 11, 15, 27, 31, 15 & ~1};
    static const char *N[] = {"current (Jcc, BE, move everything)", "little endian", "without Jcc", "+ selective with bit mask",
                              "+ streams compressed separately", "selective + separate", "separate + CALL/JMP separate", "selective + separate + CALL/JMP separate", "selective + separate, without Jcc"};
    size_t tot[9] = {0};
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        u8 *d = malloc(n); if (fread(d, 1, n, f) != n) return 1; fclose(f);
        for (int k = 0; k < 9; k++) tot[k] += variant(d, n, M[k]);
        free(d);
    }
    for (int k = 0; k < 9; k++) printf("  %-44s %.3f MB (%+.1f %%)\n", N[k], tot[k] / 1e6, 100.0 * ((double)tot[k] / tot[0] - 1));
}
