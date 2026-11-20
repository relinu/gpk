/* LZRC – see lzrc.h */
#include "lzrc.h"
#include <stdlib.h>
#include <string.h>
#include <math.h>

typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef uint64_t u64;

#define NSTATES     12
#define MINLEN      2
#define MAXLEN      273
#define LIT_SIZE    0x300
#define END_POS_IDX 14
#define FULL_DIST   128
#define OPT_NUM     2048
#define DENSE_LEN   48     /* check every length up to here, above in steps of 16 */
#define H4BITS      20
#define H3BITS      16
#define NIL         0xFFFFFFFFu
#define INF         0xFFFFFFFFu
#define PROB_INIT   1024

static inline u32 rd32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }
static inline u64 rd64(const u8 *p) { u64 v; memcpy(&v, p, 8); return v; }

static const u8 NEXT_LIT[NSTATES] = {0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5};
#define NEXT_MATCH(s)    ((s) < 7 ? 7 : 10)
#define NEXT_REP(s)      ((s) < 7 ? 8 : 11)
#define NEXT_SHORTREP(s) ((s) < 7 ? 9 : 11)

/* ------------------------------------------------------------------ */
/* Model                                                               */
/* ------------------------------------------------------------------ */
typedef struct { u16 choice, choice2, low[16][8], mid[16][8], high[256]; } LenModel;
typedef struct {
    u16 isMatch[NSTATES][16], isRep[NSTATES], isRepG0[NSTATES], isRepG1[NSTATES],
        isRepG2[NSTATES], isRep0Long[NSTATES][16];
    u16 posSlot[4][64], posSpecial[FULL_DIST - END_POS_IDX], align[16];
    LenModel len, replen;
    u16 *lit;
    int lc, lp, pb;
} Model;

static int model_init(Model *m, int lc, int lp, int pb) {
    u16 *lit = malloc(sizeof(u16) * ((size_t)LIT_SIZE << (lc + lp)));
    if (!lit) return 0;
    u16 *p = (u16 *)m;
    for (size_t i = 0; i < offsetof(Model, lit) / sizeof(u16); i++) p[i] = PROB_INIT;
    for (size_t i = 0; i < ((size_t)LIT_SIZE << (lc + lp)); i++) lit[i] = PROB_INIT;
    m->lit = lit; m->lc = lc; m->lp = lp; m->pb = pb;
    return 1;
}
static inline u16 *lit_probs(const Model *m, u32 pos, u8 prev) {
    return m->lit + LIT_SIZE * (((pos & ((1u << m->lp) - 1)) << m->lc) + (prev >> (8 - m->lc)));
}
static inline u32 pos_slot(u32 d) {
    if (d < 4) return d;
    int n = 31 - __builtin_clz(d);
    return ((u32)n << 1) | ((d >> (n - 1)) & 1);
}

/* ------------------------------------------------------------------ */
/* Range-Encoder                                                       */
/* ------------------------------------------------------------------ */
typedef struct { u64 low; u32 range; u8 cache; u64 cache_size; u8 *p, *start, *end; int overflow; } RCE;

static void rce_init(RCE *rc, u8 *p, u8 *end) {
    rc->low = 0; rc->range = 0xFFFFFFFFu; rc->cache = 0; rc->cache_size = 1;
    rc->p = rc->start = p; rc->end = end; rc->overflow = 0;
}
static inline void rce_out(RCE *rc, u8 b) { if (rc->p < rc->end) *rc->p++ = b; else rc->overflow = 1; }
static void rce_shift_low(RCE *rc) {
    if ((u32)rc->low < 0xFF000000u || (rc->low >> 32) != 0) {
        u8 carry = (u8)(rc->low >> 32), temp = rc->cache;
        do { rce_out(rc, (u8)(temp + carry)); temp = 0xFF; } while (--rc->cache_size != 0);
        rc->cache = (u8)(rc->low >> 24);
    }
    rc->cache_size++;
    rc->low = (u64)((u32)rc->low << 8);
}
static inline void rce_bit(RCE *rc, u16 *p, int bit) {
    u32 bound = (rc->range >> 11) * *p;
    if (!bit) { rc->range = bound; *p += (2048 - *p) >> 5; }
    else { rc->low += bound; rc->range -= bound; *p -= *p >> 5; }
    while (rc->range < (1u << 24)) { rc->range <<= 8; rce_shift_low(rc); }
}
static void rce_direct(RCE *rc, u32 v, int nb) {
    while (nb--) {
        rc->range >>= 1;
        if ((v >> nb) & 1) rc->low += rc->range;
        while (rc->range < (1u << 24)) { rc->range <<= 8; rce_shift_low(rc); }
    }
}
static size_t rce_flush(RCE *rc) { for (int i = 0; i < 5; i++) rce_shift_low(rc); return (size_t)(rc->p - rc->start); }

