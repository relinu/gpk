/*
 * GPK – Download-Packer für komplette Spiel-Builds.
 *
 *   gpk c [Optionen] <build-ordner> <archiv.gpk>    packen
 *   gpk x [-t N] <archiv.gpk> <ziel-ordner>          entpacken (parallel, CRC-geprüft)
 *   gpk l <archiv.gpk>                               Inhalt auflisten
 *
 * Pipeline beim Packen:
 *   1. Ordner scannen (Dateien, Ordner, Symlinks, Rechte, Zeitstempel)
 *   2. Deduplizieren: identische Dateien (CRC32 + Größe + Bytevergleich) nur 1x
 *   3. Klassifizieren je Datei: Text, x86-Code, BCn-Textur (DDS), Binärdaten,
 *      bereits komprimiert (Ogg, Video, PNG ...) -> wird nur gespeichert
 *   4. Filter je Datei per Probekompression wählen (BCJ, Feld-Split, Delta, Shuffle)
 *   5. Sortieren nach Klasse/Endung/Name, damit ähnliche Dateien im selben
 *      Fenster landen (dateiübergreifende Matches)
 *   6. Solide Blöcke (Standard 64 MiB) parallel mit LZRC komprimieren
 *
 * Archivformat (Little Endian):
 *   "GPK1" | u64 index_off | u64 index_csize | u64 index_usize | u8 index_codec | Blöcke | Index
 */
#define _GNU_SOURCE
#ifdef _WIN32
#define __USE_MINGW_ANSI_STDIO 1
#endif
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdatomic.h>
#include <errno.h>
#include <dirent.h>
#include <limits.h>
#include <pthread.h>
#include <zlib.h>
#ifndef GPK_NO_XZ
#include <lzma.h>                 /* nur für den optionalen --xz-Kern */
#endif
#include "platform.h"
#include "lzrc.h"
#include "filters.h"
#include "pngx.h"
#include "../glz.h"

typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef uint64_t u64; typedef int64_t i64;

#define NONE        0xFFFFFFFFu
#define SAMPLE      (384u << 10)   /* Stichprobe für Filter-Probeläufe */
#define HDR_SIZE    (4 + 8 + 8 + 8 + 1)

enum { CODEC_STORED = 0, CODEC_LZRC = 1, CODEC_XZ = 2 };
enum { CL_TEXT, CL_EXE, CL_TEXTURE, CL_BINARY, CL_RAW, CL_N };
static const char *const CLASS_NAMES[CL_N] = {"Text/Skripte", "x86-Programmcode", "BCn-Texturen (DDS)", "sonstige Binärdaten", "bereits komprimiert"};
enum { E_FILE = 0, E_DIR = 1, E_LINK = 2 };

static void die(const char *fmt, const char *arg) { fprintf(stderr, "gpk: "); fprintf(stderr, fmt, arg); fprintf(stderr, "\n"); exit(1); }
static double now(void) { return plat_now(); }
/* Pfad zusammensetzen; zu lange Pfade sind ein Fehler statt still abgeschnitten zu werden. */
static void join(char *out, size_t cap, const char *a, const char *sep, const char *b) {
    if ((size_t)snprintf(out, cap, "%s%s%s", a, sep, b) >= cap) die("Pfad zu lang: %s", b);
}
static void *xmalloc(size_t n) { void *p = malloc(n ? n : 1); if (!p) die("%s", "Speicher voll"); return p; }

/* ---------------- Optionen ---------------- */
static u64 g_block = 64ull << 20;
static int g_threads = 4, g_level = 9;
static const u64 g_block_max_ssize = 1ull << 30;
static int opt_filter = 1, opt_sort = 1, opt_dedup = 1, opt_raw = 1, opt_codec = CODEC_LZRC;

/* ---------------- Datenstrukturen ---------------- */
typedef struct { char *path; int type; u32 mode; i64 mtime; u64 size; u32 crc; u32 content; char *link; } Entry;
/* ssize/sdata: Datenstrom, der komprimiert wird (bei xform = umgewandelte Form, z. B. entpacktes PNG) */
typedef struct { u64 size; u32 crc; const u8 *data; fspec f; int cls; const char *ext, *base, *path; int xform; u64 ssize; u8 *sdata; } Content;
enum { XF_NONE = 0, XF_PNG = 1 };
typedef struct { u32 content; u64 off; u32 len; } Piece;
typedef struct { Piece *pc; u32 np, cap; u64 usize, off, csize; int codec; u8 *cdata; int owned; } Block;

static Entry *E; static u32 nE, capE;
static Content *C; static u32 nC;
static Block *B; static u32 nB, capB;

/* ---------------- Puffer schreiben / lesen ---------------- */
typedef struct { u8 *p; size_t n, cap; } Buf;
static void bput(Buf *b, const void *d, size_t n) {
    if (b->n + n > b->cap) { b->cap = (b->n + n) * 2 + 4096; b->p = realloc(b->p, b->cap); if (!b->p) die("%s", "Speicher voll"); }
    memcpy(b->p + b->n, d, n); b->n += n;
}
#define PUT(b, T, v) do { T _t = (T)(v); bput(b, &_t, sizeof _t); } while (0)

typedef struct { const u8 *p, *end; int err; } Rd;
static void rget(Rd *r, void *d, size_t n) {
    if ((size_t)(r->end - r->p) < n) { r->err = 1; memset(d, 0, n); return; }
    memcpy(d, r->p, n); r->p += n;
}
#define GET(r, T) ({ T _v; rget(r, &_v, sizeof _v); _v; })
static char *rstr(Rd *r) {
    u16 l = GET(r, u16);
    char *s = xmalloc((size_t)l + 1);
    rget(r, s, l); s[l] = 0;
    return s;
}

