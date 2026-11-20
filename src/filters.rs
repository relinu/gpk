//! Umkehrbare Vorfilter für Spieldateien (Port von `legacy-c/gpk/filters.c`).
//!
//! Alle Filter erhalten die Länge: `encode` und `decode` arbeiten auf gleich
//! langen Puffern. Bei beschädigten Daten liefern sie Unsinn, aber nie einen
//! Speicherfehler oder Panic.

pub const FT_NONE: u8 = 0;
pub const FT_DELTA: u8 = 1;
pub const FT_SHUF: u8 = 2;
pub const FT_SHUFDELTA: u8 = 3;
pub const FT_BCJ: u8 = 4;
pub const FT_SPLIT: u8 = 5;
pub const FT_WPRED: u8 = 6;
pub const FT_X86S: u8 = 7;
pub const FT_COUNT: u8 = 8;

pub const FILTER_NAMES: [&str; FT_COUNT as usize] =
    ["none", "delta", "shuffle", "shuffle+delta", "x86-bcj", "split", "wort-praediktion", "x86-split"];

/// Filterbeschreibung (Feldlayout wie im Archivindex).
/// FT_WPRED: `stride` = Datensatzgröße, `fields[0]` = Wortgröße (2|4),
/// `fields[1]` = Ordnung (1|2), `fields[2]` = 1 → Byte-Planes nach Datensatz.
/// FT_SPLIT: `fields[..nfields]` = Feldbreiten, `skip` = Kopfbytes am Dateianfang.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct FSpec {
    pub id: u8,
    pub stride: u8,
    pub nfields: u8,
    pub fields: [u8; 6],
    pub skip: u32,
}

impl FSpec {
    pub fn new(id: u8) -> Self {
        FSpec { id, ..Default::default() }
    }
    pub fn with_stride(id: u8, stride: u8) -> Self {
        FSpec { id, stride, ..Default::default() }
    }
    /// Plausibilitätsprüfung beim Lesen eines Archivs.
    pub fn is_valid(&self) -> bool {
        if self.id >= FT_COUNT || self.nfields > 6 {
            return false;
        }
        match self.id {
            FT_DELTA | FT_SHUF | FT_SHUFDELTA => self.stride != 0,
            FT_SPLIT => self.fields[..self.nfields as usize].iter().map(|&w| w as u32).sum::<u32>() != 0,
            FT_WPRED => {
                let w = self.fields[0];
                (w == 2 || w == 4) && (1..=2).contains(&self.fields[1]) && self.stride != 0 && self.stride % w == 0
            }
            _ => true,
        }
    }
}

#[inline]
fn rd32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
#[inline]
fn wr32(b: &mut [u8], i: usize, v: u32) {
    b[i..i + 4].copy_from_slice(&v.to_le_bytes());
}

// ---- Byte-Planes -------------------------------------------------------
fn shuffle(inp: &[u8], out: &mut [u8], s: usize) {
    let n = inp.len();
    if s == 0 {
        out.copy_from_slice(inp);
        return;
    }
    let m = n / s;
    for (j, rec) in inp[..m * s].chunks_exact(s).enumerate() {
        for (k, &b) in rec.iter().enumerate() {
            out[k * m + j] = b;
        }
    }
    out[m * s..].copy_from_slice(&inp[m * s..]);
}
fn unshuffle(inp: &[u8], out: &mut [u8], s: usize) {
    let n = inp.len();
    if s == 0 {
        out.copy_from_slice(inp);
        return;
    }
    let m = n / s;
    for (j, rec) in out[..m * s].chunks_exact_mut(s).enumerate() {
        for (k, b) in rec.iter_mut().enumerate() {
            *b = inp[k * m + j];
        }
    }
    out[m * s..].copy_from_slice(&inp[m * s..]);
}

