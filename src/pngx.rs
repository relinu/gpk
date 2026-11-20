//! Bitgenaue PNG-Rekompression (Port von `legacy-c/gpk/pngx.c`, formatgleich).
//!
//! Der IDAT-zlib-Strom wird entpackt, wenn zlib ihn mit bestimmten Parametern
//! (Level, memLevel, Strategie) bitgenau neu erzeugt. Gespeichert wird dann:
//! - Variante 1 `PNGX`: Kopf + Skelett (PNG ohne IDAT-Nutzdaten) + entpackte Rohdaten
//! - Variante 2 `PNGY`: Kopf + Skelett + Zeilenfiltertypen + Pixel-Coder-Daten
//!
//! Jede Umwandlung wird vor der Rückgabe vollständig rekonstruiert und geprüft.

use crate::{deflate, inflate, pxc};

const MAX_RAW: usize = 512 << 20;
const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];

#[inline]
fn be32(p: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]])
}
#[inline]
fn rd32(p: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]])
}

// ---- PNG-Zeilenfilter (8 Bit je Kanal, bpp = Kanäle) ----
#[inline]
fn paeth(a: i32, b: i32, c: i32) -> u8 {
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    (if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }) as u8
}
#[inline]
fn png_pred(f: u8, cur: &[u8], prior: Option<&[u8]>, i: usize, bpp: usize) -> u8 {
    let a = if i >= bpp { cur[i - bpp] as i32 } else { 0 };
    let b = prior.map_or(0, |p| p[i] as i32);
    let c = match prior {
        Some(p) if i >= bpp => p[i - bpp] as i32,
        _ => 0,
    };
    match f {
        1 => a as u8,
        2 => b as u8,
        3 => ((a + b) >> 1) as u8,
        4 => paeth(a, b, c),
        _ => 0,
    }
}
/// raw (h Zeilen à 1+w*bpp) -> Pixel + Filtertypen
fn png_unfilter(raw: &[u8], w: usize, h: usize, bpp: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let rl = w * bpp;
    let mut px = vec![0u8; rl * h];
    let mut filt = vec![0u8; h];
    for y in 0..h {
        let src = &raw[y * (rl + 1)..(y + 1) * (rl + 1)];
        let f = src[0];
        if f > 4 {
            return None;
        }
        filt[y] = f;
        let (before, rest) = px.split_at_mut(y * rl);
        let prior = if y > 0 { Some(&before[(y - 1) * rl..]) } else { None };
        let cur = &mut rest[..rl];
        for i in 0..rl {
            cur[i] = src[1 + i].wrapping_add(png_pred(f, cur, prior, i, bpp));
        }
    }
    Some((px, filt))
}
fn png_refilter(px: &[u8], filt: &[u8], w: usize, h: usize, bpp: usize) -> Vec<u8> {
    let rl = w * bpp;
    let mut raw = vec![0u8; h * (rl + 1)];
    for y in 0..h {
        let dst = &mut raw[y * (rl + 1)..(y + 1) * (rl + 1)];
        let cur = &px[y * rl..(y + 1) * rl];
        let prior = if y > 0 { Some(&px[(y - 1) * rl..y * rl]) } else { None };
        dst[0] = filt[y];
        for i in 0..rl {
            dst[1 + i] = cur[i].wrapping_sub(png_pred(filt[y], cur, prior, i, bpp));
        }
    }
    raw
}

