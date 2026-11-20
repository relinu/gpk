/* TCM-Fuzztest: Roundtrip auf Text/Binärdaten und beschädigte Ströme (mit ASan/UBSan bauen). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../tcm.h"
static const char TXT[] = "der Drache und das Schwert\n  {\"key\": 42}\n";
int main(void) {
    srand(7);
    int bad = 0;
    for (int it = 0; it < 6; it++) {
        size_t n = 20000 + (size_t)(rand() % 60000);
        unsigned char *s = malloc(n), *c = malloc(tcm_bound(n)), *d = malloc(n);
        for (size_t i = 0; i < n; i++)
            s[i] = it % 2 ? (unsigned char)rand() : (unsigned char)TXT[(i * 7 + i / 13) % strlen(TXT)];
        size_t cs = tcm_compress(s, n, c, tcm_bound(n));
        if (!cs || !tcm_decompress(c, cs, d, n) || memcmp(s, d, n)) { printf("Roundtrip-Fehler %d\n", it); bad++; }
        for (int k = 0; k < 4; k++) {                      /* verfälschen / abschneiden: darf nicht abstürzen */
            unsigned char *m = malloc(cs); memcpy(m, c, cs);
            m[rand() % cs] ^= (unsigned char)(1 + rand() % 255);
            tcm_decompress(m, k == 3 ? cs / 2 : cs, d, n);
            free(m);
        }
        free(s); free(c); free(d);
    }
    printf("TCM-Fuzztest: %s\n", bad ? "FEHLER" : "OK");
    return bad != 0;
}