// ---- x86 BCJ -----------------------------------------------------------
/// E8/E9 mit kleinem Operanden (oberstes Byte 00/FF): Ziel absolut machen
/// (25-Bit-Regel, bijektiv). Nach jedem E8/E9 werden die 4 Folgebytes immer
/// übersprungen, damit Encoder und Decoder dieselben Entscheidungen treffen.
fn bcj(b: &mut [u8], off: u64, enc: bool) {
    let n = b.len();
    let mut i = 0;
    while i + 5 <= n {
        if b[i] & 0xFE == 0xE8 {
            if b[i + 4] == 0x00 || b[i + 4] == 0xFF {
                let p = off.wrapping_add(i as u64) as u32;
                let mut v = rd32(b, i + 1);
                v = if enc { v.wrapping_add(p) } else { v.wrapping_sub(p) };
                v &= 0x01FF_FFFF;
                if v & 0x0100_0000 != 0 {
                    v |= 0xFE00_0000;
                }
                wr32(b, i + 1, v);
            }
            i += 5;
        } else {
            i += 1;
        }
    }
}

// ---- x86 mit getrennten Strömen (BCJ2-Prinzip) ---------------------------
#[inline]
fn x86_abs(v: u32, pos: u32, enc: bool) -> u32 {
    if v >> 24 != 0x00 && v >> 24 != 0xFF {
        return v;
    }
    let mut v = if enc { v.wrapping_add(pos) } else { v.wrapping_sub(pos) };
    v &= 0x01FF_FFFF;
    if v & 0x0100_0000 != 0 {
        v |= 0xFE00_0000;
    }
    v
}
/// Länge des Opcodes (1 für E8/E9, 2 für 0F 8x), wenn ein 4-Byte-Operand folgt.
#[inline]
fn x86_oplen(b0: u8, b1: Option<u8>, i: usize, n: usize) -> usize {
    if b0 & 0xFE == 0xE8 && i + 5 <= n {
        1
    } else if b0 == 0x0F && i + 6 <= n && b1.is_some_and(|b| b & 0xF0 == 0x80) {
        2
    } else {
        0
    }
}
fn x86s_enc(inp: &[u8], out: &mut [u8], off: u64) {
    let n = inp.len();
    let (mut i, mut mp, mut k) = (0usize, 0usize, 0usize);
    while i < n {
        let ol = x86_oplen(inp[i], inp.get(i + 1).copied(), i, n);
        if ol == 0 {
            out[mp] = inp[i];
            mp += 1;
            i += 1;
            continue;
        }
        out[mp..mp + ol].copy_from_slice(&inp[i..i + ol]);
        mp += ol;
        let v = x86_abs(rd32(inp, i + ol), off.wrapping_add((i + ol + 4) as u64) as u32, true);
        k += 1;
        out[n - 4 * k..n - 4 * k + 4].copy_from_slice(&v.to_be_bytes());
        i += ol + 4;
    }
}
fn x86s_dec(inp: &[u8], out: &mut [u8], off: u64) {
    let n = inp.len();
    let (mut i, mut mp, mut k) = (0usize, 0usize, 0usize);
    // mp <= i und 4k <= i - mp gelten immer, daher bleiben alle Zugriffe in [0, n).
    while i < n {
        let ol = x86_oplen(inp[mp], inp.get(mp + 1).copied(), i, n);
        if ol == 0 {
            out[i] = inp[mp];
            mp += 1;
            i += 1;
            continue;
        }
        out[i..i + ol].copy_from_slice(&inp[mp..mp + ol]);
        mp += ol;
        k += 1;
        let p = n - 4 * k;
        let v = u32::from_be_bytes([inp[p], inp[p + 1], inp[p + 2], inp[p + 3]]);
        wr32(out, i + ol, x86_abs(v, off.wrapping_add((i + ol + 4) as u64) as u32, false));
        i += ol + 4;
    }
}

