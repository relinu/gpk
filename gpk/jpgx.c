/* JPGX – siehe jpgx.h. Nur Ganzzahl-Arithmetik. */
#include "jpgx.h"
#include <stdlib.h>
#include <string.h>

typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef int16_t i16; typedef uint64_t u64;

#define MAX_PIXELS (1ull << 28)

/* ======================================================================= */
/* JPEG-Struktur                                                           */
/* ======================================================================= */
typedef struct {
    int present;
    u8 bits[17], vals[256]; int nvals;
    int maxcode[18], valptr[17], mincode[17];     /* Dekodierung */
    u16 code[256]; u8 len[256];                    /* Kodierung (je Symbol) */
} Huff;
typedef struct { int id, h, v, td, ta; int bw, bh; i16 *coef; } Comp;
typedef struct {
    int X, Y, nf, ns, ri, hmax, vmax, mcux, mcuy, interleaved;
    Comp c[4]; int scan[4];                        /* Komponenten in Scan-Reihenfolge (Index in c) */
    Huff dc[4], ac[4];
    size_t data_start;                             /* Beginn der entropiekodierten Daten */
} JInfo;

static u32 be16(const u8 *p) { return (u32)p[0] << 8 | p[1]; }

static int build_huff(Huff *h) {
    int code = 0, k = 0;
    memset(h->len, 0, sizeof h->len);
    for (int l = 1; l <= 16; l++) {
        h->valptr[l] = k; h->mincode[l] = code;
        for (int i = 0; i < h->bits[l]; i++) {
            if (k >= h->nvals) return 0;
            u8 s = h->vals[k++];
            h->code[s] = (u16)code; h->len[s] = (u8)l; code++;
        }
        h->maxcode[l] = h->bits[l] ? code - 1 : -1;
        if (code > (1 << l)) return 0;                 /* kein gültiger Präfixcode */
        code <<= 1;
    }
    h->maxcode[17] = 0x7FFFFFFF;
    h->present = 1;
    return 1;
}