/* ---------------- Thread-Pool ---------------- */
typedef struct { atomic_uint next; u32 n; void (*fn)(u32, void *); void *ctx; } Pool;
static void *pool_worker(void *a) {
    Pool *p = a;
    for (;;) { u32 i = atomic_fetch_add(&p->next, 1); if (i >= p->n) break; p->fn(i, p->ctx); }
    return NULL;
}
static void parallel_for(u32 n, void (*fn)(u32, void *), void *ctx) {
    Pool p; atomic_init(&p.next, 0); p.n = n; p.fn = fn; p.ctx = ctx;
    int t = g_threads < 1 ? 1 : g_threads;
    pthread_t th[64];
    if (t > 64) t = 64;
    for (int i = 0; i < t; i++) pthread_create(&th[i], NULL, pool_worker, &p);
    for (int i = 0; i < t; i++) pthread_join(th[i], NULL);
}

/* ---------------- Scannen ---------------- */
static void add_entry(Entry e) {
    if (nE == capE) { capE = capE * 2 + 256; E = realloc(E, capE * sizeof(Entry)); if (!E) die("%s", "Speicher voll"); }
    E[nE++] = e;
}
static int cmpstr(const void *a, const void *b) { return strcmp(*(char *const *)a, *(char *const *)b); }

static void scan(const char *root, const char *rel) {
    char full[GPK_PATH];
    join(full, sizeof full, root, *rel ? "/" : "", rel);
    DIR *d = opendir(full);
    if (!d) die("Kann Ordner nicht öffnen: %s", full);
    char **names = NULL; size_t nn = 0, cn = 0;
    struct dirent *de;
    while ((de = readdir(d))) {
        if (!strcmp(de->d_name, ".") || !strcmp(de->d_name, "..")) continue;
        if (nn == cn) { cn = cn * 2 + 32; names = realloc(names, cn * sizeof *names); }
        names[nn++] = strdup(de->d_name);
    }
    closedir(d);
    qsort(names, nn, sizeof *names, cmpstr);
    for (size_t i = 0; i < nn; i++) {
        char r[GPK_PATH], f[GPK_PATH];
        join(r, sizeof r, rel, *rel ? "/" : "", names[i]);
        join(f, sizeof f, root, "/", r);
        struct stat st;
        if (lstat(f, &st)) die("stat fehlgeschlagen: %s", f);
        Entry e = {0};
        e.path = strdup(r); e.mode = st.st_mode & 07777; e.mtime = st.st_mtime; e.content = NONE;
        if (S_ISDIR(st.st_mode)) { e.type = E_DIR; add_entry(e); scan(root, r); }
        else if (S_ISREG(st.st_mode)) { e.type = E_FILE; e.size = plat_file_size(f); add_entry(e); }
        else if (S_ISLNK(st.st_mode)) {
            char t[GPK_PATH];
            if (plat_readlink(f, t, sizeof t) < 0) die("readlink fehlgeschlagen: %s", f);
            e.type = E_LINK; e.link = strdup(t); add_entry(e);
        }
        free(names[i]);
    }
    free(names);
}

static u32 crc_of(const u8 *d, u64 n) {
    uLong c = crc32(0, NULL, 0);
    while (n) { uInt k = n > (1u << 30) ? (1u << 30) : (uInt)n; c = crc32(c, d, k); d += k; n -= k; }
    return (u32)c;
}

/* ---------------- Klassifikation & Filterwahl ---------------- */
static int is_text(const u8 *s, size_t n) {
    if (n > 65536) n = 65536;
    if (!n) return 0;
    size_t good = 0;
    for (size_t i = 0; i < n; i++) {
        u8 c = s[i];
        if (c == 0) return 0;
        if (c >= 32 || c == '\n' || c == '\r' || c == '\t') good++;
    }
    return good >= n * 97 / 100;
}
static inline u32 le32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }
static inline u16 le16(const u8 *p) { u16 v; memcpy(&v, p, 2); return v; }

static int is_x86(const u8 *d, u64 n) {
    if (n >= 20 && !memcmp(d, "\177ELF", 4)) { u16 m = le16(d + 18); return m == 3 || m == 62; }
    if (n >= 64 && d[0] == 'M' && d[1] == 'Z') {
        u32 pe = le32(d + 0x3C);
        if ((u64)pe + 6 <= n && !memcmp(d + pe, "PE\0\0", 4)) { u16 m = le16(d + pe + 4); return m == 0x14c || m == 0x8664; }
    }
    return 0;
}

/* DDS mit BC1..BC5: Blöcke in Feld-Streams zerlegen (Endpunkte vs. Indizes). */
static int dds_split(const u8 *d, u64 n, fspec *f) {
    if (n < 128 || memcmp(d, "DDS ", 4)) return 0;
    const u8 *cc = d + 84;
    u32 skip = 128, dxgi = 0;
    if (!memcmp(cc, "DX10", 4)) { if (n < 148) return 0; dxgi = le32(d + 128); skip = 148; }
    memset(f, 0, sizeof *f);
    f->id = FT_SPLIT; f->skip = skip;
    static const u8 BC1[] = {4, 4}, BC2[] = {8, 4, 4}, BC3[] = {2, 6, 4, 4}, BC4[] = {2, 6}, BC5[] = {2, 6, 2, 6};
    const u8 *fl = NULL; int nf = 0;
#define SETF(a) do { fl = a; nf = (int)sizeof(a); } while (0)
    if (!memcmp(cc, "DXT1", 4) || (dxgi >= 70 && dxgi <= 72)) SETF(BC1);
    else if (!memcmp(cc, "DXT3", 4) || (dxgi >= 73 && dxgi <= 75)) SETF(BC2);
    else if (!memcmp(cc, "DXT5", 4) || (dxgi >= 76 && dxgi <= 78)) SETF(BC3);
    else if (!memcmp(cc, "ATI1", 4) || !memcmp(cc, "BC4U", 4) || (dxgi >= 79 && dxgi <= 81)) SETF(BC4);
    else if (!memcmp(cc, "ATI2", 4) || !memcmp(cc, "BC5U", 4) || (dxgi >= 82 && dxgi <= 84)) SETF(BC5);
#undef SETF
    if (!fl) return 0;
    f->nfields = (u8)nf; memcpy(f->fields, fl, nf);
    return 1;
}

