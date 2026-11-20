#include "filters.h"
#include <string.h>
#include <stdlib.h>

typedef uint8_t u8; typedef uint32_t u32; typedef uint64_t u64;

const char *const FILTER_NAMES[FT_COUNT] = {"none", "delta", "shuffle", "shuffle+delta", "x86-bcj", "split", "word-pred", "x86-split"};

static inline u32 rd32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }
static inline void wr32(u8 *p, u32 v) { memcpy(p, &v, 4); }

/* ---- Byte-Planes ---------------------------------------------------- */
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

/* ---- x86 BCJ ---------------------------------------------------------
 * E8 (CALL rel32) / E9 (JMP rel32): if the top byte of the operand is 0x00
 * or 0xFF (small relative jump), the target is made absolute:
 * v = (rel + pos) mod 2^25, sign-extended. The top byte stays
 * 0x00/0xFF, so the decoder makes exactly the same decisions -> bijective.
 * Effect: calls of the same function from different places become
 * identical byte sequences that LZ finds. */
static void bcj(u8 *b, size_t n, u64 off, int enc) {
    /* After every E8/E9 the 4 following bytes are always skipped: this way no
     * later transformation can change a byte that went into an earlier
     * decision (otherwise the decoder would decide differently). */
    size_t i = 0;
    while (i + 5 <= n) {
        if ((b[i] & 0xFE) == 0xE8) {
            if (b[i + 4] == 0x00 || b[i + 4] == 0xFF) {
                u32 v = rd32(b + i + 1), p = (u32)(off + i);
                v = enc ? v + p : v - p;
                v &= 0x01FFFFFFu;
                if (v & 0x01000000u) v |= 0xFE000000u;
                wr32(b + i + 1, v);
            }
            i += 5;
        } else i++;
    }
}

/* ---- x86 with separate streams (BCJ2 principle) ----------------------
 * CALL (E8), JMP (E9) and conditional jumps (0F 80..8F) have a 4-byte
 * operand. These operands are taken out of the code and written together
 * at the end (in reverse order, so the decoder needs no length field; the
 * total length stays the same). There they are stored as absolute targets
 * in big endian: calls of the same function become identical, and the code
 * stream keeps only the instruction bytes, which LZ finds better.
 * The absolute conversion uses the same bijective 25-bit rule as FT_BCJ. */
static inline u32 x86_abs(u32 v, u32 pos, int enc) {
    if ((v >> 24) != 0x00 && (v >> 24) != 0xFF) return v;     /* not a small jump: unchanged */
    v = enc ? v + pos : v - pos;
    v &= 0x01FFFFFFu;
    if (v & 0x01000000u) v |= 0xFE000000u;
    return v;
}
static inline void wr_be32(u8 *p, u32 v) { p[0] = (u8)(v >> 24); p[1] = (u8)(v >> 16); p[2] = (u8)(v >> 8); p[3] = (u8)v; }
static inline u32 rd_be32(const u8 *p) { return (u32)p[0] << 24 | (u32)p[1] << 16 | (u32)p[2] << 8 | p[3]; }

static void x86s_enc(const u8 *in, u8 *out, size_t n, u64 off) {
    size_t i = 0, mp = 0, k = 0;
    while (i < n) {
        size_t ol = 0;
        if ((in[i] & 0xFE) == 0xE8 && i + 5 <= n) ol = 1;
        else if (in[i] == 0x0F && i + 6 <= n && (in[i + 1] & 0xF0) == 0x80) ol = 2;
        if (!ol) { out[mp++] = in[i++]; continue; }
        memcpy(out + mp, in + i, ol); mp += ol;
        u32 v = rd32(in + i + ol);
        v = x86_abs(v, (u32)(off + i + ol + 4), 1);
        wr_be32(out + n - 4 * (++k), v);
        i += ol + 4;
    }
}
static void x86s_dec(const u8 *in, u8 *out, size_t n, u64 off) {
    size_t i = 0, mp = 0, k = 0;
    while (i < n) {
        u8 b = in[mp];
        size_t ol = 0;
        if ((b & 0xFE) == 0xE8 && i + 5 <= n) ol = 1;
        else if (b == 0x0F && i + 6 <= n && (in[mp + 1] & 0xF0) == 0x80) ol = 2;
        if (!ol) { out[i++] = in[mp++]; continue; }
        memcpy(out + i, in + mp, ol); mp += ol;
        u32 v = rd_be32(in + n - 4 * (++k));
        wr32(out + i + ol, x86_abs(v, (u32)(off + i + ol + 4), 0));
        i += ol + 4;
    }
}