/* Kopf bis einschließlich SOS lesen. Liefert 1 bei unterstützter Datei. */
static int parse_header(const u8 *d, size_t n, JInfo *J) {
    memset(J, 0, sizeof *J);
    if (n < 4 || d[0] != 0xFF || d[1] != 0xD8) return 0;
    size_t p = 2; int have_sof = 0;
    for (;;) {
        if (p + 4 > n || d[p] != 0xFF) return 0;
        while (p < n && d[p] == 0xFF) p++;            /* Füllbytes */
        if (p >= n) return 0;
        u8 m = d[p++];
        if (m == 0xD8 || m == 0xD9 || (m >= 0xD0 && m <= 0xD7) || m == 0x01) return 0;
        if (p + 2 > n) return 0;
        u32 L = be16(d + p);
        if (L < 2 || p + L > n) return 0;
        const u8 *s = d + p + 2; u32 sl = L - 2;
        if (m == 0xC0 || m == 0xC1) {
            if (have_sof || sl < 6 || s[0] != 8) return 0;
            J->Y = (int)be16(s + 1); J->X = (int)be16(s + 3); J->nf = s[5];
            if (!J->X || !J->Y || J->nf < 1 || J->nf > 4 || sl < 6u + 3u * J->nf) return 0;
            if ((u64)J->X * J->Y > MAX_PIXELS) return 0;
            for (int i = 0; i < J->nf; i++) {
                Comp *c = &J->c[i];
                c->id = s[6 + 3 * i]; c->h = s[7 + 3 * i] >> 4; c->v = s[7 + 3 * i] & 15;
                if (c->h < 1 || c->h > 4 || c->v < 1 || c->v > 4) return 0;
                if (c->h > J->hmax) J->hmax = c->h;
                if (c->v > J->vmax) J->vmax = c->v;
            }
            have_sof = 1;
        } else if ((m >= 0xC2 && m <= 0xCF && m != 0xC4 && m != 0xC8 && m != 0xCC)) {
            return 0;                                   /* progressiv, verlustfrei, arithmetisch */
        } else if (m == 0xC4) {
            u32 q = 0;
            while (q < sl) {
                if (q + 17 > sl) return 0;
                int tc = s[q] >> 4, th = s[q] & 15;
                if (tc > 1 || th > 3) return 0;
                Huff *h = tc ? &J->ac[th] : &J->dc[th];
                int tot = 0;
                h->bits[0] = 0;
                for (int l = 1; l <= 16; l++) { h->bits[l] = s[q + l]; tot += h->bits[l]; }
                if (tot > 256 || q + 17 + (u32)tot > sl) return 0;
                memcpy(h->vals, s + q + 17, (size_t)tot); h->nvals = tot;
                if (!build_huff(h)) return 0;
                q += 17 + (u32)tot;
            }
        } else if (m == 0xDD) {
            if (sl < 2) return 0;
            J->ri = (int)be16(s);
        } else if (m == 0xDA) {
            if (!have_sof || sl < 1) return 0;
            J->ns = s[0];
            if (J->ns < 1 || J->ns > 4 || sl != 4u + 2u * J->ns) return 0;
            if (J->ns != J->nf) return 0;               /* nur ein Scan mit allen Komponenten */
            for (int i = 0; i < J->ns; i++) {
                int cid = s[1 + 2 * i], k = -1;
                for (int j = 0; j < J->nf; j++) if (J->c[j].id == cid) k = j;
                if (k < 0) return 0;
                for (int j = 0; j < i; j++) if (J->scan[j] == k) return 0;
                J->scan[i] = k;
                J->c[k].td = s[2 + 2 * i] >> 4; J->c[k].ta = s[2 + 2 * i] & 15;
                if (J->c[k].td > 3 || J->c[k].ta > 3 || !J->dc[J->c[k].td].present || !J->ac[J->c[k].ta].present) return 0;
            }
            const u8 *t = s + 1 + 2 * J->ns;
            if (t[0] != 0 || t[1] != 63 || t[2] != 0) return 0;   /* Ss=0, Se=63, Ah=Al=0 */
            J->data_start = p + L;
            break;
        }
        p += L;
    }
    /* Geometrie */
    J->interleaved = J->ns > 1;
    if (J->interleaved) {
        J->mcux = (J->X + 8 * J->hmax - 1) / (8 * J->hmax);
        J->mcuy = (J->Y + 8 * J->vmax - 1) / (8 * J->vmax);
        for (int i = 0; i < J->nf; i++) { J->c[i].bw = J->mcux * J->c[i].h; J->c[i].bh = J->mcuy * J->c[i].v; }
    } else {
        Comp *c = &J->c[J->scan[0]];
        int cw = (J->X * c->h + J->hmax - 1) / J->hmax, chh = (J->Y * c->v + J->vmax - 1) / J->vmax;
        c->bw = (cw + 7) / 8; c->bh = (chh + 7) / 8;
        J->mcux = c->bw; J->mcuy = c->bh;
    }
    return 1;
}
static int alloc_coefs(JInfo *J) {
    for (int i = 0; i < J->nf; i++) {
        Comp *c = &J->c[i];
        if (!c->bw) continue;
        c->coef = calloc((size_t)c->bw * c->bh * 64, sizeof(i16));
        if (!c->coef) return 0;
    }
    return 1;
}
static void free_coefs(JInfo *J) { for (int i = 0; i < 4; i++) { free(J->c[i].coef); J->c[i].coef = NULL; } }