static double delta_entropy(const u8 *p, size_t n, int s) {
    u32 h[256] = {0};
    for (size_t i = s; i < n; i++) h[(u8)(p[i] - p[i - s])]++;
    double tot = (double)(n - s), e = 0;
    for (int k = 0; k < 256; k++) if (h[k]) { double q = h[k] / tot; e -= h[k] * __builtin_log2(q); }
    return e;
}

/* Bewertet einen Filterkandidaten mit dem echten LZRC-Kern (schnelle Einstellung). */
static size_t trial_size(const fspec *f, const u8 *s, size_t sn, u64 soff, u8 *tmp, u8 *out) {
    filter_encode(f, s, tmp, sn, soff);
    lzrc_params p; lzrc_default_params(&p, 2);
    size_t z = lzrc_compress(tmp, sn, out, lzrc_bound(sn), &p);
    return z ? z : sn;
}

static void add_cand(fspec *cand, int *nc, fspec f) {
    for (int k = 0; k < *nc; k++) if (!memcmp(&cand[k], &f, sizeof f)) return;   /* Duplikate vermeiden */
    cand[(*nc)++] = f;
}

/* PNG-Rekompression versuchen; nur übernehmen, wenn sie nach LZRC wirklich kleiner ist. */
static void xform_one(u32 ci, void *ctx) {
    (void)ctx;
    Content *c = &C[ci];
    if (c->size < 64 || c->data[0] != 0x89 || memcmp(c->data + 1, "PNG", 3)) return;
    size_t tn; u8 *t = pngx_forward(c->data, c->size, &tn);
    if (!t) return;
    if (tn > g_block) { free(t); return; }
    lzrc_params p; lzrc_default_params(&p, 2);
    u8 *out = xmalloc(lzrc_bound(tn));
    size_t z = lzrc_compress(t, tn, out, lzrc_bound(tn), &p);
    free(out);
    if (!z || z > c->size * 0.97) { free(t); return; }
    c->xform = XF_PNG; c->sdata = t; c->ssize = tn;
}

static void classify_one(u32 ci, void *ctx) {
    (void)ctx;
    Content *c = &C[ci];
    memset(&c->f, 0, sizeof c->f);
    c->cls = CL_BINARY;
    const u8 *d = c->sdata; u64 n = c->ssize;
    size_t sn = n < SAMPLE ? (size_t)n : SAMPLE;
    u64 soff = n > 2ull * SAMPLE ? ((n / 2 - SAMPLE / 2) & ~63ull) : 0;
    const u8 *s = d + soff;
    if (is_text(d, n < 65536 ? n : 65536)) { c->cls = CL_TEXT; return; }
    if (!opt_filter && !opt_raw) return;
    if (n < 4096) return;

    u8 *tmp = xmalloc(sn), *out = xmalloc(lzrc_bound(sn) > glz_bound(sn) ? lzrc_bound(sn) : glz_bound(sn));
    /* Schneller Vortest: bereits komprimiert (Ogg, Video, PNG ...)? */
    size_t g = glz_compress(s, sn, out, glz_bound(sn), 1, GLZ_FLAG_NO_FILTER);
    if (opt_raw && (!g || g > sn * 0.99)) { c->cls = CL_RAW; free(tmp); free(out); return; }

    fspec cand[24]; int nc = 0;
    memset(cand, 0, sizeof cand);
    fspec f0; memset(&f0, 0, sizeof f0);
    add_cand(cand, &nc, f0);
    if (opt_filter) {
        fspec t;
        if (is_x86(d, n)) { c->cls = CL_EXE; t = f0; t.id = FT_BCJ; add_cand(cand, &nc, t); }
        else if (dds_split(d, n, &t)) { c->cls = CL_TEXTURE; add_cand(cand, &nc, t); }
        else {
            /* Stride-Erkennung: die zwei Abstände mit der kleinsten Delta-Entropie */
            size_t en = sn < 65536 ? sn : 65536;
            int st[2] = {1, 1}; double be[2] = {1e300, 1e300};
            for (int k = 1; k <= 64; k++) {
                double e = delta_entropy(s, en, k) * (1.0 + 0.002 * k);   /* kleine Strides leicht bevorzugen */
                if (e < be[0]) { be[1] = be[0]; st[1] = st[0]; be[0] = e; st[0] = k; }
                else if (e < be[1]) { be[1] = e; st[1] = k; }
            }
            for (int q = 0; q < 2; q++) {
                int S = st[q];
                t = f0; t.id = FT_DELTA; t.stride = (u8)S; add_cand(cand, &nc, t);
                if (S > 1) {
                    t = f0; t.id = FT_SHUFDELTA; t.stride = (u8)S; add_cand(cand, &nc, t);
                    t = f0; t.id = FT_SHUF; t.stride = (u8)S; add_cand(cand, &nc, t);
                }
                for (int W = 4; W >= 2; W -= 2) {
                    if (S % W) continue;
                    for (int o = 1; o <= 2; o++) {
                        t = f0; t.id = FT_WPRED; t.stride = (u8)S; t.fields[0] = (u8)W; t.fields[1] = (u8)o; t.fields[2] = S > W;
                        add_cand(cand, &nc, t);
                    }
                }
            }
        }
    }
    size_t best = (size_t)-1, none_size = 0;
    for (int k = 0; k < nc; k++) {
        size_t z = trial_size(&cand[k], s, sn, soff, tmp, out);
        if (k == 0) none_size = z;
        if (z < best) { best = z; c->f = cand[k]; }
    }
    if (c->f.id != FT_NONE && best > none_size * 0.995) { memset(&c->f, 0, sizeof c->f); best = none_size; }
    if (opt_raw && best > sn * 0.98) { c->cls = CL_RAW; memset(&c->f, 0, sizeof c->f); }
    free(tmp); free(out);
}

