/* JPGX fuzz test (ASan/UBSan): round trip; corrupted original JPEGs (parser/Huffman decoder)
 * and corrupted transformed data (model decoder) must never cause memory errors. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../jpgx.h"
#include "sample_jpegs.h"
int main(void) {
    srand(5); int bad = 0, accepted = 0;
    for (unsigned s = 0; s < sizeof SAMPLE_SIZES / sizeof *SAMPLE_SIZES; s++) {
        size_t n = SAMPLE_SIZES[s]; const unsigned char *jpg = SAMPLE_JPEGS[s];
        unsigned char *o = malloc(n), *m = malloc(n);
        size_t tn = 0; unsigned char *t = jpgx_forward(jpg, n, &tn);
        if (!t || !jpgx_inverse(t, tn, o, n) || memcmp(o, jpg, n)) { printf("roundtrip error, sample %u\n", s); bad++; }
        for (int it = 0; it < 300; it++) {                 /* corrupted original: parser/Huffman decoder */
            memcpy(m, jpg, n);
            for (int k = 1 + rand() % 4; k > 0; k--) m[rand() % n] ^= (unsigned char)(1 + rand() % 255);
            size_t mn = rand() % 5 ? n : (size_t)(rand() % n);
            size_t tn2; unsigned char *t2 = jpgx_forward(m, mn, &tn2);
            if (t2) {                                       /* accepted -> must be exactly reconstructible */
                accepted++;
                unsigned char *o2 = malloc(mn ? mn : 1);
                if (!jpgx_inverse(t2, tn2, o2, mn) || memcmp(o2, m, mn)) { printf("accepted, but reconstructed incorrectly\n"); bad++; }
                free(o2); free(t2);
            }
        }
        for (int it = 0; t && it < 300; it++) {           /* corrupted transformed data */
            unsigned char *x = malloc(tn); memcpy(x, t, tn);
            for (int k = 1 + rand() % 4; k > 0; k--) x[rand() % tn] ^= (unsigned char)(1 + rand() % 255);
            jpgx_inverse(x, rand() % 5 ? tn : (size_t)(rand() % tn), o, n);
            free(x);
        }
        free(o); free(m); free(t);
    }
    printf("JPGX fuzz test: %s (%d corrupted JPEGs accepted and reconstructed exactly)\n", bad ? "ERROR" : "OK", accepted);
    return bad != 0;
}
