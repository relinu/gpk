//! TCM – Context-Mixing-Kern für Text (Port von `legacy-c/gpk/tcm.c`, bytegleich).
//!
//! Bitweise Vorhersage aus Ordnung 0–4 und 6, Wort-, Spalten- und Match-Modell,
//! zwei Mischnetze, zwei APM-Stufen, binärer Arithmetik-Coder. Nur Ganzzahlen.

const NHASH: usize = 7; // o1 o2 o3 o4 o6 Wort Spalte
const NIN: usize = NHASH + 3; // + o0, Match, Bias
const HBITS: u32 = 22;
const LR: i32 = 2;
const MMBITS: u32 = 20;
const MINMATCH: u32 = 6;
const SM_LIMIT: u32 = 255;

const SQ: [i32; 33] = [
    1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901, 3975, 4022, 4050, 4068,
    4079, 4085, 4089, 4092, 4093, 4094,
];
/// squash(d) = 4096 / (1 + e^(-d/256)), ganzzahlig (Stützstellen + Interpolation).
#[inline]
fn squash(d: i32) -> i32 {
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    ((SQ[i] * (128 - w) + SQ[i + 1] * w + 64) >> 7).clamp(1, 4095)
}

struct Tables {
    stretch: [i16; 4096],
    dt: [i32; 1024],
}
fn tables() -> &'static Tables {
    static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut stretch = [0i16; 4096];
        let mut pi = 0usize;
        for x in -2047..=2047 {
            let v = squash(x) as usize;
            for s in stretch.iter_mut().take(v + 1).skip(pi) {
                *s = x as i16;
            }
            pi = v + 1;
        }
        for s in stretch.iter_mut().skip(pi) {
            *s = 2047;
        }
        let mut dt = [0i32; 1024];
        for (i, d) in dt.iter_mut().enumerate() {
            *d = (65536 * 2) / (2 * i as i32 + 3);
        }
        Tables { stretch, dt }
    })
}

#[inline]
fn sm_p(e: u32) -> usize {
    (e >> 20) as usize
}
#[inline]
fn sm_update(e: &mut u32, bit: i32, dt: &[i32; 1024]) {
    let v = *e;
    let mut n = v & 1023;
    let mut p = (v >> 10) as i32;
    let target = if bit != 0 { (1 << 22) - 1 } else { 0 };
    p += (((target - p) as i64 * dt[n as usize] as i64) >> 16) as i32;
    if n < SM_LIMIT {
        n += 1;
    }
    *e = ((p as u32) << 10) | n;
}

struct Apm {
    t: Vec<u16>,
    idx: usize,
}
impl Apm {
    /// Tabelle für `n` Kontexte; ein vorhandener Puffer wird wiederverwendet.
    fn with_buffer(mut t: Vec<u16>, n: usize) -> Self {
        let mut row = [0u16; 33];
        for (j, r) in row.iter_mut().enumerate() {
            *r = (squash((j as i32 - 16) * 128) * 16) as u16;
        }
        t.clear();
        t.reserve_exact(n * 33);
        for _ in 0..n {
            t.extend_from_slice(&row);
        }
        Apm { t, idx: 0 }
    }
    #[inline]
    fn p(&mut self, stretch: &[i16; 4096], pr: i32, cx: usize) -> i32 {
        let s = stretch[pr as usize] as i32 + 2048;
        let (lo, w) = ((s >> 7) as usize, s & 127);
        self.idx = cx * 33 + lo + (w >> 6) as usize;
        (self.t[cx * 33 + lo] as i32 * (128 - w) + self.t[cx * 33 + lo + 1] as i32 * w) >> 11
    }
    #[inline]
    fn update(&mut self, bit: i32) {
        let g = (bit << 16) + (bit << 5) - bit - bit;
        let t = &mut self.t[self.idx];
        *t = t.wrapping_add(((g - *t as i32) >> 6) as u16);
    }
}

#[inline]
fn hash2(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0xC2B2_AE3D);
    h ^ (h >> 13)
}

struct Model {
    tab: &'static Tables,
    ht: Vec<Vec<u32>>,
    slot: [usize; NHASH],
    hctx: [u32; NHASH],
    o0: Vec<u32>,
    o0slot: usize,
    mm_sm: [u32; 128],
    mmt: Vec<u32>,
    mptr: u32,
    mlen: u32,
    w: Vec<[i32; NIN]>,
    w2: Vec<[i32; NIN]>,
    pm1: i32,
    pm2: i32,
    col: u32,
    linestart: u32,
    above: u32,
    x: [i32; NIN],
    pmix: i32,
    a1: Apm,
    a2: Apm,
    c0: u32,
    c4: u32,
    word0: u32,
    word1: u32,
    bitpos: u32,
    jn: usize,
    mm_expect: bool,
    mm_idx: usize,
    pos: u32,
}

