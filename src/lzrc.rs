//! LZRC – main core: LZ77 with a binary-tree match finder, price-based optimal
//! parsing and an adaptive binary range coder (LZMA principle).
//! Port of `legacy-c/gpk/lzrc.c`, same format and byte-identical output.

const NSTATES: usize = 12;
const MINLEN: u32 = 2;
const MAXLEN: u32 = 273;
const LIT_SIZE: usize = 0x300;
const END_POS_IDX: u32 = 14;
const FULL_DIST: u32 = 128;
const OPT_NUM: u32 = 2048;
const DENSE_LEN: u32 = 48;
const H4BITS: u32 = 20;
const H3BITS: u32 = 16;
const NIL: u32 = u32::MAX;
const INF: u32 = u32::MAX;
const PROB_INIT: u16 = 1024;

const NEXT_LIT: [u8; NSTATES] = [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5];
#[inline]
fn next_match(s: u32) -> u32 {
    if s < 7 {
        7
    } else {
        10
    }
}
#[inline]
fn next_rep(s: u32) -> u32 {
    if s < 7 {
        8
    } else {
        11
    }
}
#[inline]
fn next_shortrep(s: u32) -> u32 {
    if s < 7 {
        9
    } else {
        11
    }
}

/// Parameters: literal context bits, literal position bits, position bits, search depth, "nice" length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub lc: u32,
    pub lp: u32,
    pub pb: u32,
    pub depth: u32,
    pub nice: u32,
}

impl Params {
    pub fn level(level: i32) -> Self {
        let (depth, nice) = match level {
            ..=3 => (8, 32),
            4..=6 => (32, 64),
            7..=8 => (64, 128),
            _ => (128, 192),
        };
        Params { lc: 3, lp: 0, pb: 2, depth, nice }
    }
}

pub fn bound(n: usize) -> usize {
    n + n / 16 + 4096
}

// ------------------------------------------------------------------
// Model
// ------------------------------------------------------------------
#[derive(Clone)]
struct LenModel {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; 16],
    mid: [[u16; 8]; 16],
    high: [u16; 256],
}
impl LenModel {
    fn new() -> Self {
        LenModel { choice: PROB_INIT, choice2: PROB_INIT, low: [[PROB_INIT; 8]; 16], mid: [[PROB_INIT; 8]; 16], high: [PROB_INIT; 256] }
    }
}
struct Model {
    is_match: [[u16; 16]; NSTATES],
    is_rep: [u16; NSTATES],
    g0: [u16; NSTATES],
    g1: [u16; NSTATES],
    g2: [u16; NSTATES],
    rep0_long: [[u16; 16]; NSTATES],
    pos_slot: [[u16; 64]; 4],
    pos_special: [u16; (FULL_DIST - END_POS_IDX) as usize],
    align: [u16; 16],
    len: LenModel,
    replen: LenModel,
    lit: Vec<u16>,
    lc: u32,
    lp: u32,
    pb: u32,
}
impl Model {
    fn new(lc: u32, lp: u32, pb: u32) -> Box<Self> {
        Box::new(Model {
            is_match: [[PROB_INIT; 16]; NSTATES],
            is_rep: [PROB_INIT; NSTATES],
            g0: [PROB_INIT; NSTATES],
            g1: [PROB_INIT; NSTATES],
            g2: [PROB_INIT; NSTATES],
            rep0_long: [[PROB_INIT; 16]; NSTATES],
            pos_slot: [[PROB_INIT; 64]; 4],
            pos_special: [PROB_INIT; (FULL_DIST - END_POS_IDX) as usize],
            align: [PROB_INIT; 16],
            len: LenModel::new(),
            replen: LenModel::new(),
            lit: vec![PROB_INIT; LIT_SIZE << (lc + lp)],
            lc,
            lp,
            pb,
        })
    }
    #[inline]
    fn lit_base(&self, pos: u32, prev: u8) -> usize {
        LIT_SIZE * ((((pos & ((1 << self.lp) - 1)) << self.lc) + ((prev as u32) >> (8 - self.lc))) as usize)
    }
}
#[inline]
fn pos_slot(d: u32) -> u32 {
    if d < 4 {
        return d;
    }
    let n = 31 - d.leading_zeros();
    (n << 1) | ((d >> (n - 1)) & 1)
}