static void enc_tree(RCE *rc, u16 *probs, int nb, u32 sym) {
    u32 m = 1;
    for (int i = nb - 1; i >= 0; i--) { int b = (sym >> i) & 1; rce_bit(rc, &probs[m], b); m = (m << 1) | b; }
}
static void enc_rtree(RCE *rc, u16 *probs, int nb, u32 sym) {
    u32 m = 1;
    for (int i = 0; i < nb; i++) { int b = sym & 1; sym >>= 1; rce_bit(rc, &probs[m], b); m = (m << 1) | b; }
}
static void enc_len(RCE *rc, LenModel *lm, u32 len, u32 ps) {
    u32 l = len - MINLEN;
    if (l < 8) { rce_bit(rc, &lm->choice, 0); enc_tree(rc, lm->low[ps], 3, l); }
    else if (l < 16) { rce_bit(rc, &lm->choice, 1); rce_bit(rc, &lm->choice2, 0); enc_tree(rc, lm->mid[ps], 3, l - 8); }
    else { rce_bit(rc, &lm->choice, 1); rce_bit(rc, &lm->choice2, 1); enc_tree(rc, lm->high, 8, l - 16); }
}
static void enc_dist(RCE *rc, Model *m, u32 d, u32 len) {
    u32 ls = len - MINLEN < 3 ? len - MINLEN : 3;
    u32 slot = pos_slot(d);
    enc_tree(rc, m->posSlot[ls], 6, slot);
    if (slot >= 4) {
        int fb = (int)(slot >> 1) - 1;
        u32 base = (2 | (slot & 1)) << fb, red = d - base;
        if (slot < END_POS_IDX) enc_rtree(rc, m->posSpecial + base - slot - 1, fb, red);
        else { rce_direct(rc, red >> 4, fb - 4); enc_rtree(rc, m->align, 4, red & 15); }
    }
}
static void enc_literal(RCE *rc, Model *m, u32 pos, u8 prev, u8 sym, int matched, u8 mb) {
    u16 *probs = lit_probs(m, pos, prev);
    u32 mm = 1; int same = matched;
    for (int i = 7; i >= 0; i--) {
        int b = (sym >> i) & 1;
        if (same) { int mbit = (mb >> i) & 1; rce_bit(rc, &probs[0x100 + (mbit << 8) + mm], b); same = (b == mbit); }
        else rce_bit(rc, &probs[mm], b);
        mm = (mm << 1) | b;
    }
}

/* ------------------------------------------------------------------ */
/* Range-Decoder                                                       */
/* ------------------------------------------------------------------ */
typedef struct { u32 range, code; const u8 *p, *end; } RCD;

