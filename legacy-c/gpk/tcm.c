/* TCM – see tcm.h */
#include "tcm.h"
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <malloc.h>
#define tcm_aligned_alloc(al, n) _aligned_malloc((n), (al))
#define tcm_aligned_free(p) _aligned_free(p)
#else
#define tcm_aligned_alloc(al, n) aligned_alloc((al), (n))
#define tcm_aligned_free(p) free(p)
#endif

typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef int32_t i32; typedef uint64_t u64;

#ifndef TCM_EXTRA
#define TCM_EXTRA  1            /* additional models: column/indentation, sparse */
#endif
#define NHASH      (6 + TCM_EXTRA)  /* hashed models: o1 o2 o3 o4 o6 word [column sparse] */
#define NIN        (NHASH + 3)      /* + o0, Match, Bias */
#ifndef HBITS
#define HBITS      22           /* entries per hashed table (2^22 x 4 bytes = 16 MiB) */
#endif
#ifndef TCM_LR
#define TCM_LR     2            /* learning rate of the mixers */
#endif
#define MMBITS     20           /* match model: hash of the last 6 bytes -> position */
#define MINMATCH   6

/* ---------------- Logistic helpers ---------------- */
static int16_t STRETCH[4096];
static int tables_ready;
/* squash(d) = 4096 / (1 + e^(-d/256)), with integers only (support points + interpolation):
 * encoder and decoder must compute bit-identically on every platform – floating point
 * from different math libraries (glibc, msvcrt) would not be safe for that. */
static int squash(int d) {
    static const int T[33] = {1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047,
                              2549, 2994, 3348, 3607, 3785, 3901, 3975, 4022, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094};
    if (d > 2047) return 4095;
    if (d < -2047) return 1;
    int w = d & 127; d = (d >> 7) + 16;
    int v = (T[d] * (128 - w) + T[d + 1] * w + 64) >> 7;
    return v < 1 ? 1 : (v > 4095 ? 4095 : v);
}
static void init_tables(void) {
    if (tables_ready) return;
    int pi = 0;
    for (int x = -2047; x <= 2047; x++) {          /* inverse function of squash */
        int v = squash(x);
        for (int j = pi; j <= v; j++) STRETCH[j] = (int16_t)x;
        pi = v + 1;
    }
    for (int j = pi; j < 4096; j++) STRETCH[j] = 2047;
    tables_ready = 1;                               /* idempotent, races are harmless */
}

/* ---------------- Counters with a count-dependent learning rate ----------------
 * Entry: upper 22 bits probability (of a 1), lower 10 bits count. */
static int DT[1024];
#ifndef SM_LIMIT
#define SM_LIMIT 255
#endif
static inline int sm_p(u32 e) { return (int)(e >> 20); }                 /* 12 Bit */
static inline void sm_update(u32 *e, int bit) {
    u32 v = *e; int n = v & 1023; i32 p = (i32)(v >> 10);
    i32 target = bit ? (1 << 22) - 1 : 0;
    p += (i32)(((int64_t)(target - p) * DT[n]) >> 16);
    if (n < SM_LIMIT) n++;
    *e = ((u32)p << 10) | (u32)n;
}

/* ---------------- APM (refinement) ---------------- */
typedef struct { u16 *t; int idx; } APM;
static void apm_init(APM *a, int n) {
    a->t = malloc(sizeof(u16) * (size_t)n * 33);
    for (int i = 0; i < n; i++)
        for (int j = 0; j < 33; j++) a->t[i * 33 + j] = (u16)(squash((j - 16) * 128) * 16);
    a->idx = 0;
}
static inline int apm_p(APM *a, int pr, int cx) {
    int s = STRETCH[pr] + 2048, lo = s >> 7, w = s & 127;
    a->idx = cx * 33 + lo + (w >> 6);               /* the entry with the larger weight is trained */
    return (a->t[cx * 33 + lo] * (128 - w) + a->t[cx * 33 + lo + 1] * w) >> 11;
}
static inline void apm_update(APM *a, int bit) {
    int g = (bit << 16) + (bit << 5) - bit - bit;
    a->t[a->idx] += (u16)((g - a->t[a->idx]) >> 6);
}