// ------------------------------------------------------------------
// Range-Encoder
// ------------------------------------------------------------------
struct Rce {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}
impl Rce {
    fn new(out: Vec<u8>) -> Self {
        Rce { low: 0, range: 0xFFFF_FFFF, cache: 0, cache_size: 1, out }
    }
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = ((self.low as u32) << 8) as u64;
    }
    #[inline]
    fn bit(&mut self, p: &mut u16, bit: u32) {
        let bound = (self.range >> 11) * *p as u32;
        if bit == 0 {
            self.range = bound;
            *p += (2048 - *p) >> 5;
        } else {
            self.low += bound as u64;
            self.range -= bound;
            *p -= *p >> 5;
        }
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shift_low();
        }
    }
    fn direct(&mut self, v: u32, nb: u32) {
        for i in (0..nb).rev() {
            self.range >>= 1;
            if (v >> i) & 1 != 0 {
                self.low += self.range as u64;
            }
            while self.range < (1 << 24) {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }
    fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
    fn tree(&mut self, probs: &mut [u16], nb: u32, sym: u32) {
        let mut m = 1usize;
        for i in (0..nb).rev() {
            let b = (sym >> i) & 1;
            self.bit(&mut probs[m], b);
            m = (m << 1) | b as usize;
        }
    }
    /// Reverse tree; the counters for node m live at `probs[base + m - 1]`.
    fn rtree(&mut self, probs: &mut [u16], base: usize, nb: u32, mut sym: u32) {
        let mut m = 1usize;
        for _ in 0..nb {
            let b = sym & 1;
            sym >>= 1;
            self.bit(&mut probs[base + m - 1], b);
            m = (m << 1) | b as usize;
        }
    }
    fn len(&mut self, lm: &mut LenModel, len: u32, ps: usize) {
        let l = len - MINLEN;
        if l < 8 {
            self.bit(&mut lm.choice, 0);
            self.tree(&mut lm.low[ps], 3, l);
        } else if l < 16 {
            self.bit(&mut lm.choice, 1);
            self.bit(&mut lm.choice2, 0);
            self.tree(&mut lm.mid[ps], 3, l - 8);
        } else {
            self.bit(&mut lm.choice, 1);
            self.bit(&mut lm.choice2, 1);
            self.tree(&mut lm.high, 8, l - 16);
        }
    }
    fn dist(&mut self, m: &mut Model, d: u32, len: u32) {
        let ls = (len - MINLEN).min(3) as usize;
        let slot = pos_slot(d);
        self.tree(&mut m.pos_slot[ls], 6, slot);
        if slot >= 4 {
            let fb = (slot >> 1) - 1;
            let base = (2 | (slot & 1)) << fb;
            let red = d - base;
            if slot < END_POS_IDX {
                self.rtree(&mut m.pos_special, (base - slot) as usize, fb, red);
            } else {
                self.direct(red >> 4, fb - 4);
                self.rtree(&mut m.align, 1, 4, red & 15);
            }
        }
    }
    fn literal(&mut self, m: &mut Model, pos: u32, prev: u8, sym: u8, matched: bool, mb: u8) {
        let base = m.lit_base(pos, prev);
        let probs = &mut m.lit[base..base + LIT_SIZE];
        let mut mm = 1usize;
        let mut same = matched;
        for i in (0..8).rev() {
            let b = ((sym >> i) & 1) as u32;
            if same {
                let mbit = ((mb >> i) & 1) as usize;
                self.bit(&mut probs[0x100 + (mbit << 8) + mm], b);
                same = b as usize == mbit;
            } else {
                self.bit(&mut probs[mm], b);
            }
            mm = (mm << 1) | b as usize;
        }
    }
}

// ------------------------------------------------------------------
// Range-Decoder
// ------------------------------------------------------------------
struct Rcd<'a> {
    range: u32,
    code: u32,
    src: &'a [u8],
    p: usize,
}
impl<'a> Rcd<'a> {
    fn new(src: &'a [u8]) -> Self {
        let mut r = Rcd { range: 0xFFFF_FFFF, code: 0, src, p: 0 };
        for _ in 0..5 {
            r.code = (r.code << 8) | r.next() as u32;
        }
        r
    }
    #[inline]
    fn next(&mut self) -> u8 {
        if self.p < self.src.len() {
            self.p += 1;
            self.src[self.p - 1]
        } else {
            0
        }
    }
    #[inline]
    fn bit(&mut self, p: &mut u16) -> u32 {
        let bound = (self.range >> 11) * *p as u32;
        let b = if self.code < bound {
            self.range = bound;
            *p += (2048 - *p) >> 5;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            *p -= *p >> 5;
            1
        };
        if self.range < (1 << 24) {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next() as u32;
        }
        b
    }
    /// Same as `bit`, without data-dependent branches (for poorly predictable bits).
    #[inline(always)]
    fn bit_bl(&mut self, p: &mut u16) -> u32 {
        let pv = *p as u32;
        let bound = (self.range >> 11) * pv;
        let b = (self.code >= bound) as u32;
        let mask = b.wrapping_neg();
        self.code -= bound & mask;
        self.range = (bound & !mask) | ((self.range - bound) & mask);
        let p0 = pv + ((2048 - pv) >> 5);
        let p1 = pv - (pv >> 5);
        *p = ((p0 & !mask) | (p1 & mask)) as u16;
        if self.range < (1 << 24) {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next() as u32;
        }
        b
    }
    fn tree(&mut self, probs: &mut [u16], nb: u32) -> u32 {
        let mut m = 1usize;
        for _ in 0..nb {
            m = (m << 1) | self.bit_bl(&mut probs[m]) as usize;
        }
        (m - (1 << nb)) as u32
    }
    fn rtree(&mut self, probs: &mut [u16], base: usize, nb: u32) -> u32 {
        let (mut m, mut sym) = (1usize, 0u32);
        for i in 0..nb {
            let b = self.bit_bl(&mut probs[base + m - 1]);
            m = (m << 1) | b as usize;
            sym |= b << i;
        }
        sym
    }
    fn len(&mut self, lm: &mut LenModel, ps: usize) -> u32 {
        if self.bit(&mut lm.choice) == 0 {
            return MINLEN + self.tree(&mut lm.low[ps], 3);
        }
        if self.bit(&mut lm.choice2) == 0 {
            return MINLEN + 8 + self.tree(&mut lm.mid[ps], 3);
        }
        MINLEN + 16 + self.tree(&mut lm.high, 8)
    }
    fn dist(&mut self, m: &mut Model, len: u32) -> u32 {
        let ls = (len - MINLEN).min(3) as usize;
        let slot = self.tree(&mut m.pos_slot[ls], 6);
        if slot < 4 {
            return slot;
        }
        let fb = (slot >> 1) - 1;
        let d = (2 | (slot & 1)) << fb;
        if slot < END_POS_IDX {
            return d + self.rtree(&mut m.pos_special, (d - slot) as usize, fb);
        }
        let mut direct = 0u32;
        for _ in 0..fb - 4 {
            self.range >>= 1;
            let t = self.code.wrapping_sub(self.range) >> 31; // 1 if code < range
            self.code = self.code.wrapping_sub(self.range & t.wrapping_sub(1));
            direct = (direct << 1) | (1 - t);
            if self.range < (1 << 24) {
                self.range <<= 8;
                self.code = (self.code << 8) | self.next() as u32;
            }
        }
        d.wrapping_add(direct << 4).wrapping_add(self.rtree(&mut m.align, 1, 4))
    }
}

