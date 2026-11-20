/*
 * GLZ – "Game LZ" – see glz.h for the design goals.
 *
 * Container format:
 *   "GLZ1" | u64 original size | blocks...
 *   Block: u8 filter | u8 stride | u8 mode (0=stored, 1=lz) |
 *          u32 rawsize | u32 payloadsize | payload
 *
 * LZ payload of a block:
 *   u32 nseq | HUF(literals) | HUF(tokens) | HUF(offcodes) |
 *   u32 extlen | ext (LEB128 length overflows) | u32 bitlen | offset extra bits
 *
 *   token   = (min(litlen,15) << 4) | min(matchlen-4, 15)
 *   offcode = 0 -> rep0, 1 -> rep1 (swap), 2+b -> offset (1<<b)+extra(b bits)
 */
#include "glz.h"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

typedef uint8_t  u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;

#define BLOCK_SIZE   (1u << 20)
#define MINMATCH     4
#define HUF_MAXLEN   11
#define HUF_TABSIZE  (1u << HUF_MAXLEN)
#define HASH_BITS    17
#define NIL          0xFFFFFFFFu
#define BLK_HDR      11
#define FILE_HDR     12
#define TRIAL_BYTES  (256u * 1024u)

enum { F_NONE = 0, F_DELTA = 1, F_SHUF = 2, F_SHUFDELTA = 3 };

static glz_stats g_stats;
const glz_stats *glz_last_stats(void) { return &g_stats; }

static inline void wr32(u8 *p, u32 v) { memcpy(p, &v, 4); }
static inline u32  rd32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }
static inline u64  rd64(const u8 *p) { u64 v; memcpy(&v, p, 8); return v; }
static inline int  bitlen32(u32 v) { return 31 - __builtin_clz(v); } /* floor(log2 v), v>0 */

/* ======================================================================= */
/* Bit I/O (LSB first)                                                     */
/* ======================================================================= */
typedef struct { u8 *p, *start, *end; u64 acc; int n; int overflow; } BW;

static void bw_init(BW *b, u8 *p, u8 *end) {
    b->p = b->start = p; b->end = end; b->acc = 0; b->n = 0; b->overflow = 0;
}
static inline void bw_put(BW *b, u32 v, int nb) {           /* nb <= 32 */
    b->acc |= (u64)v << b->n;
    b->n += nb;
    while (b->n >= 8) {
        if (b->p < b->end) *b->p++ = (u8)b->acc; else b->overflow = 1;
        b->acc >>= 8; b->n -= 8;
    }
}
static size_t bw_flush(BW *b) {
    if (b->n > 0) { if (b->p < b->end) *b->p++ = (u8)b->acc; else b->overflow = 1; }
    b->acc = 0; b->n = 0;
    return (size_t)(b->p - b->start);
}

typedef struct { const u8 *p, *end; u64 buf; int n; } BR;

static inline void br_init(BR *b, const u8 *p, const u8 *end) { b->p = p; b->end = end; b->buf = 0; b->n = 0; }
static inline void br_refill(BR *b) {
    if (b->end - b->p >= 8) {                     /* fast path */
        b->buf |= rd64(b->p) << b->n;
        b->p += (63 - b->n) >> 3;
        b->n |= 56;
    } else {                                      /* end: pad with zeros */
        while (b->n <= 56) {
            b->buf |= (u64)(b->p < b->end ? *b->p++ : 0) << b->n;
            b->n += 8;
        }
    }
}
static inline u32 br_bits(BR *b, int nb) {        /* nb <= 32, refill first */
    u32 v = (u32)(b->buf & ((1ull << nb) - 1));
    b->buf >>= nb; b->n -= nb;
    return v;
}