/// Große Puffer eines Modells (~125 MB), werden je Thread wiederverwendet:
/// spart das Anfordern und die Seitenfehler bei jedem Block.
struct Buffers {
    ht: Vec<Vec<u32>>,
    mmt: Vec<u32>,
    a2: Vec<u16>,
}
impl Buffers {
    fn new() -> Self {
        Buffers { ht: (0..NHASH).map(|_| vec![0u32; 1 << HBITS]).collect(), mmt: vec![0u32; 1 << MMBITS], a2: Vec::new() }
    }
    /// Gebrauchte Puffer auf den Anfangszustand zurücksetzen.
    fn cleared(mut self) -> Self {
        for t in &mut self.ht {
            t.fill(0);
        }
        self.mmt.fill(0);
        self
    }
}

thread_local! {
    static CACHE: std::cell::RefCell<Option<Buffers>> = const { std::cell::RefCell::new(None) };
}

/// Führt `f` mit einem frischen Modell aus; die großen Puffer kommen aus dem Thread-Cache.
fn with_model<R>(f: impl FnOnce(&mut Model) -> R) -> R {
    let bufs = CACHE.with(|c| c.borrow_mut().take()).map(Buffers::cleared).unwrap_or_else(Buffers::new);
    let mut m = Model::new(bufs);
    let r = f(&mut m);
    let Model { ht, mmt, a2, .. } = *m;
    CACHE.with(|c| *c.borrow_mut() = Some(Buffers { ht, mmt, a2: a2.t }));
    r
}

impl Model {
    fn new(b: Buffers) -> Box<Self> {
        let mut m = Box::new(Model {
            tab: tables(),
            ht: b.ht,
            slot: [0; NHASH],
            hctx: [0; NHASH],
            o0: vec![1 << 31; 256 * 16],
            o0slot: 0,
            mm_sm: [1 << 31; 128],
            mmt: b.mmt,
            mptr: 0,
            mlen: 0,
            w: vec![[(1 << 16) / 4; NIN]; 256],
            w2: vec![[(1 << 16) / 4; NIN]; 256],
            pm1: 0,
            pm2: 0,
            col: 0,
            linestart: 0,
            above: 0,
            x: [0; NIN],
            pmix: 0,
            a1: Apm::with_buffer(Vec::new(), 256),
            a2: Apm::with_buffer(b.a2, 65536),
            c0: 1,
            c4: 0,
            word0: 0,
            word1: 0,
            bitpos: 0,
            jn: 1,
            mm_expect: false,
            mm_idx: 0,
            pos: 0,
        });
        m.select_slots();
        m
    }

    /// Slots (16 Einträge, Eintrag 0 = Prüfsumme) für das aktuelle Nibble wählen;
    /// zwei Kandidaten je Kontext, ggf. den mit weniger Erfahrung ersetzen.
    fn select_slots(&mut self) {
        let nib = if self.bitpos == 0 { 0 } else { (self.c0 & 15) | 16 };
        for k in 0..NHASH {
            let h = hash2(self.hctx[k], nib + k as u32 * 977);
            let tag = (h & 0xFFFF) | 1;
            let a = ((h >> (32 - HBITS)) & !31) as usize;
            let t = &mut self.ht[k];
            let s = if t[a] == tag {
                a
            } else if t[a + 16] == tag {
                a + 16
            } else {
                let s = if (t[a + 1] & 1023) <= (t[a + 17] & 1023) { a } else { a + 16 };
                t[s] = tag;
                t[s + 1..s + 16].fill(1 << 31);
                s
            };
            self.slot[k] = s;
        }
        self.o0slot = if nib == 0 { 0 } else { ((nib & 15) + 1) as usize * 16 };
    }