/// Decompresses exactly `out.len()` bytes. `None` on corrupted data.
pub fn decompress(src: &[u8], out: &mut [u8]) -> Option<()> {
    if src.len() < 2 {
        return None;
    }
    let (lc, lp, pb) = ((src[0] & 15) as u32, (src[0] >> 4) as u32, src[1] as u32);
    if lc > 8 || lp > 4 || pb > 4 {
        return None;
    }
    let mut m = Model::new(lc, lp, pb);
    let mut rc = Rcd::new(&src[2..]);
    let rawsize = out.len();
    let (mut state, mut r0, mut r1, mut r2, mut r3) = (0u32, 0usize, 0usize, 0usize, 0usize);
    let pbm = (1u32 << pb) - 1;
    let mut pos = 0usize;
    while pos < rawsize {
        let ps = (pos as u32 & pbm) as usize;
        if rc.bit(&mut m.is_match[state as usize][ps]) == 0 {
            let prev = if pos > 0 { out[pos - 1] } else { 0 };
            let base = m.lit_base(pos as u32, prev);
            let probs = &mut m.lit[base..base + LIT_SIZE];
            let mut mm = 1usize;
            if state >= 7 {
                if r0 >= pos {
                    return None;
                }
                // Matched literal: while the decoded bits equal those of the match byte,
                // the probabilities at 0x100 + (match bit << 8) are used, afterwards the plain ones.
                let mut mb = out[pos - r0 - 1] as usize;
                let mut offs = 0x100usize;
                for _ in 0..8 {
                    mb <<= 1;
                    let mbit = offs & mb;
                    let b = rc.bit_bl(&mut probs[offs + mbit + mm]) as usize;
                    mm = (mm << 1) | b;
                    // Keep offs only while b equals the match bit
                    offs &= ((b << 8) ^ mbit ^ 0x100) & 0x100;
                }
            } else {
                while mm < 0x100 {
                    mm = (mm << 1) | rc.bit_bl(&mut probs[mm & 0xFF]) as usize;
                }
            }
            out[pos] = mm as u8;
            pos += 1;
            state = NEXT_LIT[state as usize] as u32;
            continue;
        }
        let len;
        if rc.bit(&mut m.is_rep[state as usize]) == 0 {
            len = rc.len(&mut m.len, ps);
            let d = rc.dist(&mut m, len) as usize;
            r3 = r2;
            r2 = r1;
            r1 = r0;
            r0 = d;
            state = next_match(state);
        } else {
            if rc.bit(&mut m.g0[state as usize]) == 0 {
                if rc.bit(&mut m.rep0_long[state as usize][ps]) == 0 {
                    // Short rep: 1 byte
                    if r0 >= pos {
                        return None;
                    }
                    out[pos] = out[pos - r0 - 1];
                    pos += 1;
                    state = next_shortrep(state);
                    continue;
                }
            } else {
                let d;
                if rc.bit(&mut m.g1[state as usize]) == 0 {
                    d = r1;
                } else {
                    if rc.bit(&mut m.g2[state as usize]) == 0 {
                        d = r2;
                    } else {
                        d = r3;
                        r3 = r2;
                    }
                    r2 = r1;
                }
                r1 = r0;
                r0 = d;
            }
            len = rc.len(&mut m.replen, ps);
            state = next_rep(state);
        }
        let len = len as usize;
        if r0 >= pos || len > rawsize - pos {
            return None;
        }
        let s = pos - r0 - 1;
        if r0 + 1 >= len {
            out.copy_within(s..s + len, pos);
        } else {
            for k in 0..len {
                out[pos + k] = out[s + k];
            }
        }
        pos += len;
    }
    Some(())
}