/* ======================================================================= */
/* Canonical Huffman, length-limited to 11 bits                             */
/* ======================================================================= */
static void huf_build_lengths(const u32 *freq_in, u8 *len) {
    u32 f[256];
    memcpy(f, freq_in, sizeof f);
    for (;;) {
        u64 w[512]; int parent[512], sym[256], m = 0;
        for (int s = 0; s < 256; s++) if (f[s]) { sym[m] = s; w[m] = f[s]; m++; }
        memset(len, 0, 256);
        if (m == 0) return;
        if (m == 1) { len[sym[0]] = 1; return; }
        int alive[512], na = 0, nn = m;
        for (int i = 0; i < m; i++) alive[na++] = i;
        while (na > 1) {                           /* O(n^2), n <= 256: good enough */
            int a = 0, b = 1;
            if (w[alive[b]] < w[alive[a]]) { a = 1; b = 0; }
            for (int i = 2; i < na; i++) {
                if (w[alive[i]] < w[alive[a]]) { b = a; a = i; }
                else if (w[alive[i]] < w[alive[b]]) b = i;
            }
            int x = alive[a], y = alive[b];
            w[nn] = w[x] + w[y]; parent[x] = nn; parent[y] = nn;
            int hi = a > b ? a : b, lo = a > b ? b : a;
            alive[hi] = alive[--na];
            alive[lo] = nn;
            nn++;
        }
        int depth[512]; depth[nn - 1] = 0;
        int maxl = 0;
        for (int i = nn - 2; i >= 0; i--) depth[i] = depth[parent[i]] + 1;
        for (int i = 0; i < m; i++) { len[sym[i]] = (u8)depth[i]; if (depth[i] > maxl) maxl = depth[i]; }
        if (maxl <= HUF_MAXLEN) return;
        for (int s = 0; s < 256; s++) if (f[s]) f[s] = (f[s] + 1) >> 1;   /* flatten, rebuild */
    }
}

static u32 rev_bits(u32 v, int n) { u32 r = 0; while (n--) { r = (r << 1) | (v & 1); v >>= 1; } return r; }

/* Returns 0 for invalid lengths (Kraft inequality violated). */
static int huf_codes(const u8 *len, u32 *code) {
    u32 cnt[16] = {0}, next[16];
    for (int s = 0; s < 256; s++) cnt[len[s]]++;
    cnt[0] = 0;
    u32 kraft = 0;
    for (int b = 1; b <= HUF_MAXLEN; b++) kraft += cnt[b] << (HUF_MAXLEN - b);
    if (kraft > HUF_TABSIZE) return 0;
    u32 c = 0;
    for (int b = 1; b < 16; b++) { c = (c + cnt[b - 1]) << 1; next[b] = c; }
    for (int s = 0; s < 256; s++) if (len[s]) code[s] = rev_bits(next[len[s]]++, len[s]);
    return 1;
}

/* Stream: mode(1) | u32 count | ...   mode 0 = raw, 1 = RLE, 2 = Huffman */
static size_t huf_encode(const u8 *src, u32 n, u8 *dst, size_t cap) {
    if (cap < 5 + (size_t)n + 4) return 0;
    u32 freq[256] = {0};
    for (u32 i = 0; i < n; i++) freq[src[i]]++;
    int used = 0;
    for (int s = 0; s < 256; s++) used += freq[s] != 0;
    if (used == 1) { dst[0] = 1; wr32(dst + 1, n); dst[5] = src[0]; return 6; }
    if (n >= 64 && used > 1) {
        u8 len[256]; u32 code[256];
        huf_build_lengths(freq, len);
        u64 bits = 0;
        for (int s = 0; s < 256; s++) bits += (u64)freq[s] * len[s];
        size_t hsize = 1 + 4 + 4 + 128 + (size_t)((bits + 7) / 8);
        if (hsize < 5 + (size_t)n && huf_codes(len, code)) {
            dst[0] = 2; wr32(dst + 1, n);
            for (int s = 0; s < 256; s += 2) dst[9 + s / 2] = (u8)(len[s] | (len[s + 1] << 4));
            BW bw; bw_init(&bw, dst + 137, dst + cap);
            for (u32 i = 0; i < n; i++) bw_put(&bw, code[src[i]], len[src[i]]);
            size_t nb = bw_flush(&bw);
            if (bw.overflow) return 0;
            wr32(dst + 5, (u32)nb);
            return 137 + nb;
        }
    }
    dst[0] = 0; wr32(dst + 1, n); memcpy(dst + 5, src, n);
    return 5 + n;
}