    /// An der Bytegrenze: Kontexte für das nächste Byte (buf = alle bisherigen Bytes).
    fn byte_update(&mut self, c: u8, buf: &[u8]) {
        let cc = c as u32;
        self.c4 = (self.c4 << 8) | cc;
        let letter = c.is_ascii_alphanumeric() || c >= 128 || c == b'_';
        if letter {
            self.word0 = hash2(self.word0, cc | 32);
        } else if self.word0 != 0 {
            self.word1 = self.word0;
            self.word0 = 0;
        }
        let c4 = self.c4;
        let pos = self.pos as usize;
        self.hctx[0] = hash2(1, c4 & 0xFF);
        self.hctx[1] = hash2(2, c4 & 0xFFFF);
        self.hctx[2] = hash2(3, c4 & 0xFF_FFFF);
        self.hctx[3] = hash2(4, c4);
        self.hctx[4] = hash2(hash2(6, c4), if pos >= 6 { buf[pos - 5] as u32 | (buf[pos - 6] as u32) << 8 } else { 0 });
        self.hctx[5] = hash2(hash2(7, self.word0), self.word1.wrapping_add(if letter { 0 } else { cc }));
        // Spalte + Zeichen in der Zeile darüber
        if c == b'\n' {
            self.above = self.linestart;
            self.linestart = self.pos;
            self.col = 0;
        } else {
            self.col += 1;
        }
        let prevline = self.above;
        let ab = if prevline.wrapping_add(self.col) < self.linestart { buf[(prevline + self.col) as usize] as u32 } else { 0 };
        self.hctx[6] = hash2(hash2(8, self.col.min(64)), ab | (cc << 8));
        // Match-Modell
        if self.mlen != 0 && buf[self.mptr as usize] == c {
            self.mlen += 1;
            self.mptr += 1;
        } else {
            self.mlen = 0;
        }
        if self.pos >= MINMATCH {
            let mut h = 0u32;
            for i in 1..=MINMATCH as usize {
                h = h.wrapping_mul(0x2F0B_4A23).wrapping_add(buf[pos - i] as u32);
            }
            let h = ((h ^ (h >> 15)) >> (32 - MMBITS)) as usize;
            if self.mlen == 0 {
                let cand = self.mmt[h];
                if cand != 0 {
                    let mut l = 0u32;
                    while l < 32 && l < cand && buf[(cand - 1 - l) as usize] == buf[pos - 1 - l as usize] {
                        l += 1;
                    }
                    if l >= MINMATCH {
                        self.mlen = l;
                        self.mptr = cand;
                    }
                }
            }
            self.mmt[h] = self.pos;
        }
    }

    fn predict(&mut self, buf: &[u8]) -> u32 {
        let st = &self.tab.stretch;
        let j = self.jn;
        for k in 0..NHASH {
            self.x[k + 1] = st[sm_p(self.ht[k][self.slot[k] + j])] as i32;
        }
        self.x[0] = st[sm_p(self.o0[self.o0slot + j])] as i32;
        self.mm_expect = false;
        self.x[NHASH + 1] = 0;
        if self.mlen != 0 {
            let pb = buf[self.mptr as usize] as u32;
            let expect = (pb >> (7 - self.bitpos)) & 1;
            if ((pb | 256) >> (8 - self.bitpos)) == self.c0 {
                let lb = self.mlen.min(31);
                self.mm_idx = (lb * 2 + expect) as usize;
                self.mm_expect = true;
                self.x[NHASH + 1] = st[sm_p(self.mm_sm[self.mm_idx])] as i32;
            }
        }
        self.x[NIN - 1] = 256;
        let (w, w2) = (&self.w[self.c0 as usize], &self.w2[(self.c4 & 0xFF) as usize]);
        let (mut dot, mut dot2) = (0i64, 0i64);
        for i in 0..NIN {
            dot += self.x[i] as i64 * w[i] as i64;
            dot2 += self.x[i] as i64 * w2[i] as i64;
        }
        self.pm1 = squash((dot >> 16) as i32);
        self.pm2 = squash((dot2 >> 16) as i32);
        self.pmix = squash((st[self.pm1 as usize] as i32 + st[self.pm2 as usize] as i32) >> 1);
        let p1 = self.a1.p(st, self.pmix, self.c0 as usize);
        let p2 = self.a2.p(st, self.pmix, (self.c0 | ((self.c4 & 0xFF) << 8)) as usize);
        ((self.pmix + p1 + 2 * p2 + 2) >> 2).clamp(1, 4095) as u32
    }

    fn update(&mut self, bit: i32, buf: &[u8]) {
        let dt = &self.tab.dt;
        let j = self.jn;
        for k in 0..NHASH {
            let s = self.slot[k] + j;
            sm_update(&mut self.ht[k][s], bit, dt);
        }
        sm_update(&mut self.o0[self.o0slot + j], bit, dt);
        if self.mm_expect {
            sm_update(&mut self.mm_sm[self.mm_idx], bit, dt);
        }
        let err = ((bit << 12) - self.pm1) * LR;
        let err2 = ((bit << 12) - self.pm2) * LR;
        let c4b = (self.c4 & 0xFF) as usize;
        let (w, w2) = (&mut self.w[self.c0 as usize], &mut self.w2[c4b]);
        for i in 0..NIN {
            w[i] = w[i].wrapping_add((self.x[i] * err) >> 10);
            w2[i] = w2[i].wrapping_add((self.x[i] * err2) >> 10);
        }
        self.a1.update(bit);
        self.a2.update(bit);
        self.c0 = (self.c0 << 1) | bit as u32;
        self.jn = (self.jn << 1) | bit as usize;
        self.bitpos += 1;
        if self.bitpos == 4 {
            self.jn = 1;
            self.select_slots();
        } else if self.bitpos == 8 {
            let c = self.c0 as u8;
            self.pos += 1;
            self.byte_update(c, buf);
            self.c0 = 1;
            self.bitpos = 0;
            self.jn = 1;
            self.select_slots();
        }
    }
}