/* Blockreihenfolge im Scan: ruft fn(comp, row, col) für jeden Block auf; Restart-Grenzen über mcu_done */
#define FOR_EACH_MCU_BLOCK(J, mcu, ...)                                                         \
    for (long mcu = 0; mcu < (long)(J)->mcux * (J)->mcuy; mcu++) {                              \
        long my_ = mcu / (J)->mcux, mx_ = mcu % (J)->mcux;                                       \
        for (int si_ = 0; si_ < (J)->ns; si_++) {                                                \
            int ci = (J)->scan[si_]; Comp *cp = &(J)->c[ci];                                     \
            int hh = (J)->interleaved ? cp->h : 1, vv = (J)->interleaved ? cp->v : 1;            \
            for (int v_ = 0; v_ < vv; v_++) for (int h_ = 0; h_ < hh; h_++) {                    \
                long row = my_ * vv + v_, col = mx_ * hh + h_;                                   \
                i16 *blk = cp->coef + ((size_t)row * cp->bw + col) * 64;                         \
                __VA_ARGS__                                                                      \
            }                                                                                    \
        }

/* ======================================================================= */
/* Huffman-Scan dekodieren (bitweise, mit Marker-Prüfung)                   */
/* ======================================================================= */
typedef struct { const u8 *d; size_t n, p; u32 byte; int cnt; int err; } BR;
static int br_bit(BR *b) {
    if (!b->cnt) {
        if (b->p >= b->n) { b->err = 1; return 0; }
        u8 x = b->d[b->p++];
        if (x == 0xFF) {
            if (b->p >= b->n || b->d[b->p] != 0x00) { b->err = 1; return 0; }   /* Marker mitten in Daten */
            b->p++;
        }
        b->byte = x; b->cnt = 8;
    }
    b->cnt--;
    return (b->byte >> b->cnt) & 1;
}
static int br_bits(BR *b, int n) { int v = 0; while (n--) v = (v << 1) | br_bit(b); return v; }
static int huff_dec(BR *b, const Huff *h) {
    int code = 0;
    for (int l = 1; l <= 16; l++) {
        code = (code << 1) | br_bit(b);
        if (h->bits[l] && code <= h->maxcode[l] && code >= h->mincode[l]) return h->vals[h->valptr[l] + code - h->mincode[l]];
    }
    b->err = 1; return 0;
}
static int extend(int v, int s) { return s && v < (1 << (s - 1)) ? v - (1 << s) + 1 : v; }
/* Restbits bis zur Bytegrenze müssen 1 sein (Standard-Padding) */
static int br_pad_ok(BR *b) { int ok = (b->byte & ((1u << b->cnt) - 1)) == ((1u << b->cnt) - 1); b->cnt = 0; return ok; }

static int decode_scan(const u8 *d, size_t n, JInfo *J, size_t *scan_end) {
    BR b = {d, n, J->data_start, 0, 0, 0};
    int pred[4] = {0}, rst = 0; long left = J->ri;
    FOR_EACH_MCU_BLOCK(J, mcu, {
        int s = huff_dec(&b, &J->dc[cp->td]);
        if (s > 11) return 0;
        int diff = extend(br_bits(&b, s), s);
        pred[ci] += diff; blk[0] = (i16)pred[ci];
        for (int k = 1; k < 64;) {
            int rs = huff_dec(&b, &J->ac[cp->ta]), r = rs >> 4; s = rs & 15;
            if (!s) { if (r != 15) break; k += 16; if (k > 64) return 0; continue; }
            k += r;
            if (k > 63 || s > 10) return 0;
            blk[k++] = (i16)extend(br_bits(&b, s), s);
        }
        if (b.err) return 0;
    })
        if (J->ri && --left == 0 && mcu + 1 < (long)J->mcux * J->mcuy) {   /* Restart-Intervall endet */
            if (!br_pad_ok(&b)) return 0;
            if (b.p + 2 > n || d[b.p] != 0xFF || d[b.p + 1] != 0xD0 + rst) return 0;
            b.p += 2; rst = (rst + 1) & 7; left = J->ri;
            memset(pred, 0, sizeof pred);
        }
    }
    if (b.err || !br_pad_ok(&b)) return 0;
    *scan_end = b.p;
    return 1;
}

/* ======================================================================= */
/* Huffman-Scan exakt neu erzeugen                                          */
/* ======================================================================= */
typedef struct { u8 *o; size_t n, cap; u32 acc; int cnt; int err; } BW;
static void bw_byte(BW *w, u8 x) {
    if (w->n + 2 > w->cap) { w->cap = w->cap * 2 + 65536; u8 *t = realloc(w->o, w->cap); if (!t) { w->err = 1; return; } w->o = t; }
    w->o[w->n++] = x;
}
static void bw_bits(BW *w, u32 v, int len) {
    for (int i = len - 1; i >= 0; i--) {
        w->acc = (w->acc << 1) | ((v >> i) & 1);
        if (++w->cnt == 8) { bw_byte(w, (u8)w->acc); if ((u8)w->acc == 0xFF) bw_byte(w, 0x00); w->acc = 0; w->cnt = 0; }
    }
}
static void bw_pad(BW *w) { if (w->cnt) bw_bits(w, (1u << (8 - w->cnt)) - 1, 8 - w->cnt); }
static int bitlen(int v) { v = v < 0 ? -v : v; int s = 0; while (v) { s++; v >>= 1; } return s; }
static int emit(BW *w, const Huff *h, int sym) { if (!h->len[sym]) return 0; bw_bits(w, h->code[sym], h->len[sym]); return 1; }