static inline u8 rcd_next(RCD *rc) { return rc->p < rc->end ? *rc->p++ : 0; }
static void rcd_init(RCD *rc, const u8 *p, const u8 *end) {
    rc->p = p; rc->end = end; rc->range = 0xFFFFFFFFu; rc->code = 0;
    for (int i = 0; i < 5; i++) rc->code = (rc->code << 8) | rcd_next(rc);
}
static inline int rcd_bit(RCD *rc, u16 *p) {
    u32 bound = (rc->range >> 11) * *p; int b;
    if (rc->code < bound) { rc->range = bound; *p += (2048 - *p) >> 5; b = 0; }
    else { rc->code -= bound; rc->range -= bound; *p -= *p >> 5; b = 1; }
    if (rc->range < (1u << 24)) { rc->range <<= 8; rc->code = (rc->code << 8) | rcd_next(rc); }
    return b;
}
static inline u32 dec_tree(RCD *rc, u16 *probs, int nb) {
    u32 m = 1;
    for (int i = 0; i < nb; i++) m = (m << 1) | (u32)rcd_bit(rc, &probs[m]);
    return m - (1u << nb);
}
static inline u32 dec_rtree(RCD *rc, u16 *probs, int nb) {
    u32 m = 1, sym = 0;
    for (int i = 0; i < nb; i++) { int b = rcd_bit(rc, &probs[m]); m = (m << 1) | (u32)b; sym |= (u32)b << i; }
    return sym;
}
static inline u32 dec_len(RCD *rc, LenModel *lm, u32 ps) {
    if (!rcd_bit(rc, &lm->choice)) return MINLEN + dec_tree(rc, lm->low[ps], 3);
    if (!rcd_bit(rc, &lm->choice2)) return MINLEN + 8 + dec_tree(rc, lm->mid[ps], 3);
    return MINLEN + 16 + dec_tree(rc, lm->high, 8);
}
static inline u32 dec_dist(RCD *rc, Model *m, u32 len) {
    u32 ls = len - MINLEN < 3 ? len - MINLEN : 3;
    u32 slot = dec_tree(rc, m->posSlot[ls], 6);
    if (slot < 4) return slot;
    int fb = (int)(slot >> 1) - 1;
    u32 d = (2 | (slot & 1)) << fb;
    if (slot < END_POS_IDX) return d + dec_rtree(rc, m->posSpecial + d - slot - 1, fb);
    u32 direct = 0;
    for (int i = 0; i < fb - 4; i++) {
        rc->range >>= 1;
        u32 t = (rc->code - rc->range) >> 31;          /* 1 if code < range */
        rc->code -= rc->range & (t - 1);
        direct = (direct << 1) | (1 - t);
        if (rc->range < (1u << 24)) { rc->range <<= 8; rc->code = (rc->code << 8) | rcd_next(rc); }
    }
    return d + (direct << 4) + dec_rtree(rc, m->align, 4);
}

int lzrc_decompress(const uint8_t *src, size_t n, uint8_t *out, size_t rawsize) {
    if (n < 2) return 0;
    int lc = src[0] & 15, lp = src[0] >> 4, pb = src[1];
    if (lc > 8 || lp > 4 || pb > 4) return 0;
    Model m;
    if (!model_init(&m, lc, lp, pb)) return 0;
    RCD rc; rcd_init(&rc, src + 2, src + n);
    u32 state = 0, r0 = 0, r1 = 0, r2 = 0, r3 = 0, pbm = (1u << pb) - 1;
    size_t pos = 0;
    int ok = 1;
    while (pos < rawsize) {
        u32 ps = (u32)pos & pbm;
        if (!rcd_bit(&rc, &m.isMatch[state][ps])) {
            u8 prev = pos ? out[pos - 1] : 0;
            u16 *probs = lit_probs(&m, (u32)pos, prev);
            u32 mm = 1;
            if (state >= 7) {
                u32 mb = out[pos - r0 - 1]; int same = 1;
                for (int i = 7; i >= 0; i--) {
                    int b;
                    if (same) { int mbit = (mb >> i) & 1; b = rcd_bit(&rc, &probs[0x100 + (mbit << 8) + mm]); same = (b == mbit); }
                    else b = rcd_bit(&rc, &probs[mm]);
                    mm = (mm << 1) | (u32)b;
                }
            } else {
                while (mm < 0x100) mm = (mm << 1) | (u32)rcd_bit(&rc, &probs[mm]);
            }
            out[pos++] = (u8)mm;
            state = NEXT_LIT[state];
            continue;
        }
        u32 len;
        if (!rcd_bit(&rc, &m.isRep[state])) {
            len = dec_len(&rc, &m.len, ps);
            u32 d = dec_dist(&rc, &m, len);
            r3 = r2; r2 = r1; r1 = r0; r0 = d;
            state = NEXT_MATCH(state);
        } else {
            if (!rcd_bit(&rc, &m.isRepG0[state])) {
                if (!rcd_bit(&rc, &m.isRep0Long[state][ps])) {       /* Short-Rep: 1 Byte */
                    if (r0 >= pos) { ok = 0; break; }
                    out[pos] = out[pos - r0 - 1]; pos++;
                    state = NEXT_SHORTREP(state);
                    continue;
                }
            } else {
                u32 d;
                if (!rcd_bit(&rc, &m.isRepG1[state])) { d = r1; }
                else {
                    if (!rcd_bit(&rc, &m.isRepG2[state])) { d = r2; }
                    else { d = r3; r3 = r2; }
                    r2 = r1;
                }
                r1 = r0; r0 = d;
            }
            len = dec_len(&rc, &m.replen, ps);
            state = NEXT_REP(state);
        }
        if (r0 >= pos || len > rawsize - pos) { ok = 0; break; }
        u8 *o = out + pos; const u8 *s = o - r0 - 1;
        if (r0 + 1 >= len) memcpy(o, s, len);
        else for (u32 k = 0; k < len; k++) o[k] = s[k];
        pos += len;
    }
    free(m.lit);
    return ok;
}