pub fn bound(n: usize) -> usize {
    n + n / 32 + 64
}

pub fn compress(src: &[u8]) -> Vec<u8> {
    with_model(|m| compress_with(m, src))
}

fn compress_with(m: &mut Model, src: &[u8]) -> Vec<u8> {
    let (mut x1, mut x2) = (0u32, 0xFFFF_FFFFu32);
    let mut out = Vec::with_capacity(src.len() / 4 + 64);
    for (i, &c) in src.iter().enumerate() {
        for b in (0..8).rev() {
            let bit = ((c >> b) & 1) as i32;
            let p = m.predict(&src[..i]);
            let xmid = x1 + (((x2 - x1) as u64 * p as u64) >> 12) as u32;
            if bit != 0 {
                x2 = xmid;
            } else {
                x1 = xmid + 1;
            }
            m.update(bit, &src[..i + 1]);
            while (x1 ^ x2) & 0xFF00_0000 == 0 {
                out.push((x2 >> 24) as u8);
                x1 <<= 8;
                x2 = (x2 << 8) | 255;
            }
        }
    }
    for _ in 0..4 {
        out.push((x1 >> 24) as u8);
        x1 <<= 8;
    }
    out
}

/// Dekodiert genau `out.len()` Bytes. Beschädigte Daten liefern falsche Bytes
/// (die CRC-Prüfung erkennt das), aber nie einen Speicherfehler.
pub fn decompress(src: &[u8], out: &mut [u8]) {
    with_model(|m| decompress_with(m, src, out))
}

fn decompress_with(m: &mut Model, src: &[u8], out: &mut [u8]) {
    let (mut x1, mut x2, mut x) = (0u32, 0xFFFF_FFFFu32, 0u32);
    let mut ip = 0usize;
    let next = |ip: &mut usize| -> u32 {
        let b = src.get(*ip).copied().unwrap_or(0);
        *ip += 1;
        b as u32
    };
    for _ in 0..4 {
        x = (x << 8) | next(&mut ip);
    }
    for i in 0..out.len() {
        let mut c = 0u32;
        for b in 0..8 {
            let p = m.predict(&out[..i]);
            let xmid = x1 + (((x2 - x1) as u64 * p as u64) >> 12) as u32;
            let bit = (x <= xmid) as i32;
            if bit != 0 {
                x2 = xmid;
            } else {
                x1 = xmid + 1;
            }
            c = (c << 1) | bit as u32;
            if b == 7 {
                out[i] = c as u8; // Byte steht, bevor das Modell es liest
            }
            m.update(bit, &out[..i + (b == 7) as usize]);
            while (x1 ^ x2) & 0xFF00_0000 == 0 {
                x1 <<= 8;
                x2 = (x2 << 8) | 255;
                x = (x << 8) | next(&mut ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    #[test]
    fn roundtrip() {
        let text = "Der Drache bewacht das Schwert.\n  {\"key\": 42, \"name\": \"Burg\"}\n".repeat(400);
        for d in [Vec::new(), b"a".to_vec(), text.into_bytes(), Rng::new(1).bytes(30_000)] {
            let c = compress(&d);
            let mut out = vec![0u8; d.len()];
            decompress(&c, &mut out);
            assert_eq!(out, d);
        }
    }
    #[test]
    fn wiederverwendung_deterministisch() {
        // Gleiche Eingabe nach anderen Blöcken im selben Thread: identische Ausgabe
        let a = "Erster Block mit Text. ".repeat(3000).into_bytes();
        let b = Rng::new(9).bytes(20_000);
        let first = compress(&a);
        let _ = compress(&b);
        let mut out = vec![0u8; b.len()];
        decompress(&compress(&b), &mut out);
        assert_eq!(out, b);
        assert_eq!(compress(&a), first);
    }
    #[test]
    fn corrupt_never_panics() {
        let mut r = Rng::new(2);
        let d = "abc def ghi\n".repeat(500).into_bytes();
        let c = compress(&d);
        let mut out = vec![0u8; d.len()];
        for _ in 0..30 {
            let mut m = c.clone();
            let i = r.next() as usize % m.len();
            m[i] ^= 0x5A;
            decompress(&m[..m.len() - (r.next() % 3) as usize], &mut out);
        }
    }
}