/* Returns a pointer behind the stream or NULL. */
static const u8 *huf_decode(const u8 *p, const u8 *end, u8 *out, u32 maxn, u32 *nout) {
    if (end - p < 5) return NULL;
    u8 mode = p[0]; u32 n = rd32(p + 1); p += 5;
    if (n > maxn) return NULL;
    *nout = n;
    if (mode == 0) {
        if ((size_t)(end - p) < n) return NULL;
        memcpy(out, p, n); return p + n;
    }
    if (mode == 1) {
        if (end - p < 1) return NULL;
        memset(out, p[0], n); return p + 1;
    }
    if (mode != 2 || end - p < 4 + 128) return NULL;
    u32 nb = rd32(p); p += 4;
    u8 len[256]; u32 code[256];
    for (int s = 0; s < 256; s += 2) { len[s] = p[s / 2] & 15; len[s + 1] = p[s / 2] >> 4; }
    p += 128;
    if ((size_t)(end - p) < nb) return NULL;
    for (int s = 0; s < 256; s++) if (len[s] > HUF_MAXLEN) return NULL;
    if (!huf_codes(len, code)) return NULL;
    u16 tab[HUF_TABSIZE];
    for (u32 i = 0; i < HUF_TABSIZE; i++) tab[i] = 1;   /* gaps (corrupt data): harmless */
    for (int s = 0; s < 256; s++) {
        if (!len[s]) continue;
        u16 e = (u16)((s << 4) | len[s]);
        for (u32 j = code[s]; j < HUF_TABSIZE; j += 1u << len[s]) tab[j] = e;
    }
    BR br; br_init(&br, p, p + nb);
    u32 i = 0;
#define HUF_STEP() do { u16 e = tab[br.buf & (HUF_TABSIZE - 1)]; out[i++] = (u8)(e >> 4); \
                        br.buf >>= (e & 15); br.n -= (e & 15); } while (0)
    while (i + 4 <= n) { br_refill(&br); HUF_STEP(); HUF_STEP(); HUF_STEP(); HUF_STEP(); }
    while (i < n)      { br_refill(&br); HUF_STEP(); }
#undef HUF_STEP
    return p + nb;
}

/* ======================================================================= */
/* Pre-filters                                                             */
/* ======================================================================= */
static void shuffle(const u8 *in, u8 *out, size_t n, int s) {
    size_t m = n / s;
    for (int k = 0; k < s; k++) { u8 *o = out + k * m; const u8 *q = in + k; for (size_t j = 0; j < m; j++) o[j] = q[j * s]; }
    memcpy(out + m * s, in + m * s, n - m * s);
}
static void unshuffle(const u8 *in, u8 *out, size_t n, int s) {
    size_t m = n / s;
    for (int k = 0; k < s; k++) { const u8 *q = in + k * m; u8 *o = out + k; for (size_t j = 0; j < m; j++) o[j * s] = q[j]; }
    memcpy(out + m * s, in + m * s, n - m * s);
}
static void filt_enc(int f, int s, const u8 *in, u8 *out, size_t n) {
    switch (f) {
    case F_DELTA:
        for (size_t i = 0; i < n; i++) out[i] = i >= (size_t)s ? (u8)(in[i] - in[i - s]) : in[i];
        break;
    case F_SHUF: shuffle(in, out, n, s); break;
    case F_SHUFDELTA:
        shuffle(in, out, n, s);
        for (size_t i = n; i-- > 1;) out[i] = (u8)(out[i] - out[i - 1]);
        break;
    default: memcpy(out, in, n);
    }
}
static void filt_dec(int f, int s, u8 *in, u8 *out, size_t n) {   /* in may be modified */
    switch (f) {
    case F_DELTA:
        for (size_t i = 0; i < n; i++) out[i] = i >= (size_t)s ? (u8)(in[i] + out[i - s]) : in[i];
        break;
    case F_SHUF: unshuffle(in, out, n, s); break;
    case F_SHUFDELTA:
        for (size_t i = 1; i < n; i++) in[i] = (u8)(in[i] + in[i - 1]);
        unshuffle(in, out, n, s);
        break;
    default: memcpy(out, in, n);
    }
}

