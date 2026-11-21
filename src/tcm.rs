//! TCM – context-mixing core for text (port of `legacy-c/gpk/tcm.c`, byte-identical).
//!
//! Bitwise prediction from orders 0–4 and 6, word, column and match models,
//! two mixers, two APM stages, binary arithmetic coder. Integers only.

const NHASH: usize = 7; // o1 o2 o3 o4 o6 word column
const HBITS: u32 = 22;
const LR: i32 = 2;
const MMBITS: u32 = 20;
const MINMATCH: u32 = 6;
const SM_LIMIT: u32 = 255;

const SQ: [i32; 33] = [
    1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901, 3975, 4022, 4050, 4068,
    4079, 4085, 4089, 4092, 4093, 4094,
];
/// squash(d) = 4096 / (1 + e^(-d/256)), in integers (support points + interpolation).
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
    /// `squash(d)` for d in -2048..=2048 (index d + 2048); outside, squash saturates.
    sq: [u16; 4097],
    /// `stretch(squash(d))`, same index: saves one dependent lookup per stage.
    stsq: [i16; 4097],
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
        let (mut sq, mut stsq) = ([0u16; 4097], [0i16; 4097]);
        for (i, (q, t)) in sq.iter_mut().zip(stsq.iter_mut()).enumerate() {
            let v = squash(i as i32 - 2048);
            *q = v as u16;
            *t = stretch[v as usize];
        }
        Tables { stretch, dt, sq, stsq }
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
/// Adaptive probability map with `N` contexts of 33 interpolation points each.
struct Apm<const N: usize> {
    t: Box<[[u16; 33]; N]>,
    cx: usize,
    k: usize,
}
impl<const N: usize> Apm<N> {
    /// Initial table; an existing buffer is reused.
    fn with_buffer(t: Option<Box<[[u16; 33]; N]>>) -> Self {
        let mut row = [0u16; 33];
        for (j, r) in row.iter_mut().enumerate() {
            *r = (squash((j as i32 - 16) * 128) * 16) as u16;
        }
        let t = match t {
            Some(mut t) => {
                t.fill(row);
                t
            }
            None => boxed_array(row),
        };
        Apm { t, cx: 0, k: 0 }
    }
    /// `s` = stretch(p) + 2048 of the input probability (1..=4095).
    #[inline(always)]
    fn p(&mut self, s: i32, cx: usize) -> i32 {
        let (lo, w) = ((s >> 7) as usize & 31, s & 127);
        let r = &self.t[cx % N];
        self.cx = cx % N;
        self.k = lo + (w >> 6) as usize;
        (r[lo] as i32 * (128 - w) + r[lo + 1] as i32 * w) >> 11
    }
    #[inline(always)]
    fn update(&mut self, bit: i32) {
        let g = (bit << 16) + (bit << 5) - bit - bit;
        let t = &mut self.t[self.cx % N][self.k.min(32)];
        *t = t.wrapping_add(((g - *t as i32) >> 6) as u16);
    }
}

/// Heap array of `N` copies of `v` (without a large temporary on the stack).
fn boxed_array<T: Clone, const N: usize>(v: T) -> Box<[T; N]> {
    match vec![v; N].into_boxed_slice().try_into() {
        Ok(b) => b,
        Err(_) => unreachable!("length is N"),
    }
}

#[inline]
fn hash2(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0xC2B2_AE3D);
    h ^ (h >> 13)
}

/// Mixer inputs, padded to a multiple of 4 lanes (the padding inputs are always 0).
/// Order: hashed contexts 0..NHASH, o0, match, bias. All initial weights are equal,
/// so the order of the inputs does not influence the result.
const NX: usize = 12;
/// Inputs that can be non-zero.
const NREAL: usize = NHASH + 3;
const X_MM: usize = NHASH + 1;
const X_BIAS: usize = NHASH + 2;
/// Rows of 16 state entries per hash table (entry 0 of a row = check value).
const ROWS: usize = 1 << (HBITS - 4);
/// Row tables: NHASH hashed contexts plus one table whose first 17 rows hold o0.
/// A power of two, so that masked row indices need no bounds check.
const NTAB: usize = NHASH + 1;
const HT_ROWS: usize = NTAB * ROWS;
const O0_BASE: usize = NHASH * ROWS;
const O0_ROWS: usize = 17;
const _: () = assert!(HT_ROWS.is_power_of_two());

type Row = [u32; 16];
type Weights = [[i32; NX]; 256];