/* ---- Field split ----------------------------------------------------- */
static void split_layout(const fspec *f, size_t n, u64 off, size_t *head, size_t *nrec, u32 *R) {
    u32 r = 0;
    for (int k = 0; k < f->nfields; k++) r += f->fields[k];
    size_t h;
    if (off < f->skip) h = (size_t)(f->skip - off);
    else h = (size_t)((r - (off - f->skip) % r) % r);
    if (h > n) h = n;
    *head = h; *nrec = (n - h) / r; *R = r;
}
static void split_enc(const fspec *f, const u8 *in, u8 *out, size_t n, u64 off) {
    size_t head, nrec; u32 R;
    split_layout(f, n, off, &head, &nrec, &R);
    memcpy(out, in, head);
    u8 *o = out + head; u32 fo = 0;
    for (int k = 0; k < f->nfields; k++) {
        u32 w = f->fields[k];
        for (size_t j = 0; j < nrec; j++) { memcpy(o, in + head + j * R + fo, w); o += w; }
        fo += w;
    }
    size_t used = head + nrec * R;
    memcpy(o, in + used, n - used);
}
static void split_dec(const fspec *f, const u8 *in, u8 *out, size_t n, u64 off) {
    size_t head, nrec; u32 R;
    split_layout(f, n, off, &head, &nrec, &R);
    memcpy(out, in, head);
    const u8 *q = in + head; u32 fo = 0;
    for (int k = 0; k < f->nfields; k++) {
        u32 w = f->fields[k];
        for (size_t j = 0; j < nrec; j++) { memcpy(out + head + j * R + fo, q, w); q += w; }
        fo += w;
    }
    size_t used = head + nrec * R;
    memcpy(out + used, q, n - used);
}

/* ---- Word prediction -------------------------------------------------
 * Interprets the data as W-bit integers (W = 2 or 4, little endian),
 * predicts every word from the word one record (stride bytes) earlier and
 * stores only the residual (modulo 2^W). Then the bytes are separated into
 * planes by their position in the word (or in the record): the high bytes of
 * the residuals are almost always 0x00/0xFF and compress extremely well. */
static inline u32 ldw(const u8 *p, int W) { if (W == 2) { uint16_t v; memcpy(&v, p, 2); return v; } u32 v; memcpy(&v, p, 4); return v; }
static inline void stw(u8 *p, u32 v, int W) { if (W == 2) { uint16_t x = (uint16_t)v; memcpy(p, &x, 2); } else memcpy(p, &v, 4); }

static void wpred_core(const fspec *f, u8 *buf, size_t n, int enc) {
    int W = f->fields[0], ord = f->fields[1];
    size_t s = f->stride, nw = n / W, k = s / W;          /* distance in words */
    if (enc) {
        for (size_t j = nw; j-- > 0;) {
            u32 x = ldw(buf + j * W, W), p = 0;
            if (j >= k) { u32 a = ldw(buf + (j - k) * W, W); p = a; if (ord == 2 && j >= 2 * k) p = 2 * a - ldw(buf + (j - 2 * k) * W, W); }
            stw(buf + j * W, x - p, W);
        }
    } else {
        for (size_t j = 0; j < nw; j++) {
            u32 r = ldw(buf + j * W, W), p = 0;
            if (j >= k) { u32 a = ldw(buf + (j - k) * W, W); p = a; if (ord == 2 && j >= 2 * k) p = 2 * a - ldw(buf + (j - 2 * k) * W, W); }
            stw(buf + j * W, r + p, W);
        }
    }
}

void filter_encode(const fspec *f, const u8 *in, u8 *out, size_t n, u64 off) {
    int s = f->stride;
    switch (f->id) {
    case FT_DELTA:
        for (size_t i = 0; i < n; i++) out[i] = i >= (size_t)s ? (u8)(in[i] - in[i - s]) : in[i];
        break;
    case FT_SHUF: shuffle(in, out, n, s); break;
    case FT_SHUFDELTA:
        shuffle(in, out, n, s);
        for (size_t i = n; i-- > 1;) out[i] = (u8)(out[i] - out[i - 1]);
        break;
    case FT_BCJ: memcpy(out, in, n); bcj(out, n, off, 1); break;
    case FT_SPLIT: split_enc(f, in, out, n, off); break;
    case FT_X86S: x86s_enc(in, out, n, off); break;
    case FT_WPRED: {
        u8 *t = malloc(n ? n : 1);
        memcpy(t, in, n); wpred_core(f, t, n, 1);
        shuffle(t, out, n, f->fields[2] ? s : f->fields[0]);
        free(t);
        break;
    }
    default: memcpy(out, in, n);
    }
}

void filter_decode(const fspec *f, u8 *in, u8 *out, size_t n, u64 off) {
    int s = f->stride;
    switch (f->id) {
    case FT_DELTA:
        for (size_t i = 0; i < n; i++) out[i] = i >= (size_t)s ? (u8)(in[i] + out[i - s]) : in[i];
        break;
    case FT_SHUF: unshuffle(in, out, n, s); break;
    case FT_SHUFDELTA:
        for (size_t i = 1; i < n; i++) in[i] = (u8)(in[i] + in[i - 1]);
        unshuffle(in, out, n, s);
        break;
    case FT_BCJ: memcpy(out, in, n); bcj(out, n, off, 0); break;
    case FT_SPLIT: split_dec(f, in, out, n, off); break;
    case FT_X86S: x86s_dec(in, out, n, off); break;
    case FT_WPRED:
        unshuffle(in, out, n, f->fields[2] ? s : f->fields[0]);
        wpred_core(f, out, n, 0);
        break;
    default: memcpy(out, in, n);
    }
}