// ------------------------------------------------------------------
// Prices (1/16 bit) for the optimal parser – encoder only
// ------------------------------------------------------------------
fn price_table() -> [u32; 128] {
    let mut t = [0u32; 128];
    for (i, v) in t.iter_mut().enumerate() {
        *v = (-((i * 16 + 8) as f64 / 2048.0).log2() * 16.0 + 0.5) as u32;
    }
    t
}

struct Prices {
    t: [u32; 128],
    len: Vec<[u32; (MAXLEN + 1) as usize]>,
    replen: Vec<[u32; (MAXLEN + 1) as usize]>,
    slot: [[u32; 64]; 4],
    full: [[u32; FULL_DIST as usize]; 4],
    align: [u32; 16],
}
impl Prices {
    #[inline]
    fn p0(&self, p: u16) -> u32 {
        self.t[(p >> 4) as usize]
    }
    #[inline]
    fn p1(&self, p: u16) -> u32 {
        self.t[((2048 - p) >> 4) as usize]
    }
    #[inline]
    fn pbit(&self, p: u16, b: u32) -> u32 {
        if b != 0 {
            self.p1(p)
        } else {
            self.p0(p)
        }
    }
    fn tree(&self, probs: &[u16], nb: u32, sym: u32) -> u32 {
        let (mut m, mut s) = (1usize, 0);
        for i in (0..nb).rev() {
            let b = (sym >> i) & 1;
            s += self.pbit(probs[m], b);
            m = (m << 1) | b as usize;
        }
        s
    }
    fn rtree(&self, probs: &[u16], base: usize, nb: u32, mut sym: u32) -> u32 {
        let (mut m, mut s) = (1usize, 0);
        for _ in 0..nb {
            let b = sym & 1;
            sym >>= 1;
            s += self.pbit(probs[base + m - 1], b);
            m = (m << 1) | b as usize;
        }
        s
    }
    fn literal(&self, m: &Model, pos: u32, prev: u8, sym: u8, matched: bool, mb: u8) -> u32 {
        let base = m.lit_base(pos, prev);
        let probs = &m.lit[base..base + LIT_SIZE];
        let (mut mm, mut s, mut same) = (1usize, 0u32, matched);
        for i in (0..8).rev() {
            let b = ((sym >> i) & 1) as u32;
            if same {
                let mbit = ((mb >> i) & 1) as usize;
                s += self.pbit(probs[0x100 + (mbit << 8) + mm], b);
                same = b as usize == mbit;
            } else {
                s += self.pbit(probs[mm], b);
            }
            mm = (mm << 1) | b as usize;
        }
        s
    }
    fn len_prices(&self, lm: &LenModel, nps: usize) -> Vec<[u32; (MAXLEN + 1) as usize]> {
        let (c0, c1, c20, c21) = (self.p0(lm.choice), self.p1(lm.choice), self.p0(lm.choice2), self.p1(lm.choice2));
        let mut tab = vec![[0u32; (MAXLEN + 1) as usize]; 16];
        for (ps, row) in tab.iter_mut().enumerate().take(nps) {
            for len in MINLEN..=MAXLEN {
                let l = len - MINLEN;
                row[len as usize] = if l < 8 {
                    c0 + self.tree(&lm.low[ps], 3, l)
                } else if l < 16 {
                    c1 + c20 + self.tree(&lm.mid[ps], 3, l - 8)
                } else {
                    c1 + c21 + self.tree(&lm.high, 8, l - 16)
                };
            }
        }
        tab
    }
    fn update(&mut self, m: &Model) {
        let nps = 1usize << m.pb;
        self.len = self.len_prices(&m.len, nps);
        self.replen = self.len_prices(&m.replen, nps);
        for i in 0..16 {
            self.align[i] = self.rtree(&m.align, 1, 4, i as u32);
        }
        for ls in 0..4 {
            for s in 0..64u32 {
                let mut p = self.tree(&m.pos_slot[ls], 6, s);
                if s >= END_POS_IDX {
                    p += ((s >> 1) - 1 - 4) << 4; // direct bits
                }
                self.slot[ls][s as usize] = p;
            }
            for d in 0..FULL_DIST {
                let s = pos_slot(d);
                let mut p = self.slot[ls][s as usize];
                if s >= 4 {
                    let fb = (s >> 1) - 1;
                    let base = (2 | (s & 1)) << fb;
                    p += self.rtree(&m.pos_special, (base - s) as usize, fb, d - base);
                }
                self.full[ls][d as usize] = p;
            }
        }
    }
    #[inline]
    fn dist(&self, d: u32, len: u32) -> u32 {
        let ls = (len - MINLEN).min(3) as usize;
        if d < FULL_DIST {
            return self.full[ls][d as usize];
        }
        self.slot[ls][pos_slot(d) as usize] + self.align[(d & 15) as usize]
    }
}