/* Order-0 entropy (bits) of the delta(s) sequence of a sample. */
static double delta_entropy(const u8 *p, size_t n, int s) {
    u32 h[256] = {0};
    for (size_t i = s; i < n; i++) h[(u8)(p[i] - p[i - s])]++;
    double tot = (double)(n - s), e = 0;
    for (int k = 0; k < 256; k++) if (h[k]) e -= h[k] * log2(h[k] / tot);
    return e;
}

static double order0_entropy(const u8 *p, size_t n) {
    u32 h[256] = {0};
    for (size_t i = 0; i < n; i++) h[p[i]]++;
    double e = 0;
    for (int k = 0; k < 256; k++) if (h[k]) e -= h[k] * log2((double)h[k] / n);
    return e;
}

/* ======================================================================= */
/* LZ77-Encoder                                                            */
/* ======================================================================= */
typedef struct { int depth, lazy, nice; } LevelParams;
static const LevelParams LEVELS[10] = {
    {4, 0, 32}, {4, 0, 32}, {8, 0, 32}, {16, 1, 64}, {32, 1, 128},
    {64, 1, 128}, {128, 1, 256}, {256, 1, 512}, {512, 1, 1024}, {1024, 1, 4096},
};

typedef struct {
    u32 *head, *prev;
    u8 *lit, *tok, *ofc, *ext, *obits;
    u8 *filtered, *scratch;
    size_t scratch_cap;
    u32 nlit, nseq, next;
    BW ob;
    u32 rep0, rep1;
    int litcost;          /* estimated literal cost in 1/16 bit */
} Enc;

typedef struct { u32 len, off; int gain; } Match;

static inline u32 match_len(const u8 *a, const u8 *b, const u8 *end) {
    const u8 *s = a;
    while (a + 8 <= end) {
        u64 x = rd64(a) ^ rd64(b);
        if (x) return (u32)(a - s) + (__builtin_ctzll(x) >> 3);
        a += 8; b += 8;
    }
    while (a < end && *a == *b) { a++; b++; }
    return (u32)(a - s);
}

/* Cost model in 1/16 bit: literal = order-0 entropy of the block (adaptive),
 * match = token + offset code + extra bits (estimated). */
static inline int match_gain(u32 len, u32 off, u32 rep0, u32 rep1, int litcost) {
    int cost = (off == rep0 || off == rep1) ? 4 * 16 : (9 + bitlen32(off)) * 16;
    return (int)len * litcost - cost;
}

static inline u32 hash4(const u8 *p) { return (rd32(p) * 2654435761u) >> (32 - HASH_BITS); }