/* ---------------- Model state ---------------- */
typedef struct {
    u32 *ht[NHASH];             /* hashed tables, in slots of 16 (one cache line) */
    u32 *slot[NHASH];           /* current slot per model */
    u32 hctx[NHASH];            /* context hash per model (computed at byte boundaries) */
    u32 o0[256 * 16];           /* order 0: direct, [c0 nibble slot] */
    u32 *o0slot;
    u32 mm_sm[64 * 2];          /* match model: counters by (length, expected bit) */
    u32 *mmt; u32 mptr, mlen;   /* match hash table, pointer, length */
    i32 w[256][NIN];            /* mixer 1: weights per c0 */
    i32 w2[256][NIN];           /* mixer 2: weights per previous byte */
    int pm1, pm2;
    u32 col, linestart, above;  /* column, line start, character above */
    int x[NIN];                 /* mixer inputs */
    int pr, pmix;               /* final and mixed probability */
    APM a1, a2;
    u32 c0, c4, word0, word1;   /* partial byte, last 4 bytes, word hashes */
    int bitpos, jn;             /* bit in the byte, index in the nibble slot */
    int mm_expect, mm_idx;      /* expected bit of the match model, counter index */
    const u8 *buf; u32 pos;     /* data processed so far (for the match model) */
} Model;

static inline u32 hash2(u32 a, u32 b) { u32 h = a * 0x9E3779B1u ^ b * 0x85EBCA77u; h ^= h >> 15; h *= 0xC2B2AE3Du; return h ^ (h >> 13); }

static int model_init(Model *m, const u8 *buf) {
    memset(m, 0, sizeof *m);
    init_tables();
    for (int i = 0; i < 1024; i++) DT[i] = (65536 * 2) / (2 * i + 3);   /* integer: platform-independent */
    for (int k = 0; k < NHASH; k++) {
        m->ht[k] = tcm_aligned_alloc(64, sizeof(u32) << HBITS);
        if (!m->ht[k]) return 0;
        memset(m->ht[k], 0, sizeof(u32) << HBITS);   /* check value 0 = empty slot */
    }
    for (int i = 0; i < 256 * 16; i++) m->o0[i] = 1u << 31;
    for (int i = 0; i < 128; i++) m->mm_sm[i] = 1u << 31;
    m->mmt = calloc(1u << MMBITS, sizeof(u32));
    if (!m->mmt) return 0;
    for (int c = 0; c < 256; c++) for (int i = 0; i < NIN; i++) m->w[c][i] = m->w2[c][i] = (1 << 16) / 4;
    apm_init(&m->a1, 256); apm_init(&m->a2, 65536);
    m->c0 = 1; m->pr = 2048; m->buf = buf; m->jn = 1;
    return 1;
}
static void model_free(Model *m) {
    for (int k = 0; k < NHASH; k++) tcm_aligned_free(m->ht[k]);
    free(m->mmt); free(m->a1.t); free(m->a2.t);
}

/* Choose the slots for the current nibble (at bit 0 and bit 4).
 * Every slot (16 x 4 bytes = one cache line) holds a check value in entry 0;
 * entries 1..15 are the counters of the nibble tree. Two possible slots per context:
 * if neither matches, the one with less experience (count in the root entry) is replaced. */
static void select_slots(Model *m) {
    u32 nib = m->bitpos == 0 ? 0 : (m->c0 & 15) | 16;
    u32 tag[NHASH]; u32 *a[NHASH];
    /* Phase 1: compute and request all addresses -> the memory accesses run in parallel */
    for (int k = 0; k < NHASH; k++) {
        u32 h = hash2(m->hctx[k], nib + k * 977);
        tag[k] = (h & 0xFFFFu) | 1u;                  /* never 0: 0 = empty */
        a[k] = m->ht[k] + ((h >> (32 - HBITS)) & ~31u);   /* pair of two adjacent slots */
        __builtin_prefetch(a[k]);
        __builtin_prefetch(a[k] + 16);
    }
    /* Phase 2: compare the check values, replace the weaker slot if needed */
    for (int k = 0; k < NHASH; k++) {
        u32 *x = a[k], *y = a[k] + 16, *s;
        if (x[0] == tag[k]) s = x;
        else if (y[0] == tag[k]) s = y;
        else {
            s = (x[1] & 1023) <= (y[1] & 1023) ? x : y;
            s[0] = tag[k];
            for (int j = 1; j < 16; j++) s[j] = 1u << 31;
        }
        m->slot[k] = s;
    }
    m->o0slot = m->o0 + (nib == 0 ? 0 : (nib & 15) + 1) * 16;
}