static int encode_scan(JInfo *J, BW *w) {
    int pred[4] = {0}, rst = 0; long left = J->ri;
    FOR_EACH_MCU_BLOCK(J, mcu, {
        int diff = blk[0] - pred[ci]; pred[ci] = blk[0];
        int s = bitlen(diff);
        if (!emit(w, &J->dc[cp->td], s)) return 0;
        if (s) bw_bits(w, (u32)(diff < 0 ? diff - 1 : diff) & ((1u << s) - 1), s);
        int run = 0;
        for (int k = 1; k < 64; k++) {
            int v = blk[k];
            if (!v) { run++; continue; }
            while (run > 15) { if (!emit(w, &J->ac[cp->ta], 0xF0)) return 0; run -= 16; }
            s = bitlen(v);
            if (!emit(w, &J->ac[cp->ta], (run << 4) | s)) return 0;
            bw_bits(w, (u32)(v < 0 ? v - 1 : v) & ((1u << s) - 1), s);
            run = 0;
        }
        if (run && !emit(w, &J->ac[cp->ta], 0x00)) return 0;
    })
        if (J->ri && --left == 0 && mcu + 1 < (long)J->mcux * J->mcuy) {
            bw_pad(w); bw_byte(w, 0xFF); bw_byte(w, (u8)(0xD0 + rst));
            rst = (rst + 1) & 7; left = J->ri;
            memset(pred, 0, sizeof pred);
        }
    }
    bw_pad(w);
    return !w->err;
}

/* ======================================================================= */
/* Kontextmodell für die Koeffizienten                                      */
/* ======================================================================= */
typedef struct { u16 f, s; } Prob;
typedef struct { u32 x1, x2, x; u8 *out; size_t n, cap; const u8 *in; size_t ip, inn; int dec, err; } AC;
static void ac_put(AC *a, u8 b) {
    if (a->n == a->cap) { a->cap = a->cap * 2 + 65536; u8 *p = realloc(a->out, a->cap); if (!p) { a->err = 1; return; } a->out = p; }
    a->out[a->n++] = b;
}
/* Ein Bit kodieren oder (im Decoder) lesen – dieselbe Modellfunktion für beide Richtungen */
static int cbit(AC *a, Prob *p, int bit) {
    u32 pr = ((u32)p->f + p->s) >> 5; pr = pr < 1 ? 1 : (pr > 4095 ? 4095 : pr);
    u32 xmid = a->x1 + (u32)(((u64)(a->x2 - a->x1) * pr) >> 12);
    if (a->dec) bit = a->x <= xmid;
    if (bit) a->x2 = xmid; else a->x1 = xmid + 1;
    if (bit) { p->f += (65535 - p->f) >> 4; p->s += (65535 - p->s) >> 7; }
    else     { p->f -= p->f >> 4;           p->s -= p->s >> 7; }
    while (((a->x1 ^ a->x2) & 0xFF000000u) == 0) {
        if (a->dec) a->x = (a->x << 8) | (a->ip < a->inn ? a->in[a->ip++] : 0);
        else ac_put(a, (u8)(a->x2 >> 24));
        a->x1 <<= 8; a->x2 = (a->x2 << 8) | 255;
    }
    return bit;
}

#define NKB 12
#define NMQ 7
#define NRQ 8
#define NNZ 11
#define MAXE 12
typedef struct {
    Prob nz[2][NNZ][64];                        /* Anzahl Nicht-Null-AC (6-Bit-Baum) */
    Prob zero[2][NKB][NMQ][NRQ];                /* Koeffizient != 0 ? */
    Prob ex[2][NKB][NMQ][MAXE];                 /* Exponent unär */
    Prob mt[2][MAXE][2];                        /* Mantisse: oberstes Bit / Rest */
    Prob sg[2][NKB][9];                         /* Vorzeichen nach Nachbar-Vorzeichen */
    Prob dz[2][8], dsg[2][8], dex[2][8][MAXE], dmt[2][MAXE][2];   /* DC-Rest */
} Model;
static void model_init(Model *m) { Prob *p = (Prob *)m; for (size_t i = 0; i < sizeof *m / sizeof *p; i++) { p[i].f = 32768; p[i].s = 32768; } }

static const u8 KB[64] = {0, 0, 1, 2, 3, 4, 5, 5, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 9,
                          10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11,
                          11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11};
