/*
 * PNG recompression following the Precomp/Preflate principle (simplified):
 * the IDAT chunks of a PNG contain a zlib stream. If it was produced by zlib,
 * it can be recreated bit-exactly with the same parameters (level, memLevel,
 * strategy). Then, instead of the Deflate stream, we store the inflated image
 * data (which LZRC compresses much better) plus the parameters.
 * Streams from other encoders (zopfli, oxipng ...) are not transformed.
 *
 * Format: "PNGX" | level | memLevel | strategy | 0 | u32 skel_len | skeleton | u32 raw_len | raw
 * skeleton = PNG without the IDAT payload (length, type and CRC of every chunk remain).
 */
#include "pngx.h"
#include "pxc.h"
#include <stdlib.h>
#include <string.h>
#include <zlib.h>

typedef uint8_t u8; typedef uint32_t u32; typedef uint64_t u64;
static u32 be32(const u8 *p) { return (u32)p[0] << 24 | (u32)p[1] << 16 | (u32)p[2] << 8 | p[3]; }
static void wr32(u8 *p, u32 v) { memcpy(p, &v, 4); }
static u32 rd32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }
#define MAX_RAW (512u << 20)

static u8 *deflate_with(const u8 *raw, size_t rn, int level, int mem, int strat, size_t *outn) {
    z_stream z; memset(&z, 0, sizeof z);
    if (deflateInit2(&z, level, Z_DEFLATED, 15, mem, strat) != Z_OK) return NULL;
    size_t cap = deflateBound(&z, rn) + 64;
    u8 *o = malloc(cap);
    z.next_in = (u8 *)raw; z.avail_in = (uInt)rn; z.next_out = o; z.avail_out = (uInt)cap;
    int r = deflate(&z, Z_FINISH);
    *outn = z.total_out; deflateEnd(&z);
    if (r != Z_STREAM_END) { free(o); return NULL; }
    return o;
}

/* ---- PNG row filters (8 bits per channel, bpp = channels) ---- */
static inline u8 paeth(int a, int b, int c) {
    int p = a + b - c, pa = abs(p - a), pb = abs(p - b), pc = abs(p - c);
    return (u8)((pa <= pb && pa <= pc) ? a : (pb <= pc ? b : c));
}
static inline u8 png_pred(int f, const u8 *cur, const u8 *prior, size_t i, int bpp) {
    int a = i >= (size_t)bpp ? cur[i - bpp] : 0, b = prior ? prior[i] : 0, c = (prior && i >= (size_t)bpp) ? prior[i - bpp] : 0;
    switch (f) { case 1: return (u8)a; case 2: return (u8)b; case 3: return (u8)((a + b) >> 1); case 4: return paeth(a, b, c); default: return 0; }
}
/* raw (h rows of 1+w*bpp bytes) -> pixels + filter types; 0 for invalid filter types */
static int png_unfilter(const u8 *raw, u32 w, u32 h, int bpp, u8 *px, u8 *filt) {
    size_t rl = (size_t)w * bpp;
    for (u32 y = 0; y < h; y++) {
        const u8 *src = raw + (size_t)y * (rl + 1);
        int f = src[0]; if (f > 4) return 0;
        filt[y] = (u8)f;
        u8 *cur = px + (size_t)y * rl; const u8 *prior = y ? cur - rl : NULL;
        for (size_t i = 0; i < rl; i++) cur[i] = (u8)(src[1 + i] + png_pred(f, cur, prior, i, bpp));
    }
    return 1;
}
static void png_refilter(const u8 *px, const u8 *filt, u32 w, u32 h, int bpp, u8 *raw) {
    size_t rl = (size_t)w * bpp;
    for (u32 y = 0; y < h; y++) {
        u8 *dst = raw + (size_t)y * (rl + 1);
        const u8 *cur = px + (size_t)y * rl, *prior = y ? cur - rl : NULL;
        dst[0] = filt[y];
        for (size_t i = 0; i < rl; i++) dst[1 + i] = (u8)(cur[i] - png_pred(filt[y], cur, prior, i, bpp));
    }
}
/* read IHDR: 8 bits only, not interlaced, gray/RGB/gray+alpha/RGBA */
static int png_geom(const u8 *png, size_t n, u32 *w, u32 *h, int *ch) {
    if (n < 33 || memcmp(png + 12, "IHDR", 4) || be32(png + 8) != 13) return 0;
    *w = be32(png + 16); *h = be32(png + 20);
    int depth = png[24], ct = png[25], il = png[28];
    if (depth != 8 || il != 0 || !*w || !*h) return 0;
    *ch = ct == 0 ? 1 : ct == 2 ? 3 : ct == 4 ? 2 : ct == 6 ? 4 : 0;
    return *ch && (u64)*w * *h * (u64)*ch <= MAX_RAW;
}

