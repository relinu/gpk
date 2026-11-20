#include "filters.h"
#include <string.h>
#include <stdlib.h>

typedef uint8_t u8; typedef uint32_t u32; typedef uint64_t u64;

const char *const FILTER_NAMES[FT_COUNT] = {"none", "delta", "shuffle", "shuffle+delta", "x86-bcj", "split", "wort-praediktion", "x86-split"};

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
 * E8 (CALL rel32) / E9 (JMP rel32): Ist das oberste Byte des Operanden 0x00
 * oder 0xFF (kleiner relativer Sprung), wird das Ziel absolut gemacht:
 * v = (rel + pos) mod 2^25, vorzeichenerweitert. Das oberste Byte bleibt dabei
 * 0x00/0xFF, also trifft der Decoder exakt dieselben Entscheidungen -> bijektiv.
 * Effekt: Aufrufe derselben Funktion aus verschiedenen Stellen werden zu
 * identischen Bytefolgen, die LZ findet. */
static void bcj(u8 *b, size_t n, u64 off, int enc) {
    /* Nach jedem E8/E9 werden die 4 Folgebytes immer übersprungen: So kann
     * keine spätere Umwandlung ein Byte verändern, das in eine frühere
     * Entscheidung eingeflossen ist (sonst entscheidet der Decoder anders). */
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

/* ---- x86 mit getrennten Strömen (BCJ2-Prinzip) -----------------------
 * CALL (E8), JMP (E9) und bedingte Sprünge (0F 80..8F) haben einen 4-Byte-
 * Operanden. Diese Operanden werden aus dem Code herausgelöst und gesammelt
 * ans Ende geschrieben (rückwärts aufgereiht, damit der Decoder keine
 * Längenangabe braucht; Gesamtlänge bleibt gleich). Dort stehen sie als
 * absolute Ziele in Big Endian: Aufrufe derselben Funktion werden identisch,
 * und im Code-Strom bleiben nur die Befehlsbytes, die LZ besser findet.
 * Die Absolut-Umrechnung nutzt dieselbe bijektive 25-Bit-Regel wie FT_BCJ. */
static inline u32 x86_abs(u32 v, u32 pos, int enc) {
    if ((v >> 24) != 0x00 && (v >> 24) != 0xFF) return v;     /* kein kleiner Sprung: unverändert */
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

/* ---- Feld-Split ------------------------------------------------------ */
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

/* ---- Wort-Prädiktion -------------------------------------------------
 * Interpretiert die Daten als W-Bit-Ganzzahlen (W = 2 oder 4, Little Endian),
 * sagt jedes Wort aus dem Wort einen Datensatz (stride Bytes) zuvor voraus und
 * speichert nur den Rest (modulo 2^W). Danach werden die Bytes nach Position
 * im Wort (oder im Datensatz) in Planes getrennt: Die hohen Bytes der Reste
 * sind fast immer 0x00/0xFF und komprimieren extrem gut. */
static inline u32 ldw(const u8 *p, int W) { if (W == 2) { uint16_t v; memcpy(&v, p, 2); return v; } u32 v; memcpy(&v, p, 4); return v; }
static inline void stw(u8 *p, u32 v, int W) { if (W == 2) { uint16_t x = (uint16_t)v; memcpy(p, &x, 2); } else memcpy(p, &v, 4); }

static void wpred_core(const fspec *f, u8 *buf, size_t n, int enc) {
    int W = f->fields[0], ord = f->fields[1];
    size_t s = f->stride, nw = n / W, k = s / W;          /* Abstand in Wörtern */
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
