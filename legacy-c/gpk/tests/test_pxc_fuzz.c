/* PXC fuzz test: round trip for 1-4 channels, noise and flat areas, corrupted streams (ASan/UBSan). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../pxc.h"
int main(void) {
    srand(11); int bad = 0;
    for (int it = 0; it < 24; it++) {
        unsigned w = 1 + rand() % 97, h = 1 + rand() % 61; int ch = 1 + it % 4;
        size_t n = (size_t)w * h * ch;
        unsigned char *px = malloc(n), *out = malloc(n);
        for (size_t i = 0; i < n; i++)
            px[i] = it % 3 == 0 ? (unsigned char)rand() : it % 3 == 1 ? (unsigned char)((i / ch) % w * 3 + rand() % 5) : (unsigned char)((i / 37) & 1 ? 255 : 0);
        size_t cn = 0; unsigned char *c = pxc_encode(px, w, h, ch, &cn);
        if (!c || !pxc_decode(c, cn, out, w, h, ch) || memcmp(px, out, n)) { printf("roundtrip error %d (%ux%u, %d channels)\n", it, w, h, ch); bad++; }
        for (int k = 0; c && k < 6; k++) {
            unsigned char *m = malloc(cn); memcpy(m, c, cn);
            m[rand() % cn] ^= (unsigned char)(1 + rand() % 255);
            pxc_decode(m, k == 5 ? cn / 2 : cn, out, w, h, ch);
            free(m);
        }
        free(px); free(out); free(c);
    }
    printf("PXC fuzz test: %s\n", bad ? "ERROR" : "OK");
    return bad != 0;
}