/* ------------------------------------------------------------------ */
/* Prices (cost in 1/16 bit) for the optimal parser                    */
/* ------------------------------------------------------------------ */
static u32 PRICE[128];
static void init_prices(void) {
    static int done = 0;
    if (done) return;
    for (int i = 0; i < 128; i++) PRICE[i] = (u32)(-log2((i * 16 + 8) / 2048.0) * 16 + 0.5);
    done = 1;   /* idempotent: a race between threads is harmless */
}
#define P0(p) PRICE[(p) >> 4]
#define P1(p) PRICE[(2048 - (p)) >> 4]
static inline u32 pbit(u16 p, int b) { return b ? P1(p) : P0(p); }
static u32 price_tree(const u16 *probs, int nb, u32 sym) {
    u32 m = 1, s = 0;
    for (int i = nb - 1; i >= 0; i--) { int b = (sym >> i) & 1; s += pbit(probs[m], b); m = (m << 1) | b; }
    return s;
}
static u32 price_rtree(const u16 *probs, int nb, u32 sym) {
    u32 m = 1, s = 0;
    for (int i = 0; i < nb; i++) { int b = sym & 1; sym >>= 1; s += pbit(probs[m], b); m = (m << 1) | b; }
    return s;
}
static u32 price_literal(const Model *m, u32 pos, u8 prev, u8 sym, int matched, u8 mb) {
    const u16 *probs = lit_probs(m, pos, prev);
    u32 mm = 1, s = 0; int same = matched;
    for (int i = 7; i >= 0; i--) {
        int b = (sym >> i) & 1;
        if (same) { int mbit = (mb >> i) & 1; s += pbit(probs[0x100 + (mbit << 8) + mm], b); same = (b == mbit); }
        else s += pbit(probs[mm], b);
        mm = (mm << 1) | b;
    }
    return s;
}

typedef struct {
    u32 len[16][MAXLEN + 1], replen[16][MAXLEN + 1];
    u32 slot[4][64], full[4][FULL_DIST], align[16];
} Prices;

