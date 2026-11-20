/* PXC – see pxc.h. Integer arithmetic only: encoder and decoder must compute
 * bit-identically on all platforms. */
#include "pxc.h"
#include <stdlib.h>
#include <string.h>

typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef int32_t i32; typedef int64_t i64; typedef uint64_t u64;

#define NACT   10                 /* classes of gradient activity */
#define NERR   7                  /* classes of neighbor error energy */
#define NCTX   (NACT * NERR)
#define NPRED  6                  /* MED, gradient, west, north, mean(W,N), mean(W,N,NW,NE) */
#define MAXE   11                 /* exponent of the residual magnitude */

/* ---------------- binary arithmetic coder ---------------- */
typedef struct { u32 x1, x2, x; u8 *out; size_t n, cap; const u8 *in; size_t ip, inn; int err; } AC;
static void ac_put(AC *a, u8 b) {
    if (a->n == a->cap) { a->cap = a->cap * 2 + 4096; u8 *p = realloc(a->out, a->cap); if (!p) { a->err = 1; return; } a->out = p; }
    a->out[a->n++] = b;
}
/* probability from two counters with a fast and a slow learning rate */
typedef struct { u16 f, s; } Prob;
static inline u32 pr12(const Prob *p) { u32 v = ((u32)p->f + p->s) >> 5; return v < 1 ? 1 : (v > 4095 ? 4095 : v); }
static inline void pupd(Prob *p, int bit) {
    if (bit) { p->f += (65535 - p->f) >> 4; p->s += (65535 - p->s) >> 7; }
    else     { p->f -= p->f >> 4;           p->s -= p->s >> 7; }
}
static inline void enc_bit(AC *a, Prob *p, int bit) {
    u32 xmid = a->x1 + (u32)(((u64)(a->x2 - a->x1) * pr12(p)) >> 12);
    if (bit) a->x2 = xmid; else a->x1 = xmid + 1;
    pupd(p, bit);
    while (((a->x1 ^ a->x2) & 0xFF000000u) == 0) { ac_put(a, (u8)(a->x2 >> 24)); a->x1 <<= 8; a->x2 = (a->x2 << 8) | 255; }
}
static inline int dec_bit(AC *a, Prob *p) {
    u32 xmid = a->x1 + (u32)(((u64)(a->x2 - a->x1) * pr12(p)) >> 12);
    int bit = a->x <= xmid;
    if (bit) a->x2 = xmid; else a->x1 = xmid + 1;
    pupd(p, bit);
    while (((a->x1 ^ a->x2) & 0xFF000000u) == 0) {
        a->x1 <<= 8; a->x2 = (a->x2 << 8) | 255;
        a->x = (a->x << 8) | (a->ip < a->inn ? a->in[a->ip++] : 0);
    }
    return bit;
}

/* ---------------- Model ---------------- */
typedef struct {
    Prob zero[4][NCTX], sign[4][NCTX], expo[4][NCTX][MAXE + 1], mant[4][NERR][MAXE + 1][2];
    i32 bsum[4][NCTX], bcnt[4][NCTX];                    /* bias correction per context */
} Model;
static void model_init(Model *m) {
    Prob *p = (Prob *)m;
    size_t np = offsetof(Model, bsum) / sizeof(Prob);
    for (size_t i = 0; i < np; i++) { p[i].f = 32768; p[i].s = 32768; }
    memset(m->bsum, 0, sizeof m->bsum); memset(m->bcnt, 0, sizeof m->bcnt);
}

/* row buffers per channel: values (int16 plane), error per predictor, residual magnitudes */
typedef struct {
    int16_t *pl;                /* whole plane */
    u16 *pe[NPRED][2];          /* |error| per predictor: row above / current row */
    u16 *re[2];                 /* |residual| (after bias): row above / current row */
} Chan;

static inline int iabs(int v) { return v < 0 ? -v : v; }
static inline int qclass(int v, const int *T, int n) { int c = 0; while (c < n - 1 && v > T[c]) c++; return c; }
static const int TACT[NACT - 1] = {0, 2, 4, 7, 11, 17, 26, 40, 64};
static const int TERR[NERR - 1] = {0, 2, 4, 8, 15, 28};

static inline int med(int W, int N, int NW) {
    int mx = W > N ? W : N, mn = W < N ? W : N;
    if (NW >= mx) return mn;
    if (NW <= mn) return mx;
    return W + N - NW;
}