static int class_rank(int cls) { static const int r[CL_N] = {1, 0, 2, 3, 4}; return r[cls]; }
static int cmp_content(const void *a, const void *b) {
    const Content *x = &C[*(const u32 *)a], *y = &C[*(const u32 *)b];
    int r;
    if ((r = class_rank(x->cls) - class_rank(y->cls))) return r;
    if ((r = strcmp(x->ext, y->ext))) return r;
    if ((r = (int)x->f.id - (int)y->f.id)) return r;
    if ((r = strcmp(x->base, y->base))) return r;
    return strcmp(x->path, y->path);
}

/* ---------------- Blöcke ---------------- */
static Block *new_block(int codec) {
    if (nB == capB) { capB = capB * 2 + 16; B = realloc(B, capB * sizeof(Block)); }
    Block *b = &B[nB++]; memset(b, 0, sizeof *b); b->codec = codec;
    return b;
}
static void add_piece(Block *b, u32 ci, u64 off, u32 len) {
    if (b->np == b->cap) { b->cap = b->cap * 2 + 16; b->pc = realloc(b->pc, b->cap * sizeof(Piece)); }
    b->pc[b->np++] = (Piece){ci, off, len};
    b->usize += len;
}
static void build_blocks(const u32 *order, int raw) {
    Block *b = NULL;
    for (u32 k = 0; k < nC; k++) {
        Content *c = &C[order[k]];
        if ((c->cls == CL_RAW) != raw) continue;
        for (u64 off = 0; off < c->ssize;) {
            u32 len = (u32)((c->ssize - off) < g_block ? (c->ssize - off) : g_block);
            if (!b || (b->usize + len > g_block && b->np)) b = new_block(raw ? CODEC_STORED : opt_codec);
            add_piece(b, order[k], off, len);
            off += len;
        }
    }
}

static void compress_block(u32 bi, void *ctx) {
    (void)ctx;
    Block *b = &B[bi];
    u8 *buf = xmalloc(b->usize);
    u64 pos = 0, text = 0;
    for (u32 k = 0; k < b->np; k++) {
        Piece *p = &b->pc[k]; Content *c = &C[p->content];
        filter_encode(&c->f, c->sdata + p->off, buf + pos, p->len, p->off);
        if (c->cls == CL_TEXT) text += p->len;
        pos += p->len;
    }
    size_t cs = 0; u8 *out = NULL;
    if (b->codec == CODEC_LZRC) {
        lzrc_params pr; lzrc_default_params(&pr, g_level);
        if (text * 2 > b->usize) { pr.lc = 4; pr.pb = 0; }       /* Text: mehr Literal-Kontext */
        size_t cap = lzrc_bound(b->usize);
        out = xmalloc(cap);
        cs = lzrc_compress(buf, b->usize, out, cap, &pr);
    }
#ifndef GPK_NO_XZ
    else if (b->codec == CODEC_XZ) {
        size_t cap = b->usize + b->usize / 2 + 65536;
        out = xmalloc(cap);
        if (lzma_easy_buffer_encode(9 | LZMA_PRESET_EXTREME, LZMA_CHECK_NONE, NULL, buf, b->usize, out, &cs, cap) != LZMA_OK) cs = 0;
    }
#endif
    if (!cs || cs >= b->usize) { free(out); b->codec = CODEC_STORED; b->cdata = buf; b->csize = b->usize; }
    else { free(buf); b->cdata = out; b->csize = cs; }
    fprintf(stderr, "  Block %3u/%u: %7.1f MB -> %7.1f MB  (%s)\n", bi + 1, nB, b->usize / 1e6, b->csize / 1e6,
            b->codec == CODEC_STORED ? "gespeichert" : b->codec == CODEC_LZRC ? "LZRC" : "xz");
}

/* ---------------- Index ---------------- */
static void write_fspec(Buf *ix, const fspec *f) {
    PUT(ix, u8, f->id); PUT(ix, u8, f->stride); PUT(ix, u8, f->nfields);
    bput(ix, f->fields, 6); PUT(ix, u32, f->skip);
}
static fspec read_fspec(Rd *r) {
    fspec f; f.id = GET(r, u8); f.stride = GET(r, u8); f.nfields = GET(r, u8);
    rget(r, f.fields, 6); f.skip = GET(r, u32);
    if (f.id >= FT_COUNT || f.nfields > 6 || ((f.id == FT_DELTA || f.id == FT_SHUF || f.id == FT_SHUFDELTA) && !f.stride)) r->err = 1;
    if (f.id == FT_SPLIT) { u32 R = 0; for (int k = 0; k < f.nfields; k++) R += f.fields[k]; if (!R) r->err = 1; }
    if (f.id == FT_WPRED && ((f.fields[0] != 2 && f.fields[0] != 4) || f.fields[1] < 1 || f.fields[1] > 2 || !f.stride || f.stride % f.fields[0])) r->err = 1;
    return f;
}
static void put_str(Buf *ix, const char *s) { u16 l = (u16)strlen(s); PUT(ix, u16, l); bput(ix, s, l); }