/* At a byte boundary: compute the contexts for the next byte. */
static void byte_update(Model *m, u8 c) {
    m->c4 = (m->c4 << 8) | c;
    int letter = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c >= 128 || (c >= '0' && c <= '9') || c == '_';
    if (letter) m->word0 = hash2(m->word0, (u32)(c | 32));
    else if (m->word0) { m->word1 = m->word0; m->word0 = 0; }
    u32 c4 = m->c4;
    m->hctx[0] = hash2(1, c4 & 0xFF);
    m->hctx[1] = hash2(2, c4 & 0xFFFF);
    m->hctx[2] = hash2(3, c4 & 0xFFFFFF);
    m->hctx[3] = hash2(4, c4);
    m->hctx[4] = hash2(hash2(6, c4), m->pos >= 6 ? (u32)(m->buf[m->pos - 5] | m->buf[m->pos - 6] << 8) : 0);
    m->hctx[5] = hash2(hash2(7, m->word0), m->word1 + (letter ? 0 : c));
    /* Column: position since the line start + character in the line above (indentation, tables) */
    if (c == '\n') { m->above = m->linestart; m->linestart = m->pos; m->col = 0; }
    else m->col++;
#if TCM_EXTRA >= 1
    {
        u32 ab = 0, prevline = m->above;
        if (prevline + m->col < m->linestart) ab = m->buf[prevline + m->col];
        m->hctx[6] = hash2(hash2(8, m->col < 64 ? m->col : 64), ab | (c << 8));
    }
#endif
#if TCM_EXTRA >= 2
    m->hctx[7] = hash2(9, (c4 >> 8) & 0xFFFF);        /* sparse: bytes 2 and 3 back, without the last one */
#endif
    /* Match model: extend the existing match or search a new one */
    if (m->mlen && m->buf[m->mptr] == c) { m->mlen++; m->mptr++; }
    else m->mlen = 0;
    if (m->pos >= MINMATCH) {
        u32 h = 0;
        for (int i = 1; i <= MINMATCH; i++) h = h * 0x2F0B4A23u + m->buf[m->pos - i];
        h = (h ^ (h >> 15)) >> (32 - MMBITS);
        if (!m->mlen) {
            u32 cand = m->mmt[h];
            if (cand) {
                u32 l = 0;
                while (l < 32 && l < cand && m->buf[cand - 1 - l] == m->buf[m->pos - 1 - l]) l++;
                if (l >= MINMATCH) { m->mlen = l; m->mptr = cand; }
            }
        }
        m->mmt[h] = m->pos;
    }
}

static int predict(Model *m) {
    int j = m->jn;
    for (int k = 0; k < NHASH; k++) m->x[k + 1] = STRETCH[sm_p(m->slot[k][j])];
    m->x[0] = STRETCH[sm_p(m->o0slot[j])];
    if (m->mlen) {
        u8 pb = m->buf[m->mptr];
        int expect = (pb >> (7 - m->bitpos)) & 1;
        /* only valid as long as the bits of this byte so far match */
        if (((u32)(pb | 256) >> (8 - m->bitpos)) == m->c0) {
            int lb = m->mlen > 31 ? 31 : (int)m->mlen;
            m->mm_idx = lb * 2 + expect; m->mm_expect = 1;
            int st = STRETCH[sm_p(m->mm_sm[m->mm_idx])];
            m->x[NHASH + 1] = st;
        } else { m->mm_expect = 0; m->x[NHASH + 1] = 0; }
    } else { m->mm_expect = 0; m->x[NHASH + 1] = 0; }
    m->x[NIN - 1] = 256;
    i32 *w = m->w[m->c0], *w2 = m->w2[m->c4 & 0xFF];
    int64_t dot = 0, dot2 = 0;
    for (int i = 0; i < NIN; i++) { dot += (int64_t)m->x[i] * w[i]; dot2 += (int64_t)m->x[i] * w2[i]; }
    m->pm1 = squash((int)(dot >> 16)); m->pm2 = squash((int)(dot2 >> 16));
    m->pmix = squash((STRETCH[m->pm1] + STRETCH[m->pm2]) >> 1);
    int p1 = apm_p(&m->a1, m->pmix, (int)m->c0);
    int p2 = apm_p(&m->a2, m->pmix, (int)(m->c0 | ((m->c4 & 0xFF) << 8)));
    int p = (m->pmix + p1 + 2 * p2 + 2) >> 2;
    if (p < 1) p = 1;
    if (p > 4095) p = 4095;
    return m->pr = p;
}