/* header + skeleton + remaining data -> assemble the original PNG (both variants) */
static int assemble(const u8 *skel, u32 sk, const u8 *raw, size_t rn, int lvl, int mem, int strat, u8 *out, size_t origsize) {
    size_t zn; u8 *zs = deflate_with(raw, rn, lvl, mem, strat, &zn);
    if (!zs) return 0;
    size_t sp = 8, op = 8, zp = 0; int ok = 0;
    if (sk < 8 || origsize < 8) goto done;
    memcpy(out, skel, 8);
    while (sp + 12 <= sk) {
        u32 len = be32(skel + sp);
        int idat = !memcmp(skel + sp + 4, "IDAT", 4);
        size_t need = idat ? 12 : 12 + (size_t)len;
        if (sp + need > sk || op + 12 + (size_t)len > origsize) goto done;
        memcpy(out + op, skel + sp, 8);
        if (idat) { if (zp + len > zn) goto done; memcpy(out + op + 8, zs + zp, len); zp += len; memcpy(out + op + 8 + len, skel + sp + 8, 4); }
        else memcpy(out + op + 8, skel + sp + 8, (size_t)len + 4);
        sp += need; op += 12 + (size_t)len;
    }
    ok = sp == sk && op == origsize && zp == zn;
done:
    free(zs);
    return ok;
}

uint8_t *pngx_forward(const uint8_t *png, size_t n, size_t *tsize) { return pngx_forward_mode(png, n, 1, tsize); }

int pngx_channels(const uint8_t *png, size_t n) { u32 w, h; int ch; return png_geom(png, n, &w, &h, &ch) ? ch : 0; }