static void len_prices(const LenModel *lm, int nps, u32 tab[16][MAXLEN + 1]) {
    u32 c0 = P0(lm->choice), c1 = P1(lm->choice), c20 = P0(lm->choice2), c21 = P1(lm->choice2);
    for (int ps = 0; ps < nps; ps++)
        for (u32 len = MINLEN; len <= MAXLEN; len++) {
            u32 l = len - MINLEN;
            tab[ps][len] = l < 8 ? c0 + price_tree(lm->low[ps], 3, l)
                         : l < 16 ? c1 + c20 + price_tree(lm->mid[ps], 3, l - 8)
                                  : c1 + c21 + price_tree(lm->high, 8, l - 16);
        }
}
static void update_prices(const Model *m, Prices *pr) {
    int nps = 1 << m->pb;
    len_prices(&m->len, nps, pr->len);
    len_prices(&m->replen, nps, pr->replen);
    for (int i = 0; i < 16; i++) pr->align[i] = price_rtree(m->align, 4, (u32)i);
    for (int ls = 0; ls < 4; ls++) {
        for (u32 s = 0; s < 64; s++) {
            pr->slot[ls][s] = price_tree(m->posSlot[ls], 6, s);
            if (s >= END_POS_IDX) pr->slot[ls][s] += (((s >> 1) - 1 - 4) << 4);   /* direct bits */
        }
        for (u32 d = 0; d < FULL_DIST; d++) {
            u32 s = pos_slot(d), p = pr->slot[ls][s];
            if (s >= 4) {
                int fb = (int)(s >> 1) - 1; u32 base = (2 | (s & 1)) << fb;
                p += price_rtree(m->posSpecial + base - s - 1, fb, d - base);
            }
            pr->full[ls][d] = p;
        }
    }
}
static inline u32 dist_price(const Prices *pr, u32 d, u32 len) {
    u32 ls = len - MINLEN < 3 ? len - MINLEN : 3;
    if (d < FULL_DIST) return pr->full[ls][d];
    return pr->slot[ls][pos_slot(d)] + pr->align[d & 15];
}

/* ------------------------------------------------------------------ */
/* Encoder: Match-Finder + optimaler Parser                            */
/* ------------------------------------------------------------------ */
typedef struct { u32 price, from, len, dist; u8 kind, state; u32 reps[4]; } Node;
enum { K_LIT = 0, K_SHORTREP = 1, K_REP0 = 2, K_MATCH = 6 };
typedef struct { u32 len, dist; } MatchPair;

typedef struct {
    Model m; RCE rc; Prices pr;
    const u8 *buf; u32 n;
    u32 *head4, *head3, *son;   /* son: binary tree, 2 children per position */
    u32 ins;
    int depth, nice;
    u32 state, reps[4];
    Node *nodes;
    MatchPair mp[MAXLEN + 2];
} Enc;

static inline u32 h4(const u8 *p) { return (rd32(p) * 2654435761u) >> (32 - H4BITS); }
static inline u32 h3(const u8 *p) { return ((rd32(p) & 0xFFFFFFu) * 506832829u) >> (32 - H3BITS); }

static inline u32 match_len(const u8 *a, const u8 *b, u32 limit) {
    u32 l = 0;
    while (l + 8 <= limit) {
        u64 x = rd64(a + l) ^ rd64(b + l);
        if (x) return l + (__builtin_ctzll(x) >> 3);
        l += 8;
    }
    while (l < limit && a[l] == b[l]) l++;
    return l;
}

/* Binary-tree match finder: searches matches at pos and inserts pos into the tree.
 * Every node orders earlier positions lexicographically by their suffix. */
static int bt_find(Enc *e, u32 pos, u32 lenLimit, u32 maxLen, int cnt, int record) {
    const u8 *cur = e->buf + pos;
    u32 h = h4(cur), curMatch = e->head4[h];
    e->head4[h] = pos;
    u32 *ptr1 = e->son + 2 * (size_t)pos, *ptr0 = ptr1 + 1;
    u32 len0 = 0, len1 = 0;
    int cut = e->depth;
    for (;;) {
        if (curMatch == NIL || cut-- == 0) { *ptr0 = *ptr1 = NIL; break; }
        const u8 *pb = e->buf + curMatch;
        u32 *pair = e->son + 2 * (size_t)curMatch;
        u32 len = len0 < len1 ? len0 : len1;
        if (pb[len] == cur[len]) {
            len += match_len(cur + len, pb + len, lenLimit - len);
            if (record && len > maxLen) {
                maxLen = len; e->mp[cnt].len = len; e->mp[cnt].dist = pos - curMatch - 1; cnt++;
            }
            if (len == lenLimit) { *ptr1 = pair[0]; *ptr0 = pair[1]; break; }
        }
        if (pb[len] < cur[len]) { *ptr1 = curMatch; ptr1 = pair + 1; curMatch = *ptr1; len1 = len; }
        else                    { *ptr0 = curMatch; ptr0 = pair;     curMatch = *ptr0; len0 = len; }
    }
    return cnt;
}