static void update(Model *m, int bit) {
    int j = m->jn;
    for (int k = 0; k < NHASH; k++) sm_update(&m->slot[k][j], bit);
    sm_update(&m->o0slot[j], bit);
    if (m->mm_expect) sm_update(&m->mm_sm[m->mm_idx], bit);
    /* train the mixers */
    int err = ((bit << 12) - m->pm1) * TCM_LR, err2 = ((bit << 12) - m->pm2) * TCM_LR;
    i32 *w = m->w[m->c0], *w2 = m->w2[m->c4 & 0xFF];
    for (int i = 0; i < NIN; i++) { w[i] += (m->x[i] * err) >> 10; w2[i] += (m->x[i] * err2) >> 10; }
    apm_update(&m->a1, bit); apm_update(&m->a2, bit);
    /* advance the partial byte */
    m->c0 = (m->c0 << 1) | (u32)bit;
    m->jn = (m->jn << 1) | bit;
    m->bitpos++;
    if (m->bitpos == 4) { m->jn = 1; select_slots(m); }
    else if (m->bitpos == 8) {
        u8 c = (u8)m->c0;
        m->pos++;
        byte_update(m, c);
        m->c0 = 1; m->bitpos = 0; m->jn = 1;
        select_slots(m);
    }
}

/* ---------------- Arithmetic coder (32 bits, binary) ---------------- */
size_t tcm_bound(size_t n) { return n + n / 32 + 64; }

size_t tcm_compress(const uint8_t *src, size_t n, uint8_t *dst, size_t cap) {
    Model *m = malloc(sizeof(Model));
    if (!m || !model_init(m, src)) { if (m) model_free(m); free(m); return 0; }
    select_slots(m);
    u32 x1 = 0, x2 = 0xFFFFFFFFu;
    size_t op = 0; int overflow = 0;
    for (size_t i = 0; i < n && !overflow; i++) {
        u8 c = src[i];
        for (int b = 7; b >= 0; b--) {
            int bit = (c >> b) & 1;
            u32 p = (u32)predict(m);
            u32 xmid = x1 + (u32)(((u64)(x2 - x1) * p) >> 12);
            if (bit) x2 = xmid; else x1 = xmid + 1;
            update(m, bit);
            while (((x1 ^ x2) & 0xFF000000u) == 0) {
                if (op >= cap) { overflow = 1; break; }
                dst[op++] = (u8)(x2 >> 24);
                x1 <<= 8; x2 = (x2 << 8) | 255;
            }
        }
    }
    for (int k = 0; k < 4 && !overflow; k++) {          /* flush x1 completely */
        if (op >= cap) { overflow = 1; break; }
        dst[op++] = (u8)(x1 >> 24); x1 <<= 8;
    }
    model_free(m); free(m);
    return overflow ? 0 : op;
}

int tcm_decompress(const uint8_t *src, size_t n, uint8_t *dst, size_t rawsize) {
    Model *m = malloc(sizeof(Model));
    if (!m || !model_init(m, dst)) { if (m) model_free(m); free(m); return 0; }
    select_slots(m);
    u32 x1 = 0, x2 = 0xFFFFFFFFu, x = 0;
    size_t ip = 0;
    for (int k = 0; k < 4; k++) x = (x << 8) | (ip < n ? src[ip++] : 0);
    for (size_t i = 0; i < rawsize; i++) {
        u32 c = 0;
        for (int b = 0; b < 8; b++) {
            u32 p = (u32)predict(m);
            u32 xmid = x1 + (u32)(((u64)(x2 - x1) * p) >> 12);
            int bit = x <= xmid;
            if (bit) x2 = xmid; else x1 = xmid + 1;
            c = (c << 1) | (u32)bit;
            if (b == 7) dst[i] = (u8)c;                 /* the byte is stored before the model reads it */
            update(m, bit);
            while (((x1 ^ x2) & 0xFF000000u) == 0) {
                x1 <<= 8; x2 = (x2 << 8) | 255;
                x = (x << 8) | (ip < n ? src[ip++] : 0);
            }
        }
    }
    model_free(m); free(m);
    return 1;
}