/* ---------------- Packen ---------------- */
static int cmp_dedup(const void *a, const void *b) {
    const Entry *x = &E[*(const u32 *)a], *y = &E[*(const u32 *)b];
    if (x->size != y->size) return x->size < y->size ? -1 : 1;
    if (x->crc != y->crc) return x->crc < y->crc ? -1 : 1;
    return strcmp(x->path, y->path);
}
typedef struct { const char *root; } HashCtx;
static void hash_one(u32 i, void *ctx) {
    Entry *e = &E[i];
    if (e->type != E_FILE || !e->size) return;
    char f[GPK_PATH]; join(f, sizeof f, ((HashCtx *)ctx)->root, "/", e->path);
    const u8 *m = plat_map(f, e->size);
    if (!m) die("Kann Datei nicht lesen: %s", f);
    e->link = (char *)m;                /* vorübergehend: Zeiger auf Dateidaten */
    e->crc = crc_of(m, e->size);
}

static int cmd_compress(const char *dir, const char *outpath) {
    double t0 = now();
    scan(dir, "");
    HashCtx hc = {dir};
    parallel_for(nE, hash_one, &hc);

    /* Deduplizieren */
    u32 *fi = xmalloc(sizeof(u32) * (nE + 1)); u32 nf = 0;
    for (u32 i = 0; i < nE; i++) if (E[i].type == E_FILE && E[i].size) fi[nf++] = i;
    qsort(fi, nf, sizeof *fi, cmp_dedup);
    C = xmalloc(sizeof(Content) * (nf + 1));
    u64 total = 0, dup_bytes = 0; u32 dup_files = 0;
    u32 group_start = 0;
    for (u32 k = 0; k < nf; k++) {
        Entry *e = &E[fi[k]];
        total += e->size;
        if (k && (E[fi[k - 1]].size != e->size || E[fi[k - 1]].crc != e->crc)) group_start = nC;
        u32 found = NONE;
        if (opt_dedup)
            for (u32 c = group_start; c < nC; c++)
                if (C[c].size == e->size && C[c].crc == e->crc && !memcmp(C[c].data, e->link, e->size)) { found = c; break; }
        if (found != NONE) { e->content = found; dup_bytes += e->size; dup_files++; plat_unmap((const u8 *)e->link, e->size); e->link = NULL; continue; }
        Content *c = &C[nC]; memset(c, 0, sizeof *c);
        c->size = e->size; c->crc = e->crc; c->data = (const u8 *)e->link; c->path = e->path;
        const char *sl = strrchr(e->path, '/'); c->base = sl ? sl + 1 : e->path;
        const char *dot = strrchr(c->base, '.'); c->ext = dot ? dot + 1 : "";
        e->content = nC++;
    }
    for (u32 k = 0; k < nf; k++) E[fi[k]].link = NULL;
    free(fi);

    /* Umwandlungen (PNG-Rekompression), dann klassifizieren & Filter wählen */
    for (u32 i = 0; i < nC; i++) { C[i].ssize = C[i].size; C[i].sdata = (u8 *)C[i].data; }
    if (opt_filter) parallel_for(nC, xform_one, NULL);
    u32 npng = 0; u64 png_bytes = 0;
    for (u32 i = 0; i < nC; i++) if (C[i].xform) { npng++; png_bytes += C[i].size; }
    parallel_for(nC, classify_one, NULL);

    /* Sortieren */
    u32 *order = xmalloc(sizeof(u32) * (nC + 1));
    for (u32 i = 0; i < nC; i++) order[i] = i;
    if (opt_sort) qsort(order, nC, sizeof *order, cmp_content);

    /* Statistik */
    u64 cls_bytes[CL_N] = {0}; u32 cls_files[CL_N] = {0}; u32 filt_cnt[FT_COUNT] = {0};
    for (u32 i = 0; i < nC; i++) { cls_bytes[C[i].cls] += C[i].size; cls_files[C[i].cls]++; filt_cnt[C[i].f.id]++; }
    fprintf(stderr, "Build: %u Einträge, %.1f MB in Dateien\n", nE, total / 1e6);
    fprintf(stderr, "Deduplizierung: %u doppelte Dateien, %.1f MB gespart\n", dup_files, dup_bytes / 1e6);
    for (int k = 0; k < CL_N; k++)
        if (cls_files[k]) fprintf(stderr, "  %-24s %5u Dateien %9.1f MB\n", CLASS_NAMES[k], cls_files[k], cls_bytes[k] / 1e6);
    if (npng) fprintf(stderr, "PNG-Rekompression: %u Dateien (%.1f MB) bitgenau umkehrbar\n", npng, png_bytes / 1e6);
    fprintf(stderr, "Filter:");
    for (int k = 0; k < FT_COUNT; k++) if (filt_cnt[k]) fprintf(stderr, " %s=%u", FILTER_NAMES[k], filt_cnt[k]);
    fprintf(stderr, "\n");

    /* Blöcke bilden und parallel komprimieren */
    build_blocks(order, 0);
    build_blocks(order, 1);
    parallel_for(nB, compress_block, NULL);

    /* Schreiben */
    FILE *out = fopen(outpath, "wb");
    if (!out) die("Kann Archiv nicht schreiben: %s", outpath);
    u8 hdr[HDR_SIZE] = {0};
    fwrite(hdr, 1, HDR_SIZE, out);
    u64 off = HDR_SIZE;
    for (u32 i = 0; i < nB; i++) {
        B[i].off = off;
        if (fwrite(B[i].cdata, 1, B[i].csize, out) != B[i].csize) die("%s", "Schreibfehler");
        off += B[i].csize;
        free(B[i].cdata);
    }
    Buf ix = {0};
    PUT(&ix, u32, nE);
    for (u32 i = 0; i < nE; i++) {
        Entry *e = &E[i];
        PUT(&ix, u8, e->type); put_str(&ix, e->path); PUT(&ix, u32, e->mode); PUT(&ix, i64, e->mtime);
        PUT(&ix, u64, e->size); PUT(&ix, u32, e->content);
        if (e->type == E_LINK) put_str(&ix, e->link);
    }
    PUT(&ix, u32, nC);
    for (u32 i = 0; i < nC; i++) { PUT(&ix, u64, C[i].size); PUT(&ix, u32, C[i].crc); write_fspec(&ix, &C[i].f); PUT(&ix, u8, C[i].xform); PUT(&ix, u64, C[i].ssize); }
    PUT(&ix, u32, nB);
    for (u32 i = 0; i < nB; i++) {
        Block *b = &B[i];
        PUT(&ix, u64, b->off); PUT(&ix, u64, b->csize); PUT(&ix, u64, b->usize); PUT(&ix, u8, b->codec);
        PUT(&ix, u32, b->np);
        for (u32 k = 0; k < b->np; k++) { PUT(&ix, u32, b->pc[k].content); PUT(&ix, u64, b->pc[k].off); PUT(&ix, u32, b->pc[k].len); }
    }
    lzrc_params pr; lzrc_default_params(&pr, 9);
    u8 *ic = xmalloc(lzrc_bound(ix.n));
    size_t ics = lzrc_compress(ix.p, ix.n, ic, lzrc_bound(ix.n), &pr);
    u8 icodec = CODEC_LZRC;
    if (!ics || ics >= ix.n) { memcpy(ic, ix.p, ix.n); ics = ix.n; icodec = CODEC_STORED; }
    fwrite(ic, 1, ics, out);
    memcpy(hdr, "GPK1", 4);
    u64 iu = ix.n, icz = ics;
    memcpy(hdr + 4, &off, 8); memcpy(hdr + 12, &icz, 8); memcpy(hdr + 20, &iu, 8); hdr[28] = icodec;
    fseek(out, 0, SEEK_SET); fwrite(hdr, 1, HDR_SIZE, out);
    u64 arch = off + ics;
    if (fclose(out)) die("%s", "Schreibfehler");
    double dt = now() - t0;
    fprintf(stderr, "Archiv: %.2f MB (Rate %.3f, %.1f%% des Builds), Index %.1f KB, %.1f s (%.1f MB/s)\n",
            arch / 1e6, (double)total / arch, 100.0 * arch / total, ics / 1e3, dt, total / dt / 1e6);
    return 0;
}