static inline u32 bt_limit(Enc *e, u32 pos) {
    u32 r = e->n - pos;
    return r < (u32)e->nice ? r : (u32)e->nice;
}

/* Inserts skipped positions into the tree (without collecting matches). */
static void skip_to(Enc *e, u32 t) {
    for (; e->ins < t; e->ins++) {
        u32 i = e->ins;
        if (i + 4 > e->n) continue;
        e->head3[h3(e->buf + i)] = i;
        bt_find(e, i, bt_limit(e, i), 0, 0, 0);
    }
}

/* Returns matches with strictly increasing length (shortest distance for each). */
static int get_matches(Enc *e, u32 pos, u32 avail) {
    skip_to(e, pos);
    if (pos + 4 > e->n) { e->ins = pos + 1; return 0; }
    const u8 *cur = e->buf + pos;
    int cnt = 0; u32 best = 2;
    u32 hh = h3(cur), c = e->head3[hh];
    e->head3[hh] = pos;
    if (c != NIL && c < pos && pos - c <= 65536) {
        u32 l = match_len(cur, e->buf + c, avail);
        if (l > best) { e->mp[cnt].len = l; e->mp[cnt].dist = pos - c - 1; cnt++; best = l; }
    }
    u32 lim = bt_limit(e, pos);
    cnt = bt_find(e, pos, lim, best, cnt, 1);
    e->ins = pos + 1;
    /* match reaches the limit: extend it beyond the "nice" length */
    if (cnt && e->mp[cnt - 1].len == lim && lim < avail) {
        MatchPair *mp = &e->mp[cnt - 1];
        mp->len += match_len(cur + mp->len, cur + mp->len - mp->dist - 1, avail - mp->len);
    }
    return cnt;
}

static void node_derive(Node *nodes, u32 j) {
    Node *t = &nodes[j], *f = &nodes[t->from];
    switch (t->kind) {
    case K_LIT: t->state = NEXT_LIT[f->state]; memcpy(t->reps, f->reps, sizeof t->reps); break;
    case K_SHORTREP: t->state = NEXT_SHORTREP(f->state); memcpy(t->reps, f->reps, sizeof t->reps); break;
    case K_MATCH:
        t->state = NEXT_MATCH(f->state);
        t->reps[0] = t->dist; t->reps[1] = f->reps[0]; t->reps[2] = f->reps[1]; t->reps[3] = f->reps[2];
        break;
    default: {
        u32 r = t->kind - K_REP0;
        t->state = NEXT_REP(f->state);
        t->reps[0] = f->reps[r];
        for (u32 k = 1, src = 0; k < 4; k++, src++) { if (src == r) src++; t->reps[k] = f->reps[src]; }
    }
    }
}

static inline void relax(Node *nodes, u32 j, u32 price, u32 from, u8 kind, u32 len, u32 dist) {
    if (price < nodes[j].price) {
        nodes[j].price = price; nodes[j].from = from; nodes[j].kind = kind;
        nodes[j].len = len; nodes[j].dist = dist;
    }
}

static inline u32 rep_choice_price(const Model *m, u32 r, u32 st, u32 ps) {
    switch (r) {
    case 0: return P0(m->isRepG0[st]) + P1(m->isRep0Long[st][ps]);
    case 1: return P1(m->isRepG0[st]) + P0(m->isRepG1[st]);
    case 2: return P1(m->isRepG0[st]) + P1(m->isRepG1[st]) + P0(m->isRepG2[st]);
    default: return P1(m->isRepG0[st]) + P1(m->isRepG1[st]) + P1(m->isRepG2[st]);
    }
}

