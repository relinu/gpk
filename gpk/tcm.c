/* TCM – siehe tcm.h */
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
#define TCM_EXTRA  1            /* zusätzliche Modelle: Spalte/Einrückung, Sparse */
#endif
#define NHASH      (6 + TCM_EXTRA)  /* gehashte Modelle: o1 o2 o3 o4 o6 Wort [Spalte Sparse] */
#define NIN        (NHASH + 3)      /* + o0, Match, Bias */
#ifndef HBITS
#define HBITS      22           /* Einträge je gehashter Tabelle (2^22 x 4 Byte = 16 MiB) */
#endif
#ifndef TCM_LR
#define TCM_LR     2            /* Lernrate der Mischnetze */
#endif
#define MMBITS     20           /* Match-Modell: Hash der letzten 6 Bytes -> Position */
#define MINMATCH   6

/* ---------------- Logistik-Hilfen ---------------- */
static int16_t STRETCH[4096];
static int tables_ready;
/* squash(d) = 4096 / (1 + e^(-d/256)), nur mit Ganzzahlen (Stützstellen + Interpolation):
 * Encoder und Decoder müssen auf jeder Plattform bitgleich rechnen – Gleitkomma
 * aus unterschiedlichen Mathe-Bibliotheken (glibc, msvcrt) wäre dafür nicht sicher. */
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
    for (int x = -2047; x <= 2047; x++) {          /* Umkehrfunktion von squash */
        int v = squash(x);
        for (int j = pi; j <= v; j++) STRETCH[j] = (int16_t)x;
        pi = v + 1;
    }
    for (int j = pi; j < 4096; j++) STRETCH[j] = 2047;
    tables_ready = 1;                               /* idempotent, Rennen harmlos */
}

/* ---------------- Zähler mit zählerabhängiger Lernrate ----------------
 * Eintrag: obere 22 Bit Wahrscheinlichkeit (für 1), untere 10 Bit Anzahl. */
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

/* ---------------- APM (Nachkorrektur) ---------------- */
typedef struct { u16 *t; int idx; } APM;
static void apm_init(APM *a, int n) {
    a->t = malloc(sizeof(u16) * (size_t)n * 33);
    for (int i = 0; i < n; i++)
        for (int j = 0; j < 33; j++) a->t[i * 33 + j] = (u16)(squash((j - 16) * 128) * 16);
    a->idx = 0;
}
static inline int apm_p(APM *a, int pr, int cx) {
    int s = STRETCH[pr] + 2048, lo = s >> 7, w = s & 127;
    a->idx = cx * 33 + lo + (w >> 6);               /* Eintrag mit größerem Gewicht wird trainiert */
    return (a->t[cx * 33 + lo] * (128 - w) + a->t[cx * 33 + lo + 1] * w) >> 11;
}
static inline void apm_update(APM *a, int bit) {
    int g = (bit << 16) + (bit << 5) - bit - bit;
    a->t[a->idx] += (u16)((g - a->t[a->idx]) >> 6);
}

/* ---------------- Modellzustand ---------------- */
typedef struct {
    u32 *ht[NHASH];             /* gehashte Tabellen, in 16er-Slots (eine Cache-Zeile) */
    u32 *slot[NHASH];           /* aktueller Slot je Modell */
    u32 hctx[NHASH];            /* Kontext-Hash je Modell (an Bytegrenze berechnet) */
    u32 o0[256 * 16];           /* Ordnung 0: direkt, [c0-Nibble-Slot] */
    u32 *o0slot;
    u32 mm_sm[64 * 2];          /* Match-Modell: Zähler nach (Länge, erwartetes Bit) */
    u32 *mmt; u32 mptr, mlen;   /* Match-Hash-Tabelle, Zeiger, Länge */
    i32 w[256][NIN];            /* Mixer 1: Gewichte je c0 */
    i32 w2[256][NIN];           /* Mixer 2: Gewichte je Vorgänger-Byte */
    int pm1, pm2;
    u32 col, linestart, above;  /* Spalte, Zeilenanfang, Zeichen darüber */
    int x[NIN];                 /* Mixer-Eingaben */
    int pr, pmix;               /* finale und gemischte Wahrscheinlichkeit */
    APM a1, a2;
    u32 c0, c4, word0, word1;   /* Teilbyte, letzte 4 Bytes, Wort-Hashes */
    int bitpos, jn;             /* Bit im Byte, Index im Nibble-Slot */
    int mm_expect, mm_idx;      /* erwartetes Bit des Match-Modells, Zählerindex */
    const u8 *buf; u32 pos;     /* bisher verarbeitete Daten (für das Match-Modell) */
} Model;