typedef struct { int pred, ctx, eq, p[NPRED]; } Ctx;
/* context and prediction for pixel (x,y) of a channel – identical in encoder and decoder */
static inline void context(const Model *m, const Chan *c, u32 w, u32 x, u32 y, int k, int r0mag, Ctx *o) {
    const int16_t *row = c->pl + (size_t)y * w, *up = y ? row - w : NULL;
    int W = x ? row[x - 1] : (y ? up[x] : 0);
    int N = y ? up[x] : W;
    int NW = (x && y) ? up[x - 1] : N;
    int NE = (y && x + 1 < w) ? up[x + 1] : N;
    int WW = x > 1 ? row[x - 2] : W;
    o->p[0] = med(W, N, NW);
    int g = W + N - NW, mx = W > N ? W : N, mn = W < N ? W : N;
    o->p[1] = g < mn ? mn : (g > mx ? mx : g);
    o->p[2] = W; o->p[3] = N;
    o->p[4] = (W + N + 1) >> 1;                       /* means dampen noise */
    o->p[5] = (W + N + NW + NE + 2) >> 2;
    /* weighted mix: weight ~ 1 / (local error of the predictor)^2 */
    i64 num = 0, den = 0;
    for (int i = 0; i < NPRED; i++) {
        u32 e = 1;
        if (y) { e += c->pe[i][0][x]; e += x ? c->pe[i][0][x - 1] : 0; e += x + 1 < w ? c->pe[i][0][x + 1] : 0; }
        if (x) e += 2u * c->pe[i][1][x - 1];
        i64 wt = ((i64)1 << 40) / ((i64)e * e);
        num += wt * o->p[i]; den += wt;
    }
    int blend = (int)((num + den / 2) / den);
    /* error energy of the neighbors and gradient activity -> coding context */
    int en = (x ? c->re[1][x - 1] : 0) + (y ? c->re[0][x] : 0) + ((y && x) ? c->re[0][x - 1] >> 1 : 0) + ((y && x + 1 < w) ? c->re[0][x + 1] >> 1 : 0);
    if (k > 0) en += r0mag >> 1;                                 /* residual of the green channel at the same pixel */
    int act = iabs(NE - N) + iabs(N - NW) + iabs(NW - W) + (iabs(W - WW) >> 1);
    o->eq = qclass(en, TERR, NERR);
    o->ctx = qclass(act, TACT, NACT) * NERR + o->eq;
    int bias = m->bcnt[k][o->ctx] ? m->bsum[k][o->ctx] / m->bcnt[k][o->ctx] : 0;
    o->pred = blend + bias;
}
static inline void learn(Model *m, Chan *c, u32 x, int k, const Ctx *o, int v) {
    for (int i = 0; i < NPRED; i++) { int e = iabs(v - o->p[i]); c->pe[i][1][x] = (u16)(e > 65535 ? 65535 : e); }
    int r = v - o->pred; c->re[1][x] = (u16)iabs(r);
    i32 *bs = &m->bsum[k][o->ctx], *bc = &m->bcnt[k][o->ctx];
    *bs += r + (*bc ? *bs / *bc : 0);                          /* error before bias correction */
    (*bc)++;
    if (*bc >= 32) { *bs /= 2; *bc /= 2; }
}
static void next_row(Chan *c) {
    for (int i = 0; i < NPRED; i++) { u16 *t = c->pe[i][0]; c->pe[i][0] = c->pe[i][1]; c->pe[i][1] = t; }
    u16 *t = c->re[0]; c->re[0] = c->re[1]; c->re[1] = t;
}

static void code_res(AC *a, Model *m, int k, const Ctx *o, int r) {
    enc_bit(a, &m->zero[k][o->ctx], r != 0);
    if (!r) return;
    enc_bit(a, &m->sign[k][o->ctx], r < 0);
    u32 v = (u32)iabs(r), e = 0;                     /* v >= 1; e = floor(log2 v) */
    while (v >> (e + 1)) e++;
    for (u32 i = 0; i < e; i++) enc_bit(a, &m->expo[k][o->ctx][i], 1);
    if (e < MAXE) enc_bit(a, &m->expo[k][o->ctx][e], 0);
    for (int b = (int)e - 1; b >= 0; b--) enc_bit(a, &m->mant[k][o->eq][e][b == (int)e - 1 ? 0 : 1], (v >> b) & 1);
}
static int decode_res(AC *a, Model *m, int k, const Ctx *o) {
    if (!dec_bit(a, &m->zero[k][o->ctx])) return 0;
    int neg = dec_bit(a, &m->sign[k][o->ctx]);
    u32 e = 0;
    while (e < MAXE && dec_bit(a, &m->expo[k][o->ctx][e])) e++;
    u32 v = 1;
    for (int b = (int)e - 1; b >= 0; b--) v = (v << 1) | (u32)dec_bit(a, &m->mant[k][o->eq][e][b == (int)e - 1 ? 0 : 1]);
    return neg ? -(int)v : (int)v;
}

static int chans_alloc(Chan *c, int ch, u32 w, u32 h) {
    memset(c, 0, sizeof(Chan) * 4);
    for (int k = 0; k < ch; k++) {
        if (!(c[k].pl = malloc((size_t)w * h * sizeof(int16_t)))) return 0;
        for (int i = 0; i < NPRED; i++) for (int j = 0; j < 2; j++) if (!(c[k].pe[i][j] = calloc(w, sizeof(u16)))) return 0;
        for (int j = 0; j < 2; j++) if (!(c[k].re[j] = calloc(w, sizeof(u16)))) return 0;
    }
    return 1;
}
static void chans_free(Chan *c) {
    for (int k = 0; k < 4; k++) {
        free(c[k].pl);
        for (int i = 0; i < NPRED; i++) for (int j = 0; j < 2; j++) free(c[k].pe[i][j]);
        for (int j = 0; j < 2; j++) free(c[k].re[j]);
    }
}