// ------------------------------------------------------------------
// Encoder: match finder + optimal parser
// ------------------------------------------------------------------
#[derive(Clone, Copy, Default)]
struct Node {
    price: u32,
    from: u32,
    len: u32,
    dist: u32,
    kind: u8,
    state: u8,
    reps: [u32; 4],
}
const K_LIT: u8 = 0;
const K_SHORTREP: u8 = 1;
const K_REP0: u8 = 2;
const K_MATCH: u8 = 6;

#[derive(Clone, Copy, Default)]
struct MatchPair {
    len: u32,
    dist: u32,
}

struct Enc<'a> {
    m: Box<Model>,
    pr: Prices,
    buf: &'a [u8],
    n: u32,
    head4: Vec<u32>,
    head3: Vec<u32>,
    son: Vec<u32>,
    ins: u32,
    depth: u32,
    nice: u32,
    state: u32,
    reps: [u32; 4],
    nodes: Vec<Node>,
    mp: Vec<MatchPair>,
    path: Vec<u32>,
}

#[inline]
fn rd32(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])
}
#[inline]
fn h4(d: &[u8], i: usize) -> usize {
    (rd32(d, i).wrapping_mul(2_654_435_761) >> (32 - H4BITS)) as usize
}
#[inline]
fn h3(d: &[u8], i: usize) -> usize {
    ((rd32(d, i) & 0xFF_FFFF).wrapping_mul(506_832_829) >> (32 - H3BITS)) as usize
}
/// Number of equal bytes of d[a..] and d[b..], at most `limit`.
#[inline]
fn match_len(d: &[u8], a: usize, b: usize, limit: u32) -> u32 {
    let limit = limit as usize;
    let (x, y) = (&d[a..a + limit], &d[b..b + limit]);
    let mut l = 0;
    while l + 8 <= limit {
        let v = u64::from_le_bytes(x[l..l + 8].try_into().unwrap()) ^ u64::from_le_bytes(y[l..l + 8].try_into().unwrap());
        if v != 0 {
            return (l + (v.trailing_zeros() >> 3) as usize) as u32;
        }
        l += 8;
    }
    while l < limit && x[l] == y[l] {
        l += 1;
    }
    l as u32
}

impl<'a> Enc<'a> {
    /// Binary-tree match finder: searches matches at pos and inserts pos into the tree.
    fn bt_find(&mut self, pos: u32, len_limit: u32, mut max_len: u32, mut cnt: usize, record: bool) -> usize {
        let buf = self.buf;
        let cur = pos as usize;
        let h = h4(buf, cur);
        let mut cur_match = self.head4[h];
        self.head4[h] = pos;
        // ptr1/ptr0 as indices into son
        let mut ptr1 = 2 * cur;
        let mut ptr0 = ptr1 + 1;
        let (mut len0, mut len1) = (0u32, 0u32);
        let mut cut = self.depth;
        loop {
            if cur_match == NIL || cut == 0 {
                self.son[ptr0] = NIL;
                self.son[ptr1] = NIL;
                break;
            }
            cut -= 1;
            let pb = cur_match as usize;
            let pair = 2 * pb;
            let mut len = len0.min(len1);
            if buf[pb + len as usize] == buf[cur + len as usize] {
                len += match_len(buf, cur + len as usize, pb + len as usize, len_limit - len);
                if record && len > max_len {
                    max_len = len;
                    self.mp[cnt] = MatchPair { len, dist: pos - cur_match - 1 };
                    cnt += 1;
                }
                if len == len_limit {
                    self.son[ptr1] = self.son[pair];
                    self.son[ptr0] = self.son[pair + 1];
                    break;
                }
            }
            if buf[pb + len as usize] < buf[cur + len as usize] {
                self.son[ptr1] = cur_match;
                ptr1 = pair + 1;
                cur_match = self.son[ptr1];
                len1 = len;
            } else {
                self.son[ptr0] = cur_match;
                ptr0 = pair;
                cur_match = self.son[ptr0];
                len0 = len;
            }
        }
        cnt
    }
    #[inline]
    fn bt_limit(&self, pos: u32) -> u32 {
        (self.n - pos).min(self.nice)
    }
    fn skip_to(&mut self, t: u32) {
        while self.ins < t {
            let i = self.ins;
            if i + 4 <= self.n {
                let hh = h3(self.buf, i as usize);
                self.head3[hh] = i;
                let lim = self.bt_limit(i);
                self.bt_find(i, lim, 0, 0, false);
            }
            self.ins += 1;
        }
    }
    /// Matches with strictly increasing length (shortest distance for each).
    fn get_matches(&mut self, pos: u32, avail: u32) -> usize {
        self.skip_to(pos);
        if pos + 4 > self.n {
            self.ins = pos + 1;
            return 0;
        }
        let cur = pos as usize;
        let mut cnt = 0;
        let mut best = 2u32;
        let hh = h3(self.buf, cur);
        let c = self.head3[hh];
        self.head3[hh] = pos;
        if c != NIL && c < pos && pos - c <= 65536 {
            let l = match_len(self.buf, cur, c as usize, avail);
            if l > best {
                self.mp[cnt] = MatchPair { len: l, dist: pos - c - 1 };
                cnt += 1;
                best = l;
            }
        }
        let lim = self.bt_limit(pos);
        cnt = self.bt_find(pos, lim, best, cnt, true);
        self.ins = pos + 1;
        // Match reaches the limit: extend it beyond the "nice" length
        if cnt > 0 && self.mp[cnt - 1].len == lim && lim < avail {
            let mp = self.mp[cnt - 1];
            let a = cur + mp.len as usize;
            self.mp[cnt - 1].len += match_len(self.buf, a, a - mp.dist as usize - 1, avail - mp.len);
        }
        cnt
    }