uint8_t *pngx_forward_mode(const uint8_t *png, size_t n, int mode, size_t *tsize) {
    static const u8 SIG[8] = {0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n'};
    if (n < 8 + 12 || memcmp(png, SIG, 8) || n > MAX_RAW) return NULL;
    /* walk the chunks: build the skeleton, collect the IDAT data */
    u8 *skel = malloc(n), *zs = malloc(n);
    size_t sk = 0, zn = 0, p = 8;
    memcpy(skel, png, 8); sk = 8;
    int ok = 0;
    while (p + 12 <= n) {
        u32 len = be32(png + p);
        if (len > n - p - 12) goto fail;
        const u8 *type = png + p + 4;
        memcpy(skel + sk, png + p, 8); sk += 8;
        if (!memcmp(type, "IDAT", 4)) { memcpy(zs + zn, png + p + 8, len); zn += len; }
        else { memcpy(skel + sk, png + p + 8, len); sk += len; }
        memcpy(skel + sk, png + p + 8 + len, 4); sk += 4;
        p += 12 + (size_t)len;
        if (!memcmp(type, "IEND", 4)) { ok = 1; break; }
    }
    if (!ok || p != n || zn == 0) goto fail;
    /* inflate */
    z_stream z; memset(&z, 0, sizeof z);
    if (inflateInit(&z) != Z_OK) goto fail;
    size_t rcap = zn * 4 + 65536, rn = 0;
    u8 *raw = malloc(rcap);
    z.next_in = zs; z.avail_in = (uInt)zn;
    int r;
    do {
        if (rn == rcap) { if (rcap >= MAX_RAW) { inflateEnd(&z); free(raw); goto fail; } rcap *= 2; raw = realloc(raw, rcap); }
        z.next_out = raw + rn; z.avail_out = (uInt)(rcap - rn);
        r = inflate(&z, Z_NO_FLUSH);
        rn = z.total_out;
    } while (r == Z_OK);
    int consumed_all = z.total_in == zn;
    inflateEnd(&z);
    if (r != Z_STREAM_END || !consumed_all) { free(raw); goto fail; }
    /* search the parameters with which zlib produces the stream bit-exactly */
    static const int LV[] = {9, 6, 8, 7, 5, 4, 3, 2, 1};
    for (int si = 0; si < 2; si++)
        for (int mi = 0; mi < 2; mi++)
            for (int li = 0; li < 9; li++) {
                int strat = si ? Z_FILTERED : Z_DEFAULT_STRATEGY, mem = mi ? 9 : 8, lvl = LV[li];
                size_t dn; u8 *d = deflate_with(raw, rn, lvl, mem, strat, &dn);
                int match = d && dn == zn && !memcmp(d, zs, zn);
                free(d);
                if (!match) continue;
                size_t tn;
                u8 *t = NULL;
                if (mode == 2) {                             /* pixel coder instead of raw data */
                    u32 w, h; int ch;
                    if (!png_geom(png, n, &w, &h, &ch) || rn != (size_t)h * (1 + (size_t)w * ch)) goto nomatch;
                    u8 *px = malloc((size_t)w * h * ch), *filt = malloc(h);
                    size_t pn = 0; u8 *pc = NULL;
                    if (px && filt && png_unfilter(raw, w, h, ch, px, filt)) pc = pxc_encode(px, w, h, ch, &pn);
                    free(px);
                    if (!pc) { free(filt); goto nomatch; }
                    tn = 8 + 4 + sk + 12 + h + 4 + pn;
                    t = malloc(tn);
                    memcpy(t, "PNGY", 4); t[4] = (u8)lvl; t[5] = (u8)mem; t[6] = (u8)strat; t[7] = 0;
                    wr32(t + 8, (u32)sk); memcpy(t + 12, skel, sk);
                    u8 *q = t + 12 + sk;
                    wr32(q, w); wr32(q + 4, h); q[8] = (u8)ch; q[9] = q[10] = q[11] = 0;
                    memcpy(q + 12, filt, h); wr32(q + 12 + h, (u32)pn); memcpy(q + 16 + h, pc, pn);
                    free(filt); free(pc);
                } else {
                    tn = 8 + 4 + sk + 4 + rn;
                    t = malloc(tn);
                    memcpy(t, "PNGX", 4); t[4] = (u8)lvl; t[5] = (u8)mem; t[6] = (u8)strat; t[7] = 0;
                    wr32(t + 8, (u32)sk); memcpy(t + 12, skel, sk);
                    wr32(t + 12 + sk, (u32)rn); memcpy(t + 16 + sk, raw, rn);
                }
                free(raw); free(skel); free(zs);
                /* safety net: check the reconstruction up front */
                u8 *chk = malloc(n);
                int good = pngx_inverse(t, tn, chk, n) && !memcmp(chk, png, n);
                free(chk);
                if (!good) { free(t); return NULL; }
                *tsize = tn;
                return t;
            }
nomatch:
    free(raw);
fail:
    free(skel); free(zs);
    return NULL;
}


int pngx_inverse(const uint8_t *t, size_t tn, uint8_t *out, size_t origsize) {
    if (tn < 16) return 0;
    int v2 = !memcmp(t, "PNGY", 4);
    if (!v2 && memcmp(t, "PNGX", 4)) return 0;
    int lvl = t[4], mem = t[5], strat = t[6];
    if (lvl < 1 || lvl > 9 || mem < 1 || mem > 9 || strat > 4) return 0;
    u32 sk = rd32(t + 8);
    if (sk > tn - 16) return 0;
    const u8 *skel = t + 12;
    if (!v2) {
        u32 rn = rd32(t + 12 + sk);
        if (rn != tn - 16 - sk) return 0;
        return assemble(skel, sk, t + 16 + sk, rn, lvl, mem, strat, out, origsize);
    }
    const u8 *q = t + 12 + sk;
    if ((size_t)(tn - 12 - sk) < 16) return 0;
    u32 w = rd32(q), h = rd32(q + 4); int ch = q[8];
    if (!w || !h || ch < 1 || ch > 4 || (u64)w * h * (u64)ch > MAX_RAW) return 0;
    if ((u64)12 + sk + 12 + h + 4 > tn) return 0;
    const u8 *filt = q + 12;
    for (u32 y = 0; y < h; y++) if (filt[y] > 4) return 0;
    u32 pn = rd32(q + 12 + h);
    if ((u64)12 + sk + 16 + h + pn != tn) return 0;
    size_t rn = (size_t)h * (1 + (size_t)w * ch);
    u8 *px = malloc((size_t)w * h * ch), *raw = malloc(rn);
    int ok = px && raw && pxc_decode(q + 16 + h, pn, px, w, h, ch);
    if (ok) { png_refilter(px, filt, w, h, ch, raw); ok = assemble(skel, sk, raw, rn, lvl, mem, strat, out, origsize); }
    free(px); free(raw);
    return ok;
}