static Match find_match(Enc *e, const u8 *base, u32 ip, u32 n, const LevelParams *lp) {
    Match best = {0, 0, 0};
    const u8 *cur = base + ip, *end = base + n;
    u32 reps[2] = {e->rep0, e->rep1};
    for (int r = 0; r < 2; r++) {
        u32 off = reps[r];
        if (off > ip) continue;
        u32 l = match_len(cur, cur - off, end);
        if (l >= MINMATCH) {
            int g = match_gain(l, off, e->rep0, e->rep1, e->litcost);
            if (g > best.gain) { best.len = l; best.off = off; best.gain = g; }
        }
    }
    u32 cand = e->head[hash4(cur)];
    int depth = lp->depth;
    u32 avail = n - ip;
    while (cand != NIL && depth-- > 0) {
        const u8 *c = base + cand;
        if (best.len < avail && c[best.len] == cur[best.len]) {
            u32 l = match_len(cur, c, end);
            if (l >= MINMATCH) {
                u32 off = ip - cand;
                int g = match_gain(l, off, e->rep0, e->rep1, e->litcost);
                if (g > best.gain) { best.len = l; best.off = off; best.gain = g; if (l >= (u32)lp->nice) break; }
            }
        }
        cand = e->prev[cand];
    }
    return best;
}

static inline void put_varint(Enc *e, u32 v) {
    while (v >= 128) { e->ext[e->next++] = (u8)(v | 128); v >>= 7; }
    e->ext[e->next++] = (u8)v;
}

static void emit_seq(Enc *e, const u8 *lits, u32 ll, u32 ml, u32 off) {
    memcpy(e->lit + e->nlit, lits, ll); e->nlit += ll;
    u32 m = ml - MINMATCH;
    e->tok[e->nseq] = (u8)(((ll < 15 ? ll : 15) << 4) | (m < 15 ? m : 15));
    if (ll >= 15) put_varint(e, ll - 15);
    if (m >= 15)  put_varint(e, m - 15);
    u8 oc;
    if (off == e->rep0) oc = 0;
    else if (off == e->rep1) { oc = 1; e->rep1 = e->rep0; e->rep0 = off; }
    else {
        int b = bitlen32(off);
        oc = (u8)(2 + b);
        bw_put(&e->ob, off - (1u << b), b);
        e->rep1 = e->rep0; e->rep0 = off;
    }
    e->ofc[e->nseq++] = oc;
}

/* Compresses an (already filtered) block. 0 = does not fit. */
static size_t lz_block(Enc *e, const u8 *src, u32 n, int level, u8 *out, size_t cap) {
    const LevelParams *lp = &LEVELS[level];
    for (u32 i = 0; i < (1u << HASH_BITS); i++) e->head[i] = NIL;
    e->nlit = e->nseq = e->next = 0;
    e->rep0 = 1; e->rep1 = 4;
    {   /* estimate the literal cost from the order-0 entropy (+0.5 bit Huffman overhead) */
        double en = n > 16 ? order0_entropy(src, n) : 8.0 * n;
        e->litcost = (int)(16.0 * (en / (n ? n : 1) + 0.5));
        if (e->litcost < 16) e->litcost = 16;
        if (e->litcost > 8 * 16) e->litcost = 8 * 16;
    }
    bw_init(&e->ob, e->obits, e->obits + BLOCK_SIZE);

    u32 ip = 0, anchor = 0, ins = 0;
    if (n >= MINMATCH + 1) {
        u32 limit = n - MINMATCH;
        while (ip <= limit) {
            for (; ins < ip; ins++) { u32 h = hash4(src + ins); e->prev[ins] = e->head[h]; e->head[h] = ins; }
            Match m = find_match(e, src, ip, n, lp);
            if (m.gain <= 0) { ip++; continue; }
            while (lp->lazy && ip + 1 <= limit) {           /* Lazy Matching */
                for (; ins < ip + 1; ins++) { u32 h = hash4(src + ins); e->prev[ins] = e->head[h]; e->head[h] = ins; }
                Match m2 = find_match(e, src, ip + 1, n, lp);
                if (m2.gain - e->litcost > m.gain) { ip++; m = m2; } else break;
            }
            emit_seq(e, src + anchor, ip - anchor, m.len, m.off);
            ip += m.len; anchor = ip;
        }
    }
    memcpy(e->lit + e->nlit, src + anchor, n - anchor);
    e->nlit += n - anchor;
    size_t obytes = bw_flush(&e->ob);
    if (e->ob.overflow) return 0;

    u8 *p = out, *pend = out + cap;
    if (pend - p < 4) return 0;
    wr32(p, e->nseq); p += 4;
    size_t k;
    if (!(k = huf_encode(e->lit, e->nlit, p, pend - p))) return 0;
    p += k;
    if (!(k = huf_encode(e->tok, e->nseq, p, pend - p))) return 0;
    p += k;
    if (!(k = huf_encode(e->ofc, e->nseq, p, pend - p))) return 0;
    p += k;
    if ((size_t)(pend - p) < 8 + e->next + obytes) return 0;
    wr32(p, e->next); p += 4; memcpy(p, e->ext, e->next); p += e->next;
    wr32(p, (u32)obytes); p += 4; memcpy(p, e->obits, obytes); p += obytes;
    return (size_t)(p - out);
}