// ---- Feld-Split --------------------------------------------------------
fn split_layout(f: &FSpec, n: usize, off: u64) -> (usize, usize, usize) {
    let r: usize = f.fields[..(f.nfields as usize).min(6)].iter().map(|&w| w as usize).sum();
    if r == 0 {
        return (n, 0, 1);
    }
    let skip = f.skip as u64;
    let h = if off < skip { (skip - off) as usize } else { (r - ((off - skip) % r as u64) as usize) % r };
    let h = h.min(n);
    (h, (n - h) / r, r)
}
fn split_enc(f: &FSpec, inp: &[u8], out: &mut [u8], off: u64) {
    let n = inp.len();
    let (head, nrec, r) = split_layout(f, n, off);
    out[..head].copy_from_slice(&inp[..head]);
    let mut o = head;
    let mut fo = 0;
    for &w in &f.fields[..(f.nfields as usize).min(6)] {
        let w = w as usize;
        for j in 0..nrec {
            let s = head + j * r + fo;
            out[o..o + w].copy_from_slice(&inp[s..s + w]);
            o += w;
        }
        fo += w;
    }
    let used = head + nrec * r;
    out[o..o + (n - used)].copy_from_slice(&inp[used..]);
}
fn split_dec(f: &FSpec, inp: &[u8], out: &mut [u8], off: u64) {
    let n = inp.len();
    let (head, nrec, r) = split_layout(f, n, off);
    out[..head].copy_from_slice(&inp[..head]);
    let mut q = head;
    let mut fo = 0;
    for &w in &f.fields[..(f.nfields as usize).min(6)] {
        let w = w as usize;
        for j in 0..nrec {
            let d = head + j * r + fo;
            out[d..d + w].copy_from_slice(&inp[q..q + w]);
            q += w;
        }
        fo += w;
    }
    let used = head + nrec * r;
    out[used..].copy_from_slice(&inp[q..q + (n - used)]);
}

// ---- Wort-Prädiktion ----------------------------------------------------
#[inline]
fn ldw(b: &[u8], i: usize, w: usize) -> u32 {
    if w == 2 {
        u16::from_le_bytes([b[i], b[i + 1]]) as u32
    } else {
        rd32(b, i)
    }
}
#[inline]
fn stw(b: &mut [u8], i: usize, v: u32, w: usize) {
    if w == 2 {
        b[i..i + 2].copy_from_slice(&(v as u16).to_le_bytes());
    } else {
        wr32(b, i, v);
    }
}
fn wpred_core(f: &FSpec, buf: &mut [u8], enc: bool) {
    let w = f.fields[0] as usize;
    if w != 2 && w != 4 {
        return;
    }
    let ord = f.fields[1];
    let nw = buf.len() / w;
    let k = f.stride as usize / w;
    if k == 0 {
        return;
    }
    let pred = |b: &[u8], j: usize| -> u32 {
        if j < k {
            return 0;
        }
        let a = ldw(b, (j - k) * w, w);
        if ord == 2 && j >= 2 * k {
            a.wrapping_mul(2).wrapping_sub(ldw(b, (j - 2 * k) * w, w))
        } else {
            a
        }
    };
    if enc {
        for j in (0..nw).rev() {
            let x = ldw(buf, j * w, w);
            let p = pred(buf, j);
            stw(buf, j * w, x.wrapping_sub(p), w);
        }
    } else {
        for j in 0..nw {
            let r = ldw(buf, j * w, w);
            let p = pred(buf, j);
            stw(buf, j * w, r.wrapping_add(p), w);
        }
    }
}