/// IHDR: nur 8 Bit, nicht interlaced, Grau/RGB/Grau+Alpha/RGBA → (w, h, Kanäle)
fn png_geom(png: &[u8]) -> Option<(u32, u32, usize)> {
    if png.len() < 33 || &png[12..16] != b"IHDR" || be32(png, 8) != 13 {
        return None;
    }
    let (w, h) = (be32(png, 16), be32(png, 20));
    let (depth, ct, il) = (png[24], png[25], png[28]);
    if depth != 8 || il != 0 || w == 0 || h == 0 {
        return None;
    }
    let ch = match ct {
        0 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    if w as u64 * h as u64 * ch as u64 > MAX_RAW as u64 {
        return None;
    }
    Some((w, h, ch))
}

/// Kanäle eines geeigneten PNG (für Filter-Schätzungen), sonst 0.
pub fn channels(png: &[u8]) -> usize {
    png_geom(png).map_or(0, |g| g.2)
}

/// Bei der Selbstprüfung bekannt: Rohdaten und der dazu bereits bestätigte Deflate-Strom.
type Known<'a> = Option<(&'a [u8], &'a [u8])>;

/// Original aus Skelett + Rohdaten + zlib-Parametern zusammensetzen. Mit `known` (Selbstprüfung
/// beim Packen) müssen die rekonstruierten Rohdaten den Originalrohdaten gleichen; dann wird der
/// schon Byte für Byte bestätigte Strom verwendet, statt ein zweites Mal zu komprimieren.
fn assemble(skel: &[u8], raw: &[u8], lvl: i32, mem: i32, strat: i32, out: &mut [u8], known: Known) -> bool {
    let fresh;
    let zs: &[u8] = match known {
        Some((kraw, kzs)) => {
            if raw != kraw {
                return false;
            }
            kzs
        }
        None => {
            let Some(z) = deflate::deflate_with(raw, lvl, mem, strat) else { return false };
            fresh = z;
            &fresh
        }
    };
    let (sk, origsize) = (skel.len(), out.len());
    if sk < 8 || origsize < 8 {
        return false;
    }
    out[..8].copy_from_slice(&skel[..8]);
    let (mut sp, mut op, mut zp) = (8usize, 8usize, 0usize);
    while sp + 12 <= sk {
        let len = be32(skel, sp) as usize;
        let idat = &skel[sp + 4..sp + 8] == b"IDAT";
        let need = if idat { 12 } else { 12 + len };
        if sp + need > sk || op + 12 + len > origsize {
            return false;
        }
        out[op..op + 8].copy_from_slice(&skel[sp..sp + 8]);
        if idat {
            if zp + len > zs.len() {
                return false;
            }
            out[op + 8..op + 8 + len].copy_from_slice(&zs[zp..zp + len]);
            zp += len;
            out[op + 8 + len..op + 12 + len].copy_from_slice(&skel[sp + 8..sp + 12]);
        } else {
            out[op + 8..op + 12 + len].copy_from_slice(&skel[sp + 8..sp + 12 + len]);
        }
        sp += need;
        op += 12 + len;
    }
    sp == sk && op == origsize && zp == zs.len()
}

/// mode 1: Rohdaten (PNGX), mode 2: Pixel-Coder (PNGY). `None`, wenn nicht anwendbar
/// oder nicht bitgenau reproduzierbar.
pub fn forward(png: &[u8], mode: u32) -> Option<Vec<u8>> {
    let n = png.len();
    if n < 8 + 12 || png[..8] != SIG || n > MAX_RAW {
        return None;
    }
    // Chunks: Skelett bauen, IDAT-Daten sammeln
    let mut skel = Vec::with_capacity(n.min(1 << 16));
    let mut zs = Vec::with_capacity(n);
    skel.extend_from_slice(&png[..8]);
    let mut p = 8usize;
    let mut ok = false;
    while p + 12 <= n {
        let len = be32(png, p) as usize;
        if len > n - p - 12 {
            return None;
        }
        let ty = &png[p + 4..p + 8];
        skel.extend_from_slice(&png[p..p + 8]);
        if ty == b"IDAT" {
            zs.extend_from_slice(&png[p + 8..p + 8 + len]);
        } else {
            skel.extend_from_slice(&png[p + 8..p + 8 + len]);
        }
        skel.extend_from_slice(&png[p + 8 + len..p + 12 + len]);
        p += 12 + len;
        if ty == b"IEND" {
            ok = true;
            break;
        }
    }
    if !ok || p != n || zs.is_empty() {
        return None;
    }
    let raw = inflate::inflate_zlib(&zs, MAX_RAW)?;
    // Parameter suchen, mit denen zlib den Strom bitgenau erzeugt
    const LV: [i32; 9] = [9, 6, 8, 7, 5, 4, 3, 2, 1];
    for strat in [0, 1] {
        // Z_DEFAULT_STRATEGY, Z_FILTERED
        for mem in [8, 9] {
            for lvl in LV {
                // Vergleich läuft mit und bricht beim ersten abweichenden Block ab
                if !deflate::matches(&raw, lvl, mem, strat, &zs) {
                    continue;
                }
                let mut t = Vec::new();
                t.extend_from_slice(if mode == 2 { b"PNGY" } else { b"PNGX" });
                t.extend_from_slice(&[lvl as u8, mem as u8, strat as u8, 0]);
                t.extend_from_slice(&(skel.len() as u32).to_le_bytes());
                t.extend_from_slice(&skel);
                if mode == 2 {
                    let (w, h, ch) = png_geom(png)?;
                    if raw.len() != h as usize * (1 + w as usize * ch) {
                        return None;
                    }
                    let (px, filt) = png_unfilter(&raw, w as usize, h as usize, ch)?;
                    let pc = pxc::encode(&px, w, h, ch)?;
                    t.extend_from_slice(&w.to_le_bytes());
                    t.extend_from_slice(&h.to_le_bytes());
                    t.extend_from_slice(&[ch as u8, 0, 0, 0]);
                    t.extend_from_slice(&filt);
                    t.extend_from_slice(&(pc.len() as u32).to_le_bytes());
                    t.extend_from_slice(&pc);
                } else {
                    t.extend_from_slice(&(raw.len() as u32).to_le_bytes());
                    t.extend_from_slice(&raw);
                }
                // Sicherheitsnetz: Rekonstruktion vorab prüfen (der Deflate-Strom ist durch
                // `matches` schon bestätigt; geprüft werden Rohdaten, Pixel-Coder und Aufbau)
                let mut chk = vec![0u8; n];
                return (inverse_impl(&t, &mut chk, Some((&raw, &zs))) && chk == png).then_some(t);
            }
        }
    }
    None
}

/// Rekonstruiert das Original (`out.len()` = Originalgröße). `false` bei ungültigen Daten.
pub fn inverse(t: &[u8], out: &mut [u8]) -> bool {
    inverse_impl(t, out, None)
}

fn inverse_impl(t: &[u8], out: &mut [u8], known: Known) -> bool {
    let tn = t.len();
    if tn < 16 {
        return false;
    }
    let v2 = &t[..4] == b"PNGY";
    if !v2 && &t[..4] != b"PNGX" {
        return false;
    }
    let (lvl, mem, strat) = (t[4] as i32, t[5] as i32, t[6] as i32);
    if !(1..=9).contains(&lvl) || !(1..=9).contains(&mem) || strat > 4 {
        return false;
    }
    let sk = rd32(t, 8) as usize;
    if sk > tn - 16 {
        return false;
    }
    let skel = &t[12..12 + sk];
    if !v2 {
        let rn = rd32(t, 12 + sk) as usize;
        if rn != tn - 16 - sk {
            return false;
        }
        return assemble(skel, &t[16 + sk..], lvl, mem, strat, out, known);
    }
    let q = &t[12 + sk..];
    if q.len() < 16 {
        return false;
    }
    let (w, h, ch) = (rd32(q, 0), rd32(q, 4), q[8] as usize);
    if w == 0 || h == 0 || !(1..=4).contains(&ch) || w as u64 * h as u64 * ch as u64 > MAX_RAW as u64 {
        return false;
    }
    let hh = h as usize;
    if 12 + hh + 4 > q.len() {
        return false;
    }
    let filt = &q[12..12 + hh];
    if filt.iter().any(|&f| f > 4) {
        return false;
    }
    let pn = rd32(q, 12 + hh) as usize;
    if 16 + hh + pn != q.len() {
        return false;
    }
    let mut px = vec![0u8; w as usize * hh * ch];
    if !pxc::decode(&q[16 + hh..], &mut px, w, h, ch) {
        return false;
    }
    let raw = png_refilter(&px, filt, w as usize, hh, ch);
    assemble(skel, &raw, lvl, mem, strat, out, known)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn chunk(t: &[u8], d: &[u8]) -> Vec<u8> {
        let mut v = (d.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(d);
        let mut h = crc32fast::Hasher::new();
        h.update(t);
        h.update(d);
        v.extend_from_slice(&h.finalize().to_be_bytes());
        v
    }
    /// Test-PNG mit zlib-Level `lvl` (RGBA 8 Bit, gemischte Zeilenfilter)
    pub fn make_png(w: u32, h: u32, lvl: i32) -> Vec<u8> {
        let mut raw = Vec::new();
        for y in 0..h {
            raw.push((y % 5) as u8);
            for x in 0..w {
                raw.extend_from_slice(&[(x ^ y) as u8, (x * 3) as u8, (y * 7) as u8, 255]);
            }
        }
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut png = SIG.to_vec();
        png.extend(chunk(b"IHDR", &ihdr));
        png.extend(chunk(b"IDAT", &deflate::deflate_with(&raw, lvl, 8, 0).unwrap()));
        png.extend(chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn beide_varianten_bitgenau() {
        for lvl in [1, 6, 9] {
            let png = make_png(64, 40, lvl);
            for mode in [1, 2] {
                let t = forward(&png, mode).expect("umwandelbar");
                let mut out = vec![0u8; png.len()];
                assert!(inverse(&t, &mut out));
                assert_eq!(out, png);
            }
        }
        assert!(forward(b"kein png", 1).is_none());
    }
}