static size_t filtered_size(Enc *e, const u8 *src, u32 n, int f, int s, int level) {
    filt_enc(f, s, src, e->filtered, n);
    size_t r = lz_block(e, e->filtered, n, level, e->scratch, e->scratch_cap);
    return r ? r : (size_t)-1;
}

/* Chooses the best filter via stride detection + trial compression. */
static void choose_filter(Enc *e, const u8 *src, u32 n, int *bf, int *bs) {
    *bf = F_NONE; *bs = 0;
    if (n < 4096) return;
    size_t sample = n < 65536 ? n : 65536;
    int best_s = 1; double best_e = 1e300;
    for (int s = 1; s <= 32; s++) {
        double en = delta_entropy(src, sample, s);
        if (en < best_e * 0.995) { best_e = en; best_s = s; }   /* prefer small strides */
    }
    struct { int f, s; } cand[6]; int nc = 0;
    cand[nc].f = F_NONE;  cand[nc++].s = 0;
    cand[nc].f = F_DELTA; cand[nc++].s = best_s;
    if (best_s > 1) { cand[nc].f = F_SHUFDELTA; cand[nc++].s = best_s; cand[nc].f = F_SHUF; cand[nc++].s = best_s; }
    if (best_s > 4 && best_s % 4 == 0) { cand[nc].f = F_SHUFDELTA; cand[nc++].s = 4; }
    if (best_s != 1) { cand[nc].f = F_DELTA; cand[nc++].s = 1; }
    u32 tn = n < TRIAL_BYTES ? n : TRIAL_BYTES;
    size_t best = (size_t)-1;
    for (int i = 0; i < nc; i++) {
        size_t sz = filtered_size(e, src, tn, cand[i].f, cand[i].s, 2);
        if (sz < best) { best = sz; *bf = cand[i].f; *bs = cand[i].s; }
    }
}

size_t glz_bound(size_t n) { return FILE_HDR + n + (n / BLOCK_SIZE + 1) * BLK_HDR + 64; }

