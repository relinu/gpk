/*
 * PNG-Rekompression nach dem Precomp/Preflate-Prinzip (vereinfacht):
 * Die IDAT-Chunks eines PNG enthalten einen zlib-Strom. Wurde er von zlib
 * erzeugt, lässt er sich mit denselben Parametern (Level, memLevel, Strategie)
 * bitgenau neu erzeugen. Dann speichern wir statt des Deflate-Stroms die
 * entpackten Bilddaten (die LZRC deutlich besser komprimiert) plus die Parameter.
 * Streams anderer Encoder (zopfli, oxipng ...) werden nicht umgewandelt.
 *
 * Format: "PNGX" | level | memLevel | strategy | 0 | u32 skel_len | Skelett | u32 raw_len | raw
 * Skelett = PNG ohne IDAT-Nutzdaten (Länge, Typ und CRC jedes Chunks bleiben).
 */
#include "pngx.h"
#include <stdlib.h>
#include <string.h>
#include <zlib.h>

typedef uint8_t u8; typedef uint32_t u32;
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

uint8_t *pngx_forward(const uint8_t *png, size_t n, size_t *tsize) {
    static const u8 SIG[8] = {0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n'};
    if (n < 8 + 12 || memcmp(png, SIG, 8) || n > MAX_RAW) return NULL;
    /* Chunks durchgehen: Skelett bauen, IDAT-Daten sammeln */
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
    /* Entpacken */
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
    /* Parameter suchen, mit denen zlib den Strom bitgenau erzeugt */
    static const int LV[] = {9, 6, 8, 7, 5, 4, 3, 2, 1};
    for (int si = 0; si < 2; si++)
        for (int mi = 0; mi < 2; mi++)
            for (int li = 0; li < 9; li++) {
                int strat = si ? Z_FILTERED : Z_DEFAULT_STRATEGY, mem = mi ? 9 : 8, lvl = LV[li];
                size_t dn; u8 *d = deflate_with(raw, rn, lvl, mem, strat, &dn);
                int match = d && dn == zn && !memcmp(d, zs, zn);
                free(d);
                if (!match) continue;
                size_t tn = 8 + 4 + sk + 4 + rn;
                u8 *t = malloc(tn);
                memcpy(t, "PNGX", 4); t[4] = (u8)lvl; t[5] = (u8)mem; t[6] = (u8)strat; t[7] = 0;
                wr32(t + 8, (u32)sk); memcpy(t + 12, skel, sk);
                wr32(t + 12 + sk, (u32)rn); memcpy(t + 16 + sk, raw, rn);
                free(raw); free(skel); free(zs);
                /* Sicherheitsnetz: Rekonstruktion vorab prüfen */
                u8 *chk = malloc(n);
                int good = pngx_inverse(t, tn, chk, n) && !memcmp(chk, png, n);
                free(chk);
                if (!good) { free(t); return NULL; }
                *tsize = tn;
                return t;
            }
    free(raw);
fail:
    free(skel); free(zs);
    return NULL;
}

int pngx_inverse(const uint8_t *t, size_t tn, uint8_t *out, size_t origsize) {
    if (tn < 16 || memcmp(t, "PNGX", 4)) return 0;
    int lvl = t[4], mem = t[5], strat = t[6];
    if (lvl < 1 || lvl > 9 || mem < 1 || mem > 9 || strat > 4) return 0;
    u32 sk = rd32(t + 8);
    if (sk > tn - 16) return 0;
    const u8 *skel = t + 12;
    u32 rn = rd32(t + 12 + sk);
    if (rn != tn - 16 - sk) return 0;
    size_t zn; u8 *zs = deflate_with(t + 16 + sk, rn, lvl, mem, strat, &zn);
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