static int iabs(int v) { return v < 0 ? -v : v; }
static int qmag(int v) { return v == 0 ? 0 : v == 1 ? 1 : v == 2 ? 2 : v <= 4 ? 3 : v <= 8 ? 4 : v <= 16 ? 5 : 6; }
static int qnz(int v) { static const int T[NNZ - 1] = {0, 1, 2, 3, 4, 6, 9, 14, 22, 34}; int c = 0; while (c < NNZ - 1 && v > T[c]) c++; return c; }

/* Betrag >= 1 mit Exponent (unär) + Mantisse */
static int code_mag(AC *a, Prob *ex, Prob (*mt)[2], int v) {
    int e = 0;
    if (!a->dec) { while ((v >> (e + 1)) && e < MAXE - 1) e++; }
    int ee = 0;
    while (ee < MAXE - 1 && cbit(a, &ex[ee], ee < e)) ee++;
    e = ee;
    int r = 1;
    for (int b = e - 1; b >= 0; b--) r = (r << 1) | cbit(a, &mt[e][b == e - 1 ? 0 : 1], (v >> b) & 1);
    return r;
}
static int nzcount(const i16 *b) { int c = 0; for (int k = 1; k < 64; k++) c += b[k] != 0; return c; }

/* Alle Koeffizienten einer Komponente kodieren/dekodieren (Rasterreihenfolge der Blöcke) */
static int code_comp(AC *a, Model *m, Comp *c, int cls) {
    for (int r = 0; r < c->bh; r++)
        for (int col = 0; col < c->bw; col++) {
            i16 *b = c->coef + ((size_t)r * c->bw + col) * 64;
            const i16 *A = r ? b - (size_t)c->bw * 64 : NULL, *L = col ? b - 64 : NULL, *AL = (r && col) ? b - (size_t)c->bw * 64 - 64 : NULL;
            /* DC: Vorhersage MED(A, L, AL) */
            int pa = A ? A[0] : (L ? L[0] : 0), pl = L ? L[0] : pa, pal = AL ? AL[0] : pa;
            int mx = pa > pl ? pa : pl, mn = pa < pl ? pa : pl;
            int pred = pal >= mx ? mn : (pal <= mn ? mx : pa + pl - pal);
            int dq = qmag(iabs(pa - pl)); if (dq > 7) dq = 7;
            int dr = a->dec ? 0 : b[0] - pred;
            if (cbit(a, &m->dz[cls][dq], dr != 0)) {
                int neg = cbit(a, &m->dsg[cls][dq], dr < 0);
                int mag = code_mag(a, m->dex[cls][dq], m->dmt[cls], iabs(dr));
                dr = neg ? -mag : mag;
            } else dr = 0;
            if (a->dec) { int v = pred + dr; if (v < -32768 || v > 32767) return 0; b[0] = (i16)v; }
            /* Anzahl Nicht-Null-AC: Kontext aus den Nachbarn */
            int na = A ? nzcount(A) : -1, nl = L ? nzcount(L) : -1;
            int avg = na < 0 ? (nl < 0 ? 0 : nl) : (nl < 0 ? na : (na + nl + 1) / 2);
            int nzq = qnz(avg);
            int nnz = a->dec ? 0 : nzcount(b), node = 1;
            for (int bit = 5; bit >= 0; bit--) node = (node << 1) | cbit(a, &m->nz[cls][nzq][node], (nnz >> bit) & 1);
            nnz = node - 64;
            /* AC in Zickzack-Reihenfolge, bis alle Nicht-Null-Werte kodiert sind */
            int rem = nnz;
            for (int k = 1; k < 64 && rem > 0; k++) {
                int ma = A ? iabs(A[k]) : -1, ml = L ? iabs(L[k]) : -1;
                int mag = ma < 0 ? (ml < 0 ? 0 : 2 * ml) : (ml < 0 ? 2 * ma : ma + ml);
                int mq = qmag(mag), rq = rem > NRQ ? NRQ - 1 : rem - 1, kb = KB[k];
                if (rem > 63 - k + 1) return 0;
                int v = a->dec ? 0 : b[k];
                /* Muss der Koeffizient nicht-null sein (nur noch so viele Positionen wie Werte)? */
                int isnz = (rem == 64 - k) ? 1 : cbit(a, &m->zero[cls][kb][mq][rq], v != 0);
                if (!isnz) { if (a->dec) b[k] = 0; continue; }
                int sa = A ? (A[k] > 0) - (A[k] < 0) : 0, sl = L ? (L[k] > 0) - (L[k] < 0) : 0;
                int neg = cbit(a, &m->sg[cls][kb][(sa + 1) * 3 + sl + 1], v < 0);
                int am = code_mag(a, m->ex[cls][kb][mq], m->mt[cls], iabs(v));
                if (am > 1023) return 0;
                if (a->dec) b[k] = (i16)(neg ? -am : am);
                rem--;
            }
            if (rem) return 0;
        }
    return 1;
}