size_t glz_compress(const void *src_, size_t n, void *dst_, size_t cap, int level, unsigned flags) {
    const u8 *src = (const u8 *)src_;
    u8 *dst = (u8 *)dst_;
    if (level < 1) level = 1;
    if (level > 9) level = 9;
    memset(&g_stats, 0, sizeof g_stats);
    if (cap < FILE_HDR) return 0;
    memcpy(dst, "GLZ1", 4);
    u64 n64 = n; memcpy(dst + 4, &n64, 8);
    size_t op = FILE_HDR;

    Enc e;
    e.scratch_cap = 4 * (size_t)BLOCK_SIZE + 65536;
    e.head = malloc(sizeof(u32) << HASH_BITS);
    e.prev = malloc(sizeof(u32) * BLOCK_SIZE);
    e.lit = malloc(BLOCK_SIZE);
    e.tok = malloc(BLOCK_SIZE / MINMATCH + 16);
    e.ofc = malloc(BLOCK_SIZE / MINMATCH + 16);
    e.ext = malloc(3 * (size_t)BLOCK_SIZE);
    e.obits = malloc(BLOCK_SIZE);
    e.filtered = malloc(BLOCK_SIZE);
    e.scratch = malloc(e.scratch_cap);
    size_t result = 0;
    if (!e.head || !e.prev || !e.lit || !e.tok || !e.ofc || !e.ext || !e.obits || !e.filtered || !e.scratch)
        goto done;

    for (size_t pos = 0; pos < n; pos += BLOCK_SIZE) {
        u32 bn = (u32)((n - pos) < BLOCK_SIZE ? (n - pos) : BLOCK_SIZE);
        const u8 *b = src + pos;
        int f = F_NONE, s = 0;
        if (!(flags & GLZ_FLAG_NO_FILTER)) choose_filter(&e, b, bn, &f, &s);
        filt_enc(f, s, b, e.filtered, bn);
        size_t k = lz_block(&e, e.filtered, bn, level, e.scratch, e.scratch_cap);
        if (cap - op < BLK_HDR) goto done;
        if (k && k < bn) {
            if (cap - op - BLK_HDR < k) goto done;
            dst[op] = (u8)f; dst[op + 1] = (u8)s; dst[op + 2] = 1;
            wr32(dst + op + 3, bn); wr32(dst + op + 7, (u32)k);
            memcpy(dst + op + BLK_HDR, e.scratch, k);
            op += BLK_HDR + k;
            g_stats.filter_count[f]++; g_stats.last_stride = (unsigned)s;
        } else {                                         /* not compressible: store raw */
            if (cap - op - BLK_HDR < bn) goto done;
            dst[op] = 0; dst[op + 1] = 0; dst[op + 2] = 0;
            wr32(dst + op + 3, bn); wr32(dst + op + 7, bn);
            memcpy(dst + op + BLK_HDR, b, bn);
            op += BLK_HDR + bn;
            g_stats.filter_count[4]++;
        }
    }
    result = op;
done:
    free(e.head); free(e.prev); free(e.lit); free(e.tok); free(e.ofc);
    free(e.ext); free(e.obits); free(e.filtered); free(e.scratch);
    return result;
}

/* ======================================================================= */
/* Decoder                                                                 */
/* ======================================================================= */
static inline int rd_varint(const u8 **pp, const u8 *end, u32 *v) {
    u32 r = 0; int sh = 0; const u8 *p = *pp;
    while (p < end) {
        u8 c = *p++;
        r |= (u32)(c & 127) << sh;
        if (!(c & 128)) { *v = r; *pp = p; return 1; }
        sh += 7;
        if (sh > 28) return 0;
    }
    return 0;
}

typedef struct { u8 *lit, *tok, *ofc; } Dec;

