/* BCn-Labor: Varianten zur Zerlegung von BC1/BC3-Blöcken mit LZRC vergleichen.
 * bcn_lab <datei.dds> ...  (DXT1/DXT5, Header 128 Byte) */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include "../lzrc.h"
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
static size_t pack(const u8 *d, size_t n) {
    static u8 *z; static size_t cap; if (lzrc_bound(n) > cap) { cap = lzrc_bound(n); z = realloc(z, cap); }
    lzrc_params p; lzrc_default_params(&p, 6); return lzrc_compress(d, n, z, cap, &p);
}
int main(int argc, char **argv) {
    size_t tot[6] = {0};
    const char *names[6] = {"roh", "Split (aktuell)", "Split + Endpunkt-Delta16", "Split + RGB-Planes + Delta",
                            "Split + Indizes nach Pixelposition", "RGB-Delta + Indizes nach Pixel"};
    for (int a = 1; a < argc; a++) {
        FILE *f = fopen(argv[a], "rb"); fseek(f, 0, SEEK_END); size_t n = ftell(f); fseek(f, 0, SEEK_SET);
        u8 *d = malloc(n); if (fread(d, 1, n, f) != n) return 1; fclose(f);
        int bc3 = !memcmp(d + 84, "DXT5", 4); size_t R = bc3 ? 16 : 8, H = 128, nb = (n - H) / R;
        const u8 *blk = d + H;
        u8 *alpha = malloc(nb * 8), *E = malloc(nb * 4), *I = malloc(nb * 4), *out = malloc(n * 2 + 64);
        for (size_t i = 0; i < nb; i++) {
            const u8 *b = blk + i * R;
            if (bc3) { memcpy(alpha + i * 8, b, 8); b += 8; }
            memcpy(E + i * 4, b, 4); memcpy(I + i * 4, b + 4, 4);
        }
        size_t an = bc3 ? nb * 8 : 0, o;
        /* Alpha bleibt wie im aktuellen Split: Endpunkte (2) und Indizes (6) getrennt */
        u8 *A = malloc(an + 1); for (size_t i = 0; i < nb && bc3; i++) { memcpy(A + i * 2, alpha + i * 8, 2); memcpy(A + nb * 2 + i * 6, alpha + i * 8 + 2, 6); }
        #define EMIT(buf, len) do { memcpy(out + o, buf, len); o += len; } while (0)
        /* 0 roh */
        tot[0] += pack(d, n);
        /* 1 Split aktuell */
        o = 0; EMIT(d, H); EMIT(A, an); EMIT(E, nb * 4); EMIT(I, nb * 4); tot[1] += pack(out, o);
        /* 2 Endpunkte als 16-Bit-Delta zum vorigen Block, Byte-Planes */
        u8 *E2 = malloc(nb * 4);
        for (size_t i = 0; i < nb; i++) for (int k = 0; k < 2; k++) {
            u16 v, p = 0; memcpy(&v, E + i * 4 + 2 * k, 2); if (i) memcpy(&p, E + (i - 1) * 4 + 2 * k, 2);
            u16 r = (u16)(v - p); E2[k * 2 * nb + i] = (u8)r; E2[k * 2 * nb + nb + i] = (u8)(r >> 8);
        }
        o = 0; EMIT(d, H); EMIT(A, an); EMIT(E2, nb * 4); EMIT(I, nb * 4); tot[2] += pack(out, o);
        /* 3 RGB-Planes (6 Kanäle) mit Delta zum vorigen Block */
        u8 *E3 = malloc(nb * 6);
        for (size_t i = 0; i < nb; i++) for (int k = 0; k < 2; k++) {
            u16 v, p = 0; memcpy(&v, E + i * 4 + 2 * k, 2); if (i) memcpy(&p, E + (i - 1) * 4 + 2 * k, 2);
            int c[3] = {v >> 11, (v >> 5) & 63, v & 31}, q[3] = {p >> 11, (p >> 5) & 63, p & 31};
            for (int ch = 0; ch < 3; ch++) E3[(k * 3 + ch) * nb + i] = (u8)(c[ch] - q[ch]);
        }
        o = 0; EMIT(d, H); EMIT(A, an); EMIT(E3, nb * 6); EMIT(I, nb * 4); tot[3] += pack(out, o);
        /* 4 Indizes nach Pixelposition: 16 Streams à 2 Bit, je 4 Blöcke in ein Byte gepackt */
        size_t ib = (nb + 3) / 4; u8 *I4 = calloc(16 * ib, 1);
        for (size_t i = 0; i < nb; i++) { u32 w; memcpy(&w, I + i * 4, 4);
            for (int px = 0; px < 16; px++) I4[px * ib + i / 4] |= (u8)(((w >> (2 * px)) & 3) << (2 * (i % 4))); }
        o = 0; EMIT(d, H); EMIT(A, an); EMIT(E, nb * 4); EMIT(I4, 16 * ib); tot[4] += pack(out, o);
        /* 5 Kombination 3 + 4 */
        o = 0; EMIT(d, H); EMIT(A, an); EMIT(E3, nb * 6); EMIT(I4, 16 * ib); tot[5] += pack(out, o);
        free(d); free(alpha); free(E); free(I); free(out); free(A); free(E2); free(E3); free(I4);
    }
    for (int k = 0; k < 6; k++) printf("  %-38s %10zu  (%+.1f %% ggü. Split)\n", names[k], tot[k], 100.0 * ((double)tot[k] / tot[1] - 1));
}