static inline u32 hash2(u32 a, u32 b) { u32 h = a * 0x9E3779B1u ^ b * 0x85EBCA77u; h ^= h >> 15; h *= 0xC2B2AE3Du; return h ^ (h >> 13); }

static int model_init(Model *m, const u8 *buf) {
    memset(m, 0, sizeof *m);
    init_tables();
    for (int i = 0; i < 1024; i++) DT[i] = (65536 * 2) / (2 * i + 3);   /* ganzzahlig: plattformunabhängig */
    for (int k = 0; k < NHASH; k++) {
        m->ht[k] = tcm_aligned_alloc(64, sizeof(u32) << HBITS);
        if (!m->ht[k]) return 0;
        memset(m->ht[k], 0, sizeof(u32) << HBITS);   /* Prüfsumme 0 = leerer Slot */
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

/* Slots für das aktuelle Nibble wählen (an Bit 0 und Bit 4).
 * Jeder Slot (16 x 4 Byte = eine Cache-Zeile) trägt in Eintrag 0 eine Prüfsumme;
 * Einträge 1..15 sind die Zähler des Nibble-Baums. Zwei mögliche Slots je Kontext:
 * passt keiner, wird der mit weniger Erfahrung (Zähler im Wurzeleintrag) ersetzt. */
static void select_slots(Model *m) {
    u32 nib = m->bitpos == 0 ? 0 : (m->c0 & 15) | 16;
    u32 tag[NHASH]; u32 *a[NHASH];
    /* Phase 1: alle Adressen berechnen und anfordern -> Speicherzugriffe laufen parallel */
    for (int k = 0; k < NHASH; k++) {
        u32 h = hash2(m->hctx[k], nib + k * 977);
        tag[k] = (h & 0xFFFFu) | 1u;                  /* nie 0: 0 = leer */
        a[k] = m->ht[k] + ((h >> (32 - HBITS)) & ~31u);   /* Paar aus zwei benachbarten Slots */
        __builtin_prefetch(a[k]);
        __builtin_prefetch(a[k] + 16);
    }
    /* Phase 2: Prüfsummen vergleichen, ggf. den schwächeren Slot ersetzen */
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

/* An der Bytegrenze: Kontexte für das nächste Byte berechnen. */
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
    /* Spalte: Position seit Zeilenanfang + Zeichen in der Zeile darüber (Einrückung, Tabellen) */
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
    m->hctx[7] = hash2(9, (c4 >> 8) & 0xFFFF);        /* Sparse: Bytes 2 und 3 zurück, ohne das letzte */
#endif
    /* Match-Modell: bestehenden Match verlängern oder neuen suchen */
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
        /* nur gültig, solange die bisherigen Bits dieses Bytes passen */
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
    /* Mixer trainieren */
    int err = ((bit << 12) - m->pm1) * TCM_LR, err2 = ((bit << 12) - m->pm2) * TCM_LR;
    i32 *w = m->w[m->c0], *w2 = m->w2[m->c4 & 0xFF];
    for (int i = 0; i < NIN; i++) { w[i] += (m->x[i] * err) >> 10; w2[i] += (m->x[i] * err2) >> 10; }
    apm_update(&m->a1, bit); apm_update(&m->a2, bit);
    /* Teilbyte fortschreiben */
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

/* ---------------- Arithmetik-Coder (32 Bit, binär) ---------------- */
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
    for (int k = 0; k < 4 && !overflow; k++) {          /* x1 vollständig ausgeben */
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
            if (b == 7) dst[i] = (u8)c;                 /* Byte steht, bevor das Modell es liest */
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