static int lz_decode_block(Dec *d, const u8 *p, const u8 *end, u8 *out, u32 rawsize) {
    if (end - p < 4) return 0;
    u32 nseq = rd32(p); p += 4;
    if (nseq > rawsize / MINMATCH + 1) return 0;
    u32 nlit, nt, no;
    if (!(p = huf_decode(p, end, d->lit, rawsize, &nlit))) return 0;
    if (!(p = huf_decode(p, end, d->tok, nseq, &nt)) || nt != nseq) return 0;
    if (!(p = huf_decode(p, end, d->ofc, nseq, &no)) || no != nseq) return 0;
    if (end - p < 4) return 0;
    u32 extlen = rd32(p); p += 4;
    if ((size_t)(end - p) < extlen) return 0;
    const u8 *ep = p, *eend = p + extlen; p += extlen;
    if (end - p < 4) return 0;
    u32 oblen = rd32(p); p += 4;
    if ((size_t)(end - p) < oblen) return 0;
    BR ob; br_init(&ob, p, p + oblen);

    const u8 *lp = d->lit, *lend = d->lit + nlit;
    u8 *op = out, *oend = out + rawsize;
    u32 rep0 = 1, rep1 = 4;
    for (u32 i = 0; i < nseq; i++) {
        u32 t = d->tok[i];
        u32 ll = t >> 4;
        if (ll == 15) { u32 v; if (!rd_varint(&ep, eend, &v)) return 0; ll += v; }
        if (ll > (u32)(lend - lp) || ll > (u32)(oend - op)) return 0;
        if (ll <= 16 && lend - lp >= 16 && oend - op >= 16) memcpy(op, lp, 16);
        else memcpy(op, lp, ll);
        op += ll; lp += ll;

        u32 ml = t & 15;
        if (ml == 15) { u32 v; if (!rd_varint(&ep, eend, &v)) return 0; ml += v; }
        ml += MINMATCH;
        u32 oc = d->ofc[i], off;
        if (oc == 0) off = rep0;
        else if (oc == 1) { off = rep1; rep1 = rep0; rep0 = off; }
        else {
            int b = (int)oc - 2;
            if (b > 24) return 0;
            br_refill(&ob);
            off = (1u << b) + br_bits(&ob, b);
            rep1 = rep0; rep0 = off;
        }
        if (off > (u32)(op - out) || ml > (u32)(oend - op)) return 0;
        const u8 *m = op - off;
        if (off >= 8 && (size_t)(oend - op) >= (size_t)ml + 8) {   /* 8-byte wild copy */
            u8 *dend = op + ml;
            do { memcpy(op, m, 8); op += 8; m += 8; } while (op < dend);
            op = dend;
        } else if (off == 1) {
            memset(op, op[-1], ml); op += ml;
        } else {
            for (u32 k = 0; k < ml; k++) op[k] = m[k];
            op += ml;
        }
    }
    u32 rest = (u32)(oend - op);
    if (rest != (u32)(lend - lp)) return 0;
    memcpy(op, lp, rest);
    return 1;
}

size_t glz_decompressed_size(const void *src_, size_t n) {
    const u8 *src = (const u8 *)src_;
    if (n < FILE_HDR || memcmp(src, "GLZ1", 4)) return (size_t)-1;
    u64 v; memcpy(&v, src + 4, 8);
    return (size_t)v;
}

size_t glz_decompress(const void *src_, size_t n, void *dst_, size_t cap) {
    const u8 *src = (const u8 *)src_, *end = src + n;
    u8 *dst = (u8 *)dst_;
    size_t total = glz_decompressed_size(src, n);
    if (total == (size_t)-1 || total > cap) return (size_t)-1;
    const u8 *p = src + FILE_HDR;
    Dec d;
    d.lit = malloc(BLOCK_SIZE);
    d.tok = malloc(BLOCK_SIZE / MINMATCH + 16);
    d.ofc = malloc(BLOCK_SIZE / MINMATCH + 16);
    u8 *tmp = malloc(BLOCK_SIZE);
    size_t op = 0, result = (size_t)-1;
    if (!d.lit || !d.tok || !d.ofc || !tmp) goto done;
    while (op < total) {
        if (end - p < BLK_HDR) goto done;
        int f = p[0], s = p[1], mode = p[2];
        u32 raw = rd32(p + 3), plen = rd32(p + 7);
        p += BLK_HDR;
        if (raw == 0 || raw > BLOCK_SIZE || raw > total - op || (size_t)(end - p) < plen) goto done;
        if (f > F_SHUFDELTA || (f != F_NONE && (s < 1 || s > 32))) goto done;
        if (mode == 0) {
            if (plen != raw) goto done;
            memcpy(dst + op, p, raw);
        } else if (mode == 1) {
            u8 *target = f == F_NONE ? dst + op : tmp;
            if (!lz_decode_block(&d, p, p + plen, target, raw)) goto done;
            if (f != F_NONE) filt_dec(f, s, tmp, dst + op, raw);
        } else goto done;
        p += plen; op += raw;
    }
    result = total;
done:
    free(d.lit); free(d.tok); free(d.ofc); free(tmp);
    return result;
}