/// Filter anwenden. `out.len()` muss `inp.len()` sein. `off` = Position in der Datei.
pub fn encode(f: &FSpec, inp: &[u8], out: &mut [u8], off: u64) {
    assert_eq!(inp.len(), out.len());
    let s = f.stride as usize;
    match f.id {
        FT_DELTA if s > 0 => {
            for i in 0..inp.len() {
                out[i] = if i >= s { inp[i].wrapping_sub(inp[i - s]) } else { inp[i] };
            }
        }
        FT_SHUF => shuffle(inp, out, s),
        FT_SHUFDELTA => {
            shuffle(inp, out, s);
            for i in (1..out.len()).rev() {
                out[i] = out[i].wrapping_sub(out[i - 1]);
            }
        }
        FT_BCJ => {
            out.copy_from_slice(inp);
            bcj(out, off, true);
        }
        FT_SPLIT => split_enc(f, inp, out, off),
        FT_X86S => x86s_enc(inp, out, off),
        FT_WPRED => {
            let mut t = inp.to_vec();
            wpred_core(f, &mut t, true);
            shuffle(&t, out, if f.fields[2] != 0 { s } else { f.fields[0] as usize });
        }
        _ => out.copy_from_slice(inp),
    }
}

/// Filter umkehren. `inp` darf verändert werden.
pub fn decode(f: &FSpec, inp: &mut [u8], out: &mut [u8], off: u64) {
    assert_eq!(inp.len(), out.len());
    let s = f.stride as usize;
    match f.id {
        FT_DELTA if s > 0 => {
            for i in 0..inp.len() {
                out[i] = if i >= s { inp[i].wrapping_add(out[i - s]) } else { inp[i] };
            }
        }
        FT_SHUF => unshuffle(inp, out, s),
        FT_SHUFDELTA => {
            for i in 1..inp.len() {
                inp[i] = inp[i].wrapping_add(inp[i - 1]);
            }
            unshuffle(inp, out, s);
        }
        FT_BCJ => {
            out.copy_from_slice(inp);
            bcj(out, off, false);
        }
        FT_SPLIT => split_dec(f, inp, out, off),
        FT_X86S => x86s_dec(inp, out, off),
        FT_WPRED => {
            unshuffle(inp, out, if f.fields[2] != 0 { s } else { f.fields[0] as usize });
            wpred_core(f, out, false);
        }
        _ => out.copy_from_slice(inp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    pub fn random_spec(r: &mut Rng) -> FSpec {
        let mut f = FSpec::new((r.next() % FT_COUNT as u32) as u8);
        f.stride = 1 + (r.next() % 32) as u8;
        if f.id == FT_WPRED {
            let w = if r.next() % 2 == 1 { 2 } else { 4 };
            f.fields[0] = w;
            f.fields[1] = 1 + (r.next() % 2) as u8;
            f.fields[2] = (r.next() % 2) as u8;
            f.stride = w * (1 + (r.next() % 16) as u8);
        }
        if f.id == FT_SPLIT {
            f.nfields = 1 + (r.next() % 6) as u8;
            for k in 0..f.nfields as usize {
                f.fields[k] = 1 + (r.next() % 8) as u8;
            }
            f.skip = r.next() % 300;
        }
        f
    }

    #[test]
    fn roundtrip_random() {
        let mut r = Rng::new(1);
        for _ in 0..3000 {
            let n = (r.next() % 20000) as usize;
            let off = if r.next() % 4 == 0 { 0 } else { r.next() as u64 * 7 };
            let data: Vec<u8> = (0..n)
                .map(|_| {
                    if r.next() % 3 == 0 {
                        0xE8 | (r.next() & 1) as u8
                    } else if r.next() % 2 == 1 {
                        0xFF
                    } else {
                        r.next() as u8
                    }
                })
                .collect();
            let f = random_spec(&mut r);
            let mut enc = vec![0; n];
            encode(&f, &data, &mut enc, off);
            let mut tmp = enc.clone();
            let mut dec = vec![0; n];
            decode(&f, &mut tmp, &mut dec, off);
            assert_eq!(dec, data, "Filter {:?}", f);
        }
    }

    #[test]
    fn decode_never_panics_on_garbage() {
        let mut r = Rng::new(2);
        for _ in 0..2000 {
            let n = (r.next() % 5000) as usize;
            let mut data: Vec<u8> = (0..n).map(|_| r.next() as u8).collect();
            let f = random_spec(&mut r);
            let mut out = vec![0; n];
            decode(&f, &mut data, &mut out, r.next() as u64);
        }
    }
}