    fn node_derive(&mut self, j: usize) {
        let t = self.nodes[j];
        let f = self.nodes[t.from as usize];
        let n = &mut self.nodes[j];
        match t.kind {
            K_LIT => {
                n.state = NEXT_LIT[f.state as usize];
                n.reps = f.reps;
            }
            K_SHORTREP => {
                n.state = next_shortrep(f.state as u32) as u8;
                n.reps = f.reps;
            }
            K_MATCH => {
                n.state = next_match(f.state as u32) as u8;
                n.reps = [t.dist, f.reps[0], f.reps[1], f.reps[2]];
            }
            _ => {
                let r = (t.kind - K_REP0) as usize;
                n.state = next_rep(f.state as u32) as u8;
                n.reps[0] = f.reps[r];
                let mut src = 0;
                for k in 1..4 {
                    if src == r {
                        src += 1;
                    }
                    n.reps[k] = f.reps[src];
                    src += 1;
                }
            }
        }
    }

    #[inline]
    fn relax(nodes: &mut [Node], j: u32, price: u32, from: u32, kind: u8, len: u32, dist: u32) {
        let n = &mut nodes[j as usize];
        if price < n.price {
            n.price = price;
            n.from = from;
            n.kind = kind;
            n.len = len;
            n.dist = dist;
        }
    }

    fn rep_choice_price(&self, r: u32, st: usize, ps: usize) -> u32 {
        let (m, pr) = (&self.m, &self.pr);
        match r {
            0 => pr.p0(m.g0[st]) + pr.p1(m.rep0_long[st][ps]),
            1 => pr.p1(m.g0[st]) + pr.p0(m.g1[st]),
            2 => pr.p1(m.g0[st]) + pr.p1(m.g1[st]) + pr.p0(m.g2[st]),
            _ => pr.p1(m.g0[st]) + pr.p1(m.g1[st]) + pr.p1(m.g2[st]),
        }
    }

    fn encode_step(&mut self, rc: &mut Rce, pos: u32, kind: u8, len: u32, dist: u32) {
        let m = &mut *self.m;
        let st = self.state as usize;
        let ps = (pos & ((1 << m.pb) - 1)) as usize;
        if kind == K_LIT {
            rc.bit(&mut m.is_match[st][ps], 0);
            let p = pos as usize;
            let prev = if p > 0 { self.buf[p - 1] } else { 0 };
            let matched = st >= 7;
            let mb = if matched { self.buf[p - self.reps[0] as usize - 1] } else { 0 };
            rc.literal(m, pos, prev, self.buf[p], matched, mb);
            self.state = NEXT_LIT[st] as u32;
            return;
        }
        rc.bit(&mut m.is_match[st][ps], 1);
        if kind == K_MATCH {
            rc.bit(&mut m.is_rep[st], 0);
            rc.len(&mut m.len, len, ps);
            rc.dist(m, dist, len);
            self.reps = [dist, self.reps[0], self.reps[1], self.reps[2]];
            self.state = next_match(st as u32);
            return;
        }
        rc.bit(&mut m.is_rep[st], 1);
        if kind == K_SHORTREP {
            rc.bit(&mut m.g0[st], 0);
            rc.bit(&mut m.rep0_long[st][ps], 0);
            self.state = next_shortrep(st as u32);
            return;
        }
        let r = (kind - K_REP0) as usize;
        if r == 0 {
            rc.bit(&mut m.g0[st], 0);
            rc.bit(&mut m.rep0_long[st][ps], 1);
        } else {
            rc.bit(&mut m.g0[st], 1);
            if r == 1 {
                rc.bit(&mut m.g1[st], 0);
            } else {
                rc.bit(&mut m.g1[st], 1);
                rc.bit(&mut m.g2[st], (r == 3) as u32);
            }
            let d = self.reps[r];
            for k in (1..=r).rev() {
                self.reps[k] = self.reps[k - 1];
            }
            self.reps[0] = d;
        }
        rc.len(&mut m.replen, len, ps);
        self.state = next_rep(st as u32);
    }