/* ---------------- Archiv lesen ---------------- */
typedef struct { const u8 *map; size_t size; } Archive;

static int safe_path(const char *p) {
    if (!*p || p[0] == '/') return 0;
    for (const char *q = p; *q; q++) if (!plat_path_char_ok(*q)) return 0;
    const char *s = p;
    while (*s) {
        const char *e = strchr(s, '/'); size_t l = e ? (size_t)(e - s) : strlen(s);
        if (l == 0 || (l == 2 && s[0] == '.' && s[1] == '.') || (l == 1 && s[0] == '.')) return 0;
        if (!e) break;
        s = e + 1;
    }
    return 1;
}

static void load_archive(const char *path, Archive *a) {
    u64 sz = plat_file_size(path);
    if (sz == UINT64_MAX) die("Kann Archiv nicht öffnen: %s", path);
    a->size = (size_t)sz;
    a->map = plat_map(path, sz);
    if (!a->map || a->size < HDR_SIZE || memcmp(a->map, "GPK1", 4)) die("Kein GPK-Archiv: %s", path);
    u64 ioff, ics, ius; memcpy(&ioff, a->map + 4, 8); memcpy(&ics, a->map + 12, 8); memcpy(&ius, a->map + 20, 8);
    u8 icodec = a->map[28];
    if (ioff > a->size || ics > a->size - ioff || ius > (1ull << 32)) die("%s", "Index beschädigt");
    u8 *ix = xmalloc(ius);
    if (icodec == CODEC_STORED) { if (ics != ius) die("%s", "Index beschädigt"); memcpy(ix, a->map + ioff, ius); }
    else if (!lzrc_decompress(a->map + ioff, ics, ix, ius)) die("%s", "Index beschädigt");
    Rd r = {ix, ix + ius, 0};
    nE = GET(&r, u32);
    if (nE > ius) die("%s", "Index beschädigt");
    E = xmalloc(sizeof(Entry) * (nE + 1));
    for (u32 i = 0; i < nE && !r.err; i++) {
        Entry *e = &E[i]; memset(e, 0, sizeof *e);
        e->type = GET(&r, u8); e->path = rstr(&r); e->mode = GET(&r, u32); e->mtime = GET(&r, i64);
        e->size = GET(&r, u64); e->content = GET(&r, u32);
        if (e->type == E_LINK) e->link = rstr(&r);
        if (e->type > E_LINK || !safe_path(e->path)) die("Unsicherer Pfad im Archiv: %s", e->path);
    }
    nC = GET(&r, u32);
    if (nC > ius) die("%s", "Index beschädigt");
    C = xmalloc(sizeof(Content) * (nC + 1));
    for (u32 i = 0; i < nC && !r.err; i++) {
        memset(&C[i], 0, sizeof C[i]); C[i].size = GET(&r, u64); C[i].crc = GET(&r, u32); C[i].f = read_fspec(&r);
        C[i].xform = GET(&r, u8); C[i].ssize = GET(&r, u64);
        if (C[i].xform > XF_PNG || (C[i].xform == XF_NONE && C[i].ssize != C[i].size) || (C[i].xform && C[i].ssize > g_block_max_ssize)) r.err = 1;
    }
    nB = GET(&r, u32);
    if (nB > ius) die("%s", "Index beschädigt");
    B = xmalloc(sizeof(Block) * (nB + 1));
    for (u32 i = 0; i < nB && !r.err; i++) {
        Block *b = &B[i]; memset(b, 0, sizeof *b);
        b->off = GET(&r, u64); b->csize = GET(&r, u64); b->usize = GET(&r, u64); b->codec = GET(&r, u8);
        b->np = GET(&r, u32);
        if (b->np > ius || b->off > a->size || b->csize > a->size - b->off || b->usize > (1ull << 31)) die("%s", "Index beschädigt");
        b->pc = xmalloc(sizeof(Piece) * (b->np + 1));
        u64 sum = 0;
        for (u32 k = 0; k < b->np && !r.err; k++) {
            Piece *p = &b->pc[k];
            p->content = GET(&r, u32); p->off = GET(&r, u64); p->len = GET(&r, u32);
            if (p->content >= nC || p->off > C[p->content].ssize || p->len > C[p->content].ssize - p->off ||
                (C[p->content].xform && (p->off || p->len != C[p->content].ssize))) die("%s", "Index beschädigt");
            sum += p->len;
        }
        if (sum != b->usize) die("%s", "Index beschädigt");
    }
    for (u32 i = 0; i < nE; i++)
        if (E[i].type == E_FILE && E[i].size && (E[i].content >= nC || C[E[i].content].size != E[i].size)) die("%s", "Index beschädigt");
    if (r.err) die("%s", "Index beschädigt");
    free(ix);
}