static void encode_step(Enc *e, u32 pos, u8 kind, u32 len, u32 dist) {
    Model *m = &e->m; RCE *rc = &e->rc;
    u32 st = e->state, ps = pos & ((1u << m->pb) - 1);
    if (kind == K_LIT) {
        rce_bit(rc, &m->isMatch[st][ps], 0);
        u8 prev = pos ? e->buf[pos - 1] : 0;
        int matched = st >= 7;
        enc_literal(rc, m, pos, prev, e->buf[pos], matched, matched ? e->buf[pos - e->reps[0] - 1] : 0);
        e->state = NEXT_LIT[st];
        return;
    }
    rce_bit(rc, &m->isMatch[st][ps], 1);
    if (kind == K_MATCH) {
        rce_bit(rc, &m->isRep[st], 0);
        enc_len(rc, &m->len, len, ps);
        enc_dist(rc, m, dist, len);
        e->reps[3] = e->reps[2]; e->reps[2] = e->reps[1]; e->reps[1] = e->reps[0]; e->reps[0] = dist;
        e->state = NEXT_MATCH(st);
        return;
    }
    rce_bit(rc, &m->isRep[st], 1);
    if (kind == K_SHORTREP) {
        rce_bit(rc, &m->isRepG0[st], 0);
        rce_bit(rc, &m->isRep0Long[st][ps], 0);
        e->state = NEXT_SHORTREP(st);
        return;
    }
    u32 r = kind - K_REP0;
    if (r == 0) { rce_bit(rc, &m->isRepG0[st], 0); rce_bit(rc, &m->isRep0Long[st][ps], 1); }
    else {
        rce_bit(rc, &m->isRepG0[st], 1);
        if (r == 1) rce_bit(rc, &m->isRepG1[st], 0);
        else { rce_bit(rc, &m->isRepG1[st], 1); rce_bit(rc, &m->isRepG2[st], r == 3); }
        u32 d = e->reps[r];
        for (u32 k = r; k > 0; k--) e->reps[k] = e->reps[k - 1];
        e->reps[0] = d;
    }
    enc_len(rc, &m->replen, len, ps);
    e->state = NEXT_REP(st);
}

/* Parse one window optimally via DP and encode it; returns the number of bytes processed. */
static u32 parse_window(Enc *e, u32 pos) {
    Node *nodes = e->nodes;
    const Model *m = &e->m;
    u32 pbm = (1u << m->pb) - 1;
    u32 last = e->n - pos < OPT_NUM ? e->n - pos : OPT_NUM;
    u32 reach = last + MAXLEN + 1;
    for (u32 k = 0; k <= reach; k++) nodes[k].price = INF;
    nodes[0].price = 0; nodes[0].state = (u8)e->state;
    memcpy(nodes[0].reps, e->reps, sizeof e->reps);
    u32 end = last;

    for (u32 i = 0; i < end; i++) {
        if (i) node_derive(nodes, i);
        Node *nd = &nodes[i];
        u32 P = pos + i, st = nd->state, ps = P & pbm;
        const u8 *cur = e->buf + P;
        u32 remaining = e->n - P, avail = remaining < MAXLEN ? remaining : MAXLEN;

        /* Literal */
        u8 prevb = P ? cur[-1] : 0;
        int matched = st >= 7;
        u32 lp = nd->price + P0(m->isMatch[st][ps]) +
                 price_literal(m, P, prevb, cur[0], matched, matched ? cur[-(int64_t)nd->reps[0] - 1] : 0);
        relax(nodes, i + 1, lp, i, K_LIT, 1, 0);

        u32 matchP = nd->price + P1(m->isMatch[st][ps]);
        u32 repP = matchP + P1(m->isRep[st]);
        u32 longest = 0;

        /* Short-Rep */
        if (nd->reps[0] < P && cur[0] == cur[-(int64_t)nd->reps[0] - 1])
            relax(nodes, i + 1, repP + P0(m->isRepG0[st]) + P0(m->isRep0Long[st][ps]), i, K_SHORTREP, 1, 0);

        /* Repeat distances */
        if (avail >= MINLEN) {
            for (u32 r = 0; r < 4; r++) {
                u32 d = nd->reps[r];
                if (d >= P) continue;
                const u8 *s = cur - d - 1;
                if (s[0] != cur[0] || s[1] != cur[1]) continue;
                u32 l = match_len(cur, s, avail);
                u32 base = repP + rep_choice_price(m, r, st, ps);
                for (u32 L = MINLEN; L <= l; L = (L < DENSE_LEN || L == l) ? L + 1 : (L + 16 < l ? L + 16 : l))
                    relax(nodes, i + L, base + e->pr.replen[ps][L], i, (u8)(K_REP0 + r), L, d);
                if (l > longest) longest = l;
            }
        }

        /* New matches */
        int nm = get_matches(e, P, avail);
        if (nm) {
            u32 normP = matchP + P0(m->isRep[st]);
            u32 L = MINLEN;
            for (int k = 0; k < nm; k++) {
                u32 d = e->mp[k].dist, ml = e->mp[k].len;
                int isrep = d == nd->reps[0] || d == nd->reps[1] || d == nd->reps[2] || d == nd->reps[3];
                if (!isrep)
                    for (; L <= ml; L = (L < DENSE_LEN || L == ml) ? L + 1 : (L + 16 < ml ? L + 16 : ml))
                        relax(nodes, i + L, normP + e->pr.len[ps][L] + dist_price(&e->pr, d, L), i, K_MATCH, L, d);
                L = ml + 1;
                if (ml > longest) longest = ml;
            }
        }

        if (longest >= (u32)e->nice) {           /* long match: end the window here */
            end = i + longest;
            break;
        }
    }

    /* Trace back: collect the path backwards, then encode forwards */
    static __thread u32 path[OPT_NUM + MAXLEN + 2];
    u32 np = 0, j = end;
    while (j > 0) { path[np++] = j; j = nodes[j].from; }
    for (u32 k = np; k-- > 0;) {
        Node *t = &nodes[path[k]];
        u32 at = pos + t->from;
        u32 dist = t->dist;
        if (t->kind >= K_REP0 && t->kind < K_MATCH) dist = 0;
        encode_step(e, at, t->kind, t->len, dist);
    }
    return end;
}