static uint8_t *encode_ct(const uint8_t *px, uint32_t w, uint32_t h, int ch, int ct, size_t *outn) {
    if (ch < 1 || ch > 4 || !w || !h) return NULL;
    size_t np = (size_t)w * h;
    Chan c[4]; memset(c, 0, sizeof c);
    Model *m = malloc(sizeof(Model));
    if (!m || !chans_alloc(c, ch, w, h)) { chans_free(c); free(m); return NULL; }
    for (size_t i = 0; i < np; i++) {                 /* color transform: G, R-G, B-G, A */
        const u8 *p = px + i * ch;
        if (ct && ch >= 3) { c[0].pl[i] = p[1]; c[1].pl[i] = (int16_t)(p[0] - p[1]); c[2].pl[i] = (int16_t)(p[2] - p[1]); if (ch == 4) c[3].pl[i] = p[3]; }
        else for (int k = 0; k < ch; k++) c[k].pl[i] = p[k];
    }
    model_init(m);
    AC a = {0}; a.x2 = 0xFFFFFFFFu;
    ac_put(&a, (u8)ct);                                /* first byte: color transform */
    for (u32 y = 0; y < h; y++) {
        for (u32 x = 0; x < w; x++) {
            int r0 = 0;
            for (int k = 0; k < ch; k++) {
                Ctx o; context(m, &c[k], w, x, y, k, ct ? r0 : 0, &o);
                int v = c[k].pl[(size_t)y * w + x], r = v - o.pred;
                code_res(&a, m, k, &o, r);
                learn(m, &c[k], x, k, &o, v);
                if (k == 0) r0 = iabs(r);
            }
        }
        for (int k = 0; k < ch; k++) next_row(&c[k]);
    }
    for (int i = 0; i < 4; i++) { ac_put(&a, (u8)(a.x1 >> 24)); a.x1 <<= 8; }
    chans_free(c); free(m);
    if (a.err) { free(a.out); return NULL; }
    *outn = a.n;
    return a.out;
}

/* try both variants (with/without color transform), take the smaller one */
uint8_t *pxc_encode(const uint8_t *px, uint32_t w, uint32_t h, int ch, size_t *outn) {
    size_t n0 = 0, n1 = 0;
    uint8_t *a = encode_ct(px, w, h, ch, 0, &n0);
    if (ch < 3) { *outn = n0; return a; }
    uint8_t *b = encode_ct(px, w, h, ch, 1, &n1);
    if (!a || (b && n1 < n0)) { free(a); *outn = n1; return b; }
    free(b); *outn = n0; return a;
}

int pxc_decode(const uint8_t *src, size_t n, uint8_t *px, uint32_t w, uint32_t h, int ch) {
    if (ch < 1 || ch > 4 || !w || !h) return 0;
    size_t np = (size_t)w * h;
    Chan c[4]; memset(c, 0, sizeof c);
    Model *m = malloc(sizeof(Model));
    int ok = 0;
    if (!m || !chans_alloc(c, ch, w, h)) goto done;
    model_init(m);
    if (n < 1 || src[0] > 1) goto done;
    int ct = src[0] && ch >= 3;
    AC a = {0}; a.x2 = 0xFFFFFFFFu; a.in = src; a.inn = n; a.ip = 1;
    for (int i = 0; i < 4; i++) a.x = (a.x << 8) | (a.ip < n ? src[a.ip++] : 0);
    for (u32 y = 0; y < h; y++) {
        for (u32 x = 0; x < w; x++) {
            int r0 = 0;
            for (int k = 0; k < ch; k++) {
                Ctx o; context(m, &c[k], w, x, y, k, ct ? r0 : 0, &o);
                int r = decode_res(&a, m, k, &o), v = o.pred + r;
                if (v < (ct && k > 0 && k < 3 ? -255 : 0) || v > 255) goto done;   /* corrupt data */
                c[k].pl[(size_t)y * w + x] = (int16_t)v;
                learn(m, &c[k], x, k, &o, v);
                if (k == 0) r0 = iabs(r);
            }
        }
        for (int k = 0; k < ch; k++) next_row(&c[k]);
    }
    for (size_t i = 0; i < np; i++) {                 /* undo the color transform */
        u8 *p = px + i * ch;
        if (ct) { int g = c[0].pl[i]; p[1] = (u8)g; p[0] = (u8)(c[1].pl[i] + g); p[2] = (u8)(c[2].pl[i] + g); if (ch == 4) p[3] = (u8)c[3].pl[i]; }
        else for (int k = 0; k < ch; k++) p[k] = (u8)c[k].pl[i];
    }
    ok = 1;
done:
    chans_free(c); free(m);
    return ok;
}