/* ---------------- Entpacken ---------------- */
typedef struct { const Archive *a; const char *outdir; u32 **targets; u32 *ntargets; u32 **piece_crc; atomic_int failed; } XCtx;

static void mkdirs(const char *path) {
    char t[GPK_PATH]; join(t, sizeof t, path, "", "");
    for (char *p = t + 1; *p; p++) if (*p == '/' || *p == '\\') { char c = *p; *p = 0; plat_mkdir(t); *p = c; }
    plat_mkdir(t);
}

static void extract_block(u32 bi, void *vctx) {
    XCtx *x = vctx; Block *b = &B[bi];
    const u8 *src = x->a->map + b->off;
    u8 *buf = xmalloc(b->usize + 16), *tmp = NULL;
    int ok = 1;
    if (b->codec == CODEC_STORED) { if (b->csize != b->usize) ok = 0; else memcpy(buf, src, b->usize); }
    else if (b->codec == CODEC_LZRC) ok = lzrc_decompress(src, b->csize, buf, b->usize);
#ifndef GPK_NO_XZ
    else if (b->codec == CODEC_XZ) {
        uint64_t mem = UINT64_MAX; size_t ip = 0, op = 0;
        ok = lzma_stream_buffer_decode(&mem, 0, NULL, src, &ip, b->csize, buf, &op, b->usize) == LZMA_OK && op == b->usize;
    }
#endif
    else ok = 0;
    if (!ok) { fprintf(stderr, "gpk: Block %u beschädigt\n", bi); atomic_store(&x->failed, 1); free(buf); return; }
    u32 maxlen = 0;
    for (u32 k = 0; k < b->np; k++) if (b->pc[k].len > maxlen) maxlen = b->pc[k].len;
    tmp = xmalloc(maxlen + 1);
    u64 pos = 0;
    for (u32 k = 0; k < b->np; k++) {
        Piece *p = &b->pc[k];
        Content *c = &C[p->content];
        filter_decode(&c->f, buf + pos, tmp, p->len, p->off);
        pos += p->len;
        const u8 *wdata = tmp; u64 wlen = p->len, woff = p->off;
        u8 *orig = NULL;
        if (c->xform == XF_PNG) {                      /* PNG bitgenau wiederherstellen */
            orig = xmalloc(c->size);
            if (!pngx_inverse(tmp, p->len, orig, c->size)) { atomic_store(&x->failed, 1); free(orig); continue; }
            wdata = orig; wlen = c->size; woff = 0;
        }
        x->piece_crc[bi][k] = crc_of(wdata, wlen);
        for (u32 t = 0; t < x->ntargets[p->content]; t++) {
            char f[GPK_PATH]; join(f, sizeof f, x->outdir, "/", E[x->targets[p->content][t]].path);
            int fd = plat_open_write(f, 0);
            if (fd < 0) { atomic_store(&x->failed, 1); continue; }
            if (plat_write_at(fd, wdata, wlen, woff)) atomic_store(&x->failed, 1);
            plat_close(fd);
        }
        free(orig);
    }
    free(tmp); free(buf);
}