void lzrc_default_params(lzrc_params *p, int level) {
    p->lc = 3; p->lp = 0; p->pb = 2;
    if (level <= 3) { p->depth = 8; p->nice = 32; }
    else if (level <= 6) { p->depth = 32; p->nice = 64; }
    else if (level <= 8) { p->depth = 64; p->nice = 128; }
    else { p->depth = 128; p->nice = 192; }
}

size_t lzrc_bound(size_t n) { return n + n / 16 + 4096; }

size_t lzrc_compress(const uint8_t *src, size_t n, uint8_t *dst, size_t cap, const lzrc_params *p) {
    init_prices();
    if (cap < 16 || n > 0xFFFFFFF0u) return 0;
    Enc *e = calloc(1, sizeof(Enc));
    if (!e) return 0;
    size_t result = 0;
    e->buf = src; e->n = (u32)n; e->depth = p->depth; e->nice = p->nice < MAXLEN ? p->nice : MAXLEN;
    e->head4 = malloc(sizeof(u32) << H4BITS);
    e->head3 = malloc(sizeof(u32) << H3BITS);
    e->son = malloc(2 * sizeof(u32) * (n ? n : 1));
    e->nodes = malloc(sizeof(Node) * (OPT_NUM + MAXLEN + 2));
    if (!e->head4 || !e->head3 || !e->son || !e->nodes || !model_init(&e->m, p->lc, p->lp, p->pb)) goto done;
    memset(e->head4, 0xFF, sizeof(u32) << H4BITS);
    memset(e->head3, 0xFF, sizeof(u32) << H3BITS);
    dst[0] = (u8)(p->lc | (p->lp << 4)); dst[1] = (u8)p->pb;
    rce_init(&e->rc, dst + 2, dst + cap);
    u32 pos = 0, since_update = 1u << 30;
    while (pos < e->n) {
        if (since_update >= 1024) { update_prices(&e->m, &e->pr); since_update = 0; }
        u32 adv = parse_window(e, pos);
        pos += adv; since_update += adv;
        if (e->rc.overflow) goto done;
    }
    size_t sz = rce_flush(&e->rc);
    if (!e->rc.overflow) result = sz + 2;
done:
    free(e->head4); free(e->head3); free(e->son); free(e->nodes); free(e->m.lit);
    free(e);
    return result;
}