    /// Parse one window optimally via DP and encode it; returns the number of bytes processed.
    fn parse_window(&mut self, rc: &mut Rce, pos: u32) -> u32 {
        let pbm = (1u32 << self.m.pb) - 1;
        let last = (self.n - pos).min(OPT_NUM);
        let reach = last + MAXLEN + 1;
        for k in 0..=reach as usize {
            self.nodes[k].price = INF;
        }
        self.nodes[0].price = 0;
        self.nodes[0].state = self.state as u8;
        self.nodes[0].reps = self.reps;
        let mut end = last;
        let buf = self.buf;

        let mut i = 0u32;
        while i < end {
            if i > 0 {
                self.node_derive(i as usize);
            }
            let nd = self.nodes[i as usize];
            let p = pos + i;
            let st = nd.state as usize;
            let ps = (p & pbm) as usize;
            let cur = p as usize;
            let remaining = self.n - p;
            let avail = remaining.min(MAXLEN);

            // Literal
            let prevb = if cur > 0 { buf[cur - 1] } else { 0 };
            let matched = st >= 7;
            let mb = if matched { buf[cur - nd.reps[0] as usize - 1] } else { 0 };
            let lp = nd.price + self.pr.p0(self.m.is_match[st][ps]) + self.pr.literal(&self.m, p, prevb, buf[cur], matched, mb);
            Self::relax(&mut self.nodes, i + 1, lp, i, K_LIT, 1, 0);

            let match_p = nd.price + self.pr.p1(self.m.is_match[st][ps]);
            let rep_p = match_p + self.pr.p1(self.m.is_rep[st]);
            let mut longest = 0u32;

            // Short rep
            if nd.reps[0] < p && buf[cur] == buf[cur - nd.reps[0] as usize - 1] {
                let pr = rep_p + self.pr.p0(self.m.g0[st]) + self.pr.p0(self.m.rep0_long[st][ps]);
                Self::relax(&mut self.nodes, i + 1, pr, i, K_SHORTREP, 1, 0);
            }

            // Repeat distances
            if avail >= MINLEN {
                for r in 0..4u32 {
                    let d = nd.reps[r as usize];
                    if d >= p {
                        continue;
                    }
                    let s = cur - d as usize - 1;
                    if buf[s] != buf[cur] || buf[s + 1] != buf[cur + 1] {
                        continue;
                    }
                    let l = match_len(buf, cur, s, avail);
                    let base = rep_p + self.rep_choice_price(r, st, ps);
                    let mut ll = MINLEN;
                    while ll <= l {
                        let pr = base + self.pr.replen[ps][ll as usize];
                        Self::relax(&mut self.nodes, i + ll, pr, i, K_REP0 + r as u8, ll, d);
                        ll = if ll < DENSE_LEN || ll == l {
                            ll + 1
                        } else if ll + 16 < l {
                            ll + 16
                        } else {
                            l
                        };
                    }
                    longest = longest.max(l);
                }
            }

            // New matches
            let nm = self.get_matches(p, avail);
            if nm > 0 {
                let norm_p = match_p + self.pr.p0(self.m.is_rep[st]);
                let mut ll = MINLEN;
                for k in 0..nm {
                    let MatchPair { len: ml, dist: d } = self.mp[k];
                    let isrep = nd.reps.contains(&d);
                    if !isrep {
                        while ll <= ml {
                            let pr = norm_p + self.pr.len[ps][ll as usize] + self.pr.dist(d, ll);
                            Self::relax(&mut self.nodes, i + ll, pr, i, K_MATCH, ll, d);
                            ll = if ll < DENSE_LEN || ll == ml {
                                ll + 1
                            } else if ll + 16 < ml {
                                ll + 16
                            } else {
                                ml
                            };
                        }
                    }
                    ll = ml + 1;
                    longest = longest.max(ml);
                }
            }

            if longest >= self.nice {
                end = i + longest; // long match: end the window here
                break;
            }
            i += 1;
        }

        // Trace back, then encode forwards
        self.path.clear();
        let mut j = end;
        while j > 0 {
            self.path.push(j);
            j = self.nodes[j as usize].from;
        }
        for k in (0..self.path.len()).rev() {
            let t = self.nodes[self.path[k] as usize];
            let dist = if t.kind >= K_REP0 && t.kind < K_MATCH { 0 } else { t.dist };
            self.encode_step(rc, pos + t.from, t.kind, t.len, dist);
        }
        end
    }
}

/// Compresses `src` completely (header: lc|lp<<4, pb, then range coder data).
pub fn compress(src: &[u8], p: &Params) -> Vec<u8> {
    assert!(src.len() <= 0xFFFF_FFF0, "block too large");
    let n = src.len() as u32;
    let mut out = Vec::with_capacity(src.len() / 3 + 64);
    out.push((p.lc | (p.lp << 4)) as u8);
    out.push(p.pb as u8);
    let mut rc = Rce::new(out);
    let mut e = Enc {
        m: Model::new(p.lc, p.lp, p.pb),
        pr: Prices {
            t: price_table(),
            len: Vec::new(),
            replen: Vec::new(),
            slot: [[0; 64]; 4],
            full: [[0; FULL_DIST as usize]; 4],
            align: [0; 16],
        },
        buf: src,
        n,
        head4: vec![NIL; 1 << H4BITS],
        head3: vec![NIL; 1 << H3BITS],
        son: vec![0; 2 * src.len().max(1)],
        ins: 0,
        depth: p.depth,
        nice: p.nice.min(MAXLEN),
        state: 0,
        reps: [0; 4],
        nodes: vec![Node::default(); (OPT_NUM + MAXLEN + 2) as usize],
        mp: vec![MatchPair::default(); (MAXLEN + 2) as usize],
        path: Vec::with_capacity((OPT_NUM + MAXLEN + 2) as usize),
    };
    let (mut pos, mut since_update) = (0u32, 1u32 << 30);
    while pos < n {
        if since_update >= 1024 {
            e.pr.update(&e.m);
            since_update = 0;
        }
        let adv = e.parse_window(&mut rc, pos);
        pos += adv;
        since_update += adv;
    }
    rc.finish()
}