static int cmd_extract(const char *archive, const char *outdir) {
    double t0 = now();
    Archive a; load_archive(archive, &a);
    mkdirs(outdir);
    char f[GPK_PATH];
    u32 *ntargets = calloc(nC + 1, sizeof(u32)), **targets = calloc(nC + 1, sizeof(u32 *));
    for (u32 i = 0; i < nE; i++) {
        Entry *e = &E[i];
        join(f, sizeof f, outdir, "/", e->path);
        char *sl = strrchr(f, '/'); *sl = 0; mkdirs(f); *sl = '/';
        if (e->type == E_DIR) { mkdirs(f); continue; }
        if (e->type == E_LINK) continue;
        int fd = plat_open_write(f, 1);
        if (fd < 0) die("Kann Datei nicht anlegen: %s", f);
        if (e->size && plat_set_size(fd, e->size)) die("Kann Dateigröße nicht setzen: %s", f);
        plat_close(fd);
        if (e->size) {
            u32 c = e->content;
            targets[c] = realloc(targets[c], sizeof(u32) * (ntargets[c] + 1));
            targets[c][ntargets[c]++] = i;
        }
    }
    XCtx x = {&a, outdir, targets, ntargets, xmalloc(sizeof(u32 *) * (nB + 1)), 0};
    atomic_init(&x.failed, 0);
    for (u32 i = 0; i < nB; i++) x.piece_crc[i] = xmalloc(sizeof(u32) * (B[i].np + 1));
    parallel_for(nB, extract_block, &x);

    /* CRC je Datei aus den Stück-CRCs kombinieren und prüfen */
    u32 *crc = calloc(nC + 1, sizeof(u32));
    for (u32 i = 0; i < nB; i++)
        for (u32 k = 0; k < B[i].np; k++) {
            Piece *p = &B[i].pc[k];
            crc[p->content] = (u32)crc32_combine(crc[p->content], x.piece_crc[i][k], (z_off_t)(C[p->content].xform ? C[p->content].size : p->len));
        }
    u32 bad = 0;
    for (u32 c = 0; c < nC; c++) if (crc[c] != C[c].crc) bad++;
    /* Symlinks, Rechte, Zeitstempel */
    for (u32 i = 0; i < nE; i++) {
        Entry *e = &E[i];
        join(f, sizeof f, outdir, "/", e->path);
        if (e->type == E_LINK) {
            if (e->link[0] == '/') { fprintf(stderr, "gpk: absoluter Symlink übersprungen: %s\n", e->path); continue; }
            plat_unlink(f);
            if (plat_symlink(e->link, f)) fprintf(stderr, "gpk: Symlink nicht angelegt (auf diesem System nicht unterstützt?): %s\n", e->path);
            continue;
        }
        plat_set_mode(f, e->mode);
        if (e->type == E_FILE) plat_set_mtime(f, e->mtime);
    }
    double dt = now() - t0;
    u64 total = 0; for (u32 i = 0; i < nE; i++) if (E[i].type == E_FILE) total += E[i].size;
    if (bad || atomic_load(&x.failed)) { fprintf(stderr, "gpk: FEHLER – %u Dateien mit falscher Prüfsumme\n", bad); return 1; }
    fprintf(stderr, "Entpackt: %u Einträge, %.1f MB in %.2f s (%.1f MB/s), alle CRCs OK\n", nE, total / 1e6, dt, total / dt / 1e6);
    return 0;
}

static int cmd_list(const char *archive) {
    Archive a; load_archive(archive, &a);
    for (u32 i = 0; i < nE; i++) {
        Entry *e = &E[i];
        const char *flt = (e->type == E_FILE && e->size) ? FILTER_NAMES[C[e->content].f.id] : "";
        printf("%c %04o %12llu  %-14s %s%s%s\n", "fdl"[e->type], e->mode, (unsigned long long)e->size, flt, e->path,
               e->type == E_LINK ? " -> " : "", e->type == E_LINK ? e->link : "");
    }
    u64 csum = 0; for (u32 i = 0; i < nB; i++) csum += B[i].csize;
    printf("%u Einträge, %u eindeutige Inhalte, %u Blöcke, Blockdaten %.2f MB\n", nE, nC, nB, csum / 1e6);
    return 0;
}

static void usage(void) {
    fprintf(stderr,
        "Verwendung:\n"
        "  gpk c [Optionen] <build-ordner> <archiv.gpk>\n"
        "  gpk x [-t N] <archiv.gpk> <ziel-ordner>\n"
        "  gpk l <archiv.gpk>\n"
        "Optionen beim Packen:\n"
        "  -l N         Stufe 1..9 (Standard 9)\n"
        "  -t N         Threads (Standard 4)\n"
        "  -b MiB       Blockgröße (Standard 64)\n"
        "  --xz         xz/LZMA2 statt LZRC als Kern (Vergleich)\n"
        "  --no-filter --no-sort --no-dedup --no-raw   einzelne Stufen abschalten\n"
        "  --no-preproc alle Vorverarbeitungsstufen abschalten\n");
    exit(2);
}

int main(int argc, char **argv) {
    plat_init_console();
    if (argc < 2) usage();
    const char *cmd = argv[1];
    const char *pos[2]; int np = 0;
    for (int i = 2; i < argc; i++) {
        const char *s = argv[i];
        if (!strcmp(s, "-l") && i + 1 < argc) g_level = atoi(argv[++i]);
        else if (!strcmp(s, "-t") && i + 1 < argc) g_threads = atoi(argv[++i]);
        else if (!strcmp(s, "-b") && i + 1 < argc) g_block = (u64)atoi(argv[++i]) << 20;
        else if (!strcmp(s, "--xz")) {
#ifdef GPK_NO_XZ
            die("%s", "--xz ist in diesem Build nicht verfügbar (ohne liblzma gebaut)");
#else
            opt_codec = CODEC_XZ;
#endif
        }
        else if (!strcmp(s, "--no-filter")) opt_filter = 0;
        else if (!strcmp(s, "--no-sort")) opt_sort = 0;
        else if (!strcmp(s, "--no-dedup")) opt_dedup = 0;
        else if (!strcmp(s, "--no-raw")) opt_raw = 0;
        else if (!strcmp(s, "--no-preproc")) opt_filter = opt_sort = opt_dedup = opt_raw = 0;
        else if (np < 2) pos[np++] = s;
        else usage();
    }
    if (g_block < (1u << 20) || g_block > (1ull << 30)) die("%s", "Blockgröße muss 1..1024 MiB sein");
    if (!strcmp(cmd, "c") && np == 2) return cmd_compress(pos[0], pos[1]);
    if (!strcmp(cmd, "x") && np == 2) return cmd_extract(pos[0], pos[1]);
    if (!strcmp(cmd, "l") && np == 1) return cmd_list(pos[0]);
    usage();
    return 2;
}