/* ======================================================================= */
/* Öffentliche Funktionen                                                   */
/* ======================================================================= */
static void wr32(u8 *p, u32 v) { memcpy(p, &v, 4); }
static u32 rd32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }

uint8_t *jpgx_forward(const uint8_t *jpg, size_t n, size_t *tsize) {
    JInfo J; size_t scan_end;
    if (!parse_header(jpg, n, &J)) return NULL;
    if (!alloc_coefs(&J)) { free_coefs(&J); return NULL; }
    u8 *t = NULL;
    if (!decode_scan(jpg, n, &J, &scan_end)) goto done;
    /* nach dem Scan muss EOI folgen (Rest wird unverändert übernommen) */
    if (scan_end + 2 > n || jpg[scan_end] != 0xFF || jpg[scan_end + 1] != 0xD9) goto done;
    Model *m = malloc(sizeof(Model));
    if (!m) goto done;
    model_init(m);
    AC a = {0}; a.x2 = 0xFFFFFFFFu;
    int ok = 1;
    for (int i = 0; i < J.nf && ok; i++) ok = code_comp(&a, m, &J.c[i], i ? 1 : 0);
    for (int i = 0; i < 4; i++) { ac_put(&a, (u8)(a.x1 >> 24)); a.x1 <<= 8; }
    free(m);
    if (!ok || a.err) { free(a.out); goto done; }
    size_t head = J.data_start, tail = n - scan_end;
    size_t tn = 4 + 4 + head + 4 + tail + 4 + a.n;
    t = malloc(tn);
    if (!t) { free(a.out); goto done; }
    memcpy(t, "JPGX", 4);
    wr32(t + 4, (u32)head); memcpy(t + 8, jpg, head);
    wr32(t + 8 + head, (u32)tail); memcpy(t + 12 + head, jpg + scan_end, tail);
    wr32(t + 12 + head + tail, (u32)a.n); memcpy(t + 16 + head + tail, a.out, a.n);
    free(a.out);
    /* Sicherheitsnetz: vollständige Rekonstruktion prüfen */
    u8 *chk = malloc(n);
    int good = chk && jpgx_inverse(t, tn, chk, n) && !memcmp(chk, jpg, n);
    free(chk);
    if (!good) { free(t); t = NULL; goto done; }
    *tsize = tn;
done:
    free_coefs(&J);
    return t;
}

int jpgx_inverse(const uint8_t *t, size_t tn, uint8_t *out, size_t origsize) {
    if (tn < 16 || memcmp(t, "JPGX", 4)) return 0;
    u32 head = rd32(t + 4);
    if (head > tn - 16) return 0;
    const u8 *hd = t + 8;
    u32 tail = rd32(t + 8 + head);
    if (tail > tn - 16 - head) return 0;
    const u8 *tl = t + 12 + head;
    u32 mn = rd32(t + 12 + head + tail);
    if ((size_t)16 + head + tail + mn != tn) return 0;
    JInfo J;
    if (!parse_header(hd, head, &J) || J.data_start != head) return 0;
    int ok = 0;
    Model *m = malloc(sizeof(Model));
    BW w = {0};
    if (!m || !alloc_coefs(&J)) goto done;
    model_init(m);
    AC a = {0}; a.x2 = 0xFFFFFFFFu; a.dec = 1; a.in = t + 16 + head + tail; a.inn = mn;
    for (int i = 0; i < 4; i++) a.x = (a.x << 8) | (a.ip < a.inn ? a.in[a.ip++] : 0);
    for (int i = 0; i < J.nf; i++) if (!code_comp(&a, m, &J.c[i], i ? 1 : 0)) goto done;
    if (!encode_scan(&J, &w)) goto done;
    if ((size_t)head + w.n + tail != origsize) goto done;
    memcpy(out, hd, head); memcpy(out + head, w.o, w.n); memcpy(out + head + w.n, tl, tail);
    ok = 1;
done:
    free(m); free(w.o); free_coefs(&J);
    return ok;
}