/// Fast size estimate for the filter choice: the same model and the same range coder,
/// but greedy parsing (longest match or repeat) instead of the optimal DP.
/// Produces a valid LZRC stream, but is about 5x faster than level 2.
pub fn estimate(src: &[u8]) -> usize {
    let p = Params::level(1);
    let n = src.len() as u32;
    let mut rc = Rce::new(Vec::with_capacity(src.len() / 2 + 64));
    let mut e = Enc {
        m: Model::new(p.lc, p.lp, p.pb),
        pr: Prices {
            t: [0; 128],
            len: Vec::new(),
            replen: Vec::new(),
            slot: [[0; 64]; 4],
            full: [[0; FULL_DIST as usize]; 4],
            align: [0; 16],
        },
        buf: src,
        n,
        head4: vec![NIL; 1 << H4BITS],
        head3: vec![NIL; 1 << H3BITS],
        son: vec![0; 2 * src.len().max(1)],
        ins: 0,
        depth: 4,
        nice: 32,
        state: 0,
        reps: [0; 4],
        nodes: Vec::new(),
        mp: vec![MatchPair::default(); (MAXLEN + 2) as usize],
        path: Vec::new(),
    };
    let mut pos = 0u32;
    while pos < n {
        let cur = pos as usize;
        let avail = (n - pos).min(MAXLEN);
        // Repeat of the last distance?
        let d0 = e.reps[0];
        let rep_len = if avail >= MINLEN && d0 < pos { match_len(src, cur, cur - d0 as usize - 1, avail) } else { 0 };
        let nm = e.get_matches(pos, avail);
        let (ml, md) = if nm > 0 { (e.mp[nm - 1].len, e.mp[nm - 1].dist) } else { (0, 0) };
        let (kind, len, dist) = if rep_len >= MINLEN && rep_len + 1 >= ml {
            (K_REP0, rep_len, 0)
        } else if ml >= 3 || (ml == 2 && md < 256) {
            (K_MATCH, ml, md)
        } else {
            (K_LIT, 1, 0)
        };
        e.encode_step(&mut rc, pos, kind, len, dist);
        pos += len;
    }
    rc.finish().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    #[test]
    fn estimate_is_valid_stream() {
        // estimate() encodes real LZRC data; the size is close to level 2
        let mut r = Rng::new(5);
        let d = sample(&mut r, 300_000);
        let est = estimate(&d);
        let real = compress(&d, &Params::level(2)).len();
        assert!(est >= real * 9 / 10 && est <= real * 2, "estimate {est} vs. {real}");
    }

    fn sample(r: &mut Rng, n: usize) -> Vec<u8> {
        let words: [&[u8]; 5] = [b"dragon ", b"sword ", b"{\"id\": 42}, ", b"\x00\x00\x80\x3f", b"\n    return x;"];
        let mut v = Vec::with_capacity(n);
        while v.len() < n {
            if r.next() % 5 == 0 {
                v.push(r.next() as u8);
            } else {
                v.extend_from_slice(words[(r.next() % 5) as usize]);
            }
        }
        v.truncate(n);
        v
    }

    #[test]
    fn roundtrip_levels_and_params() {
        let mut r = Rng::new(9);
        for (k, &n) in [0usize, 1, 7, 300, 70_000, 400_000].iter().enumerate() {
            let d = sample(&mut r, n);
            for lvl in [2, 9] {
                let mut p = Params::level(lvl);
                if k % 2 == 1 {
                    p.lc = 4;
                    p.pb = 0;
                }
                let c = compress(&d, &p);
                let mut out = vec![0u8; n];
                decompress(&c, &mut out).expect("decodable");
                assert_eq!(out, d);
            }
        }
        // incompressible data and long runs
        let rnd = r.bytes(100_000);
        let c = compress(&rnd, &Params::level(9));
        let mut out = vec![0u8; rnd.len()];
        decompress(&c, &mut out).unwrap();
        assert_eq!(out, rnd);
        let zeros = vec![0u8; 300_000];
        let c = compress(&zeros, &Params::level(9));
        assert!(c.len() < 1000);
        let mut out = vec![1u8; zeros.len()];
        decompress(&c, &mut out).unwrap();
        assert_eq!(out, zeros);
    }

    #[test]
    fn corrupt_never_panics() {
        let mut r = Rng::new(10);
        let d = sample(&mut r, 50_000);
        let c = compress(&d, &Params::level(5));
        let mut out = vec![0u8; d.len()];
        for _ in 0..400 {
            let mut m = c.clone();
            for _ in 0..1 + r.next() % 6 {
                let i = r.next() as usize % m.len();
                m[i] ^= 1 + (r.next() % 255) as u8;
            }
            let cut = if r.next() % 5 == 0 { r.next() as usize % m.len() } else { m.len() };
            let _ = decompress(&m[..cut], &mut out);
        }
    }
}