#[inline(always)]
fn dot16(x: &[i32; NX], w: &[i32; NX]) -> i32 {
    let p = |i: usize| x[i] as i64 * w[i] as i64;
    // Balanced sum over the real inputs: shorter dependency chain than a serial accumulation
    let a = (p(0) + p(1)) + (p(2) + p(3));
    let b = (p(4) + p(5)) + (p(6) + p(7));
    const _: () = assert!(NREAL == 10);
    ((a + b + (p(8) + p(9))) >> 16) as i32
}

#[inline(always)]
fn train(x: &[i32; NX], w: &mut [i32; NX], err: i32) {
    for i in 0..NX {
        w[i] = w[i].wrapping_add(x[i].wrapping_mul(err) >> 10);
    }
}

struct Model {
    tab: &'static Tables,
    /// All row tables (`HT_ROWS` rows of 16 states).
    ht: Box<[Row; HT_ROWS]>,
    /// Row index per input of the current nibble (NHASH hashed contexts, then o0).
    row: [usize; NTAB],
    hctx: [u32; NHASH],
    mm_sm: [u32; 64],
    mmt: Vec<u32>,
    mptr: u32,
    mlen: u32,
    w: Box<Weights>,
    w2: Box<Weights>,
    pm1: i32,
    pm2: i32,
    col: u32,
    linestart: u32,
    above: u32,
    x: [i32; NX],
    pmix: i32,
    a1: Apm<256>,
    a2: Apm<65536>,
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

/// Large buffers of a model (~125 MB), reused per thread:
/// saves the allocation and the page faults for every block.
struct Buffers {
    ht: Box<[Row; HT_ROWS]>,
    mmt: Vec<u32>,
    a2: Option<Box<[[u16; 33]; 65536]>>,
}
impl Buffers {
    fn new() -> Self {
        // The rows of the last table after o0 are never touched (no page faults)
        let mut ht: Box<[Row; HT_ROWS]> = boxed_array([0u32; 16]);
        ht[O0_BASE..O0_BASE + O0_ROWS].fill([1 << 31; 16]);
        Buffers { ht, mmt: vec![0u32; 1 << MMBITS], a2: None }
    }
    /// Reset used buffers to the initial state.
    fn cleared(mut self) -> Self {
        self.ht[..O0_BASE].fill([0u32; 16]);
        self.ht[O0_BASE..O0_BASE + O0_ROWS].fill([1 << 31; 16]);
        self.mmt.fill(0);
        self
    }
}

thread_local! {
    static CACHE: std::cell::RefCell<Option<Buffers>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` with a fresh model; the large buffers come from the thread cache.
fn with_model<R>(f: impl FnOnce(&mut Model) -> R) -> R {
    let bufs = CACHE.with(|c| c.borrow_mut().take()).map(Buffers::cleared).unwrap_or_else(Buffers::new);
    let mut m = Model::new(bufs);
    let r = f(&mut m);
    let Model { ht, mmt, a2, .. } = *m;
    CACHE.with(|c| *c.borrow_mut() = Some(Buffers { ht, mmt, a2: Some(a2.t) }));
    r
}

impl Model {
    fn new(b: Buffers) -> Box<Self> {
        let mut x = [0i32; NX];
        x[X_BIAS] = 256;
        let mut m = Box::new(Model {
            tab: tables(),
            ht: b.ht,
            row: [0; NTAB],
            hctx: [0; NHASH],
            mm_sm: [1 << 31; 64],
            mmt: b.mmt,
            mptr: 0,
            mlen: 0,
            w: boxed_array([(1 << 16) / 4; NX]),
            w2: boxed_array([(1 << 16) / 4; NX]),
            pm1: 0,
            pm2: 0,
            col: 0,
            linestart: 0,
            above: 0,
            x,
            pmix: 0,
            a1: Apm::with_buffer(None),
            a2: Apm::with_buffer(b.a2),
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
        m.select_rows();
        m
    }

    /// Choose the rows (16 entries, entry 0 = check value) for the current nibble;
    /// two candidates per context, replace the one with less experience if needed.
    fn select_rows(&mut self) {
        let nib = if self.bitpos == 0 { 0 } else { (self.c0 & 15) | 16 };
        for k in 0..NHASH {
            let h = hash2(self.hctx[k], nib + k as u32 * 977);
            let tag = (h & 0xFFFF) | 1;
            let a = (k * ROWS + ((h >> (36 - HBITS)) as usize & (ROWS - 2))) & (HT_ROWS - 2);
            let (ra, rb) = (&self.ht[a], &self.ht[a + 1]);
            let r = if ra[0] == tag {
                a
            } else if rb[0] == tag {
                a + 1
            } else {
                let r = if (ra[1] & 1023) <= (rb[1] & 1023) { a } else { a + 1 };
                let mut fresh = [1u32 << 31; 16];
                fresh[0] = tag;
                self.ht[r] = fresh;
                r
            };
            self.row[k] = r;
        }
        self.row[NHASH] = O0_BASE + if nib == 0 { 0 } else { ((nib & 15) + 1) as usize };
    }

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
        // Column + character in the line above
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
        // Match model
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

    #[inline(always)]
    fn predict(&mut self, buf: &[u8]) -> u32 {
        let tab = self.tab;
        let st = &tab.stretch;
        let j = self.jn & 15;
        for k in 0..NTAB {
            self.x[k] = st[sm_p(self.ht[self.row[k] & (HT_ROWS - 1)][j])] as i32;
        }
        self.mm_expect = false;
        self.x[X_MM] = 0;
        if self.mlen != 0 {
            let pb = buf[self.mptr as usize] as u32;
            let expect = (pb >> (7 - self.bitpos)) & 1;
            if ((pb | 256) >> (8 - self.bitpos)) == self.c0 {
                let lb = self.mlen.min(31);
                self.mm_idx = ((lb * 2 + expect) & 63) as usize;
                self.mm_expect = true;
                self.x[X_MM] = st[sm_p(self.mm_sm[self.mm_idx])] as i32;
            }
        }
        let c0 = self.c0 as usize & 255;
        let c4b = (self.c4 & 0xFF) as usize;
        let sq_idx = |d: i32| (d.clamp(-2048, 2048) + 2048) as usize;
        let i1 = sq_idx(dot16(&self.x, &self.w[c0]));
        let i2 = sq_idx(dot16(&self.x, &self.w2[c4b]));
        self.pm1 = tab.sq[i1] as i32;
        self.pm2 = tab.sq[i2] as i32;
        // pmix = squash((stretch(pm1) + stretch(pm2)) / 2), the sum is within -2047..=2047
        let im = (((tab.stsq[i1] as i32 + tab.stsq[i2] as i32) >> 1) + 2048) as usize & 4095;
        self.pmix = tab.sq[im] as i32;
        let s = tab.stsq[im] as i32 + 2048;
        let p1 = self.a1.p(s, c0);
        let p2 = self.a2.p(s, c0 | (c4b << 8));
        ((self.pmix + p1 + 2 * p2 + 2) >> 2).clamp(1, 4095) as u32
    }

    #[inline(always)]
    fn update(&mut self, bit: i32, buf: &[u8]) {
        let dt = &self.tab.dt;
        let j = self.jn & 15;
        for k in 0..NTAB {
            sm_update(&mut self.ht[self.row[k] & (HT_ROWS - 1)][j], bit, dt);
        }
        if self.mm_expect {
            sm_update(&mut self.mm_sm[self.mm_idx], bit, dt);
        }
        let err = ((bit << 12) - self.pm1) * LR;
        let err2 = ((bit << 12) - self.pm2) * LR;
        let c4b = (self.c4 & 0xFF) as usize;
        train(&self.x, &mut self.w[self.c0 as usize & 255], err);
        train(&self.x, &mut self.w2[c4b], err2);
        self.a1.update(bit);
        self.a2.update(bit);
        self.c0 = (self.c0 << 1) | bit as u32;
        self.jn = (self.jn << 1) | bit as usize;
        self.bitpos += 1;
        if self.bitpos == 4 {
            self.jn = 1;
            self.select_rows();
        } else if self.bitpos == 8 {
            let c = self.c0 as u8;
            self.pos += 1;
            self.byte_update(c, buf);
            self.c0 = 1;
            self.bitpos = 0;
            self.jn = 1;
            self.select_rows();
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

/// Decodes exactly `out.len()` bytes. Corrupted data yields wrong bytes
/// (the CRC check detects that), but never a memory error.
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
                out[i] = c as u8; // the byte is stored before the model reads it
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
        let text = "The dragon guards the sword.\n  {\"key\": 42, \"name\": \"castle\"}\n".repeat(400);
        for d in [Vec::new(), b"a".to_vec(), text.into_bytes(), Rng::new(1).bytes(30_000)] {
            let c = compress(&d);
            let mut out = vec![0u8; d.len()];
            decompress(&c, &mut out);
            assert_eq!(out, d);
        }
    }
    #[test]
    fn reuse_is_deterministic() {
        // Same input after other blocks in the same thread: identical output
        let a = "First block with text. ".repeat(3000).into_bytes();
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
