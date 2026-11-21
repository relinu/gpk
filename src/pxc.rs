//! PXC – lossless pixel coder for 8-bit images (port of `legacy-c/gpk/pxc.c`, byte-identical).
//!
//! Optional color transform (R-G, B-G), weighted mix of 6 predictors by local error,
//! bias correction, context-adaptive arithmetic coding. Integer arithmetic only.

const NACT: usize = 10;
const NERR: usize = 7;
const NCTX: usize = NACT * NERR;
const NPRED: usize = 6;
const MAXE: usize = 11;
const TACT: [i32; NACT - 1] = [0, 2, 4, 7, 11, 17, 26, 40, 64];
const TERR: [i32; NERR - 1] = [0, 2, 4, 8, 15, 28];

#[derive(Clone, Copy)]
struct Prob {
    f: u16,
    s: u16,
}
const P_INIT: Prob = Prob { f: 32768, s: 32768 };
impl Prob {
    #[inline]
    fn p12(&self) -> u32 {
        ((self.f as u32 + self.s as u32) >> 5).clamp(1, 4095)
    }
    #[inline]
    fn update(&mut self, bit: bool) {
        if bit {
            self.f += (65535 - self.f) >> 4;
            self.s += (65535 - self.s) >> 7;
        } else {
            self.f -= self.f >> 4;
            self.s -= self.s >> 7;
        }
    }
}

struct Enc {
    x1: u32,
    x2: u32,
    out: Vec<u8>,
}
impl Enc {
    #[inline]
    fn bit(&mut self, p: &mut Prob, bit: bool) {
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p.p12() as u64) >> 12) as u32;
        if bit {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        p.update(bit);
        while (self.x1 ^ self.x2) & 0xFF00_0000 == 0 {
            self.out.push((self.x2 >> 24) as u8);
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
        }
    }
}
struct Dec<'a> {
    x1: u32,
    x2: u32,
    x: u32,
    src: &'a [u8],
    ip: usize,
}
impl<'a> Dec<'a> {
    #[inline]
    fn next(&mut self) -> u32 {
        let b = self.src.get(self.ip).copied().unwrap_or(0);
        self.ip += 1;
        b as u32
    }
    #[inline]
    fn bit(&mut self, p: &mut Prob) -> bool {
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * p.p12() as u64) >> 12) as u32;
        let bit = self.x <= xmid;
        if bit {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        p.update(bit);
        while (self.x1 ^ self.x2) & 0xFF00_0000 == 0 {
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
            self.x = (self.x << 8) | self.next();
        }
        bit
    }
}

struct Model {
    zero: [[Prob; NCTX]; 4],
    sign: [[Prob; NCTX]; 4],
    expo: [[[Prob; MAXE + 1]; NCTX]; 4],
    mant: [[[[Prob; 2]; MAXE + 1]; NERR]; 4],
    bsum: [[i32; NCTX]; 4],
    bcnt: [[i32; NCTX]; 4],
}
impl Model {
    fn new() -> Box<Self> {
        Box::new(Model {
            zero: [[P_INIT; NCTX]; 4],
            sign: [[P_INIT; NCTX]; 4],
            expo: [[[P_INIT; MAXE + 1]; NCTX]; 4],
            mant: [[[[P_INIT; 2]; MAXE + 1]; NERR]; 4],
            bsum: [[0; NCTX]; 4],
            bcnt: [[0; NCTX]; 4],
        })
    }
}

/// Per channel: whole plane, |error| per predictor and |residual| for the row above [0] / current [1].
struct Chan {
    pl: Vec<i16>,
    pe: [[Vec<u16>; 2]; NPRED],
    re: [Vec<u16>; 2],
}
impl Chan {
    fn new(w: usize, h: usize) -> Self {
        Chan { pl: vec![0; w * h], pe: std::array::from_fn(|_| [vec![0; w], vec![0; w]]), re: [vec![0; w], vec![0; w]] }
    }
    fn next_row(&mut self) {
        for p in self.pe.iter_mut() {
            p.swap(0, 1);
        }
        self.re.swap(0, 1);
    }
}

#[inline]
fn qclass(v: i32, t: &[i32]) -> usize {
    let mut c = 0;
    while c < t.len() && v > t[c] {
        c += 1;
    }
    c
}
#[inline]
fn med(w: i32, n: i32, nw: i32) -> i32 {
    let (mx, mn) = (w.max(n), w.min(n));
    if nw >= mx {
        mn
    } else if nw <= mn {
        mx
    } else {
        w + n - nw
    }
}

/// `(1 << 40) / (e * e)` for small e (the common case; a division is slow).
const WT_N: usize = 1024;
fn wt_table() -> &'static [i64; WT_N] {
    static T: std::sync::OnceLock<[i64; WT_N]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0i64; WT_N];
        for (e, v) in t.iter_mut().enumerate().skip(1) {
            *v = (1i64 << 40) / (e as i64 * e as i64);
        }
        t
    })
}

/// `a / b` rounded toward zero, like the integer division, but much cheaper than a 64-bit
/// `idiv` on many CPUs. Exact as long as |a|, |b| < 2^53 and 1/|b| > |a/b| * 2^-53: then the
/// quotient is either an integer (computed exactly) or too far from the next integer for the
/// f64 rounding error to reach it. The callers' value ranges are stated at the call sites.
#[inline(always)]
fn div_trunc(a: i64, b: i64) -> i64 {
    (a as f64 / b as f64) as i64
}

struct Ctx {
    pred: i32,
    ctx: usize,
    eq: usize,
    p: [i32; NPRED],
}

fn context(m: &Model, c: &Chan, w: usize, x: usize, y: usize, k: usize, r0mag: i32) -> Ctx {
    let row = y * w;
    let px = |i: usize| c.pl[i] as i32;
    let wv = if x > 0 {
        px(row + x - 1)
    } else if y > 0 {
        px(row - w + x)
    } else {
        0
    };
    let n = if y > 0 { px(row - w + x) } else { wv };
    let nw = if x > 0 && y > 0 { px(row - w + x - 1) } else { n };
    let ne = if y > 0 && x + 1 < w { px(row - w + x + 1) } else { n };
    let ww = if x > 1 { px(row + x - 2) } else { wv };
    let g = wv + n - nw;
    let (mx, mn) = (wv.max(n), wv.min(n));
    let p = [med(wv, n, nw), g.clamp(mn, mx), wv, n, (wv + n + 1) >> 1, (wv + n + nw + ne + 2) >> 2];
    // Weighted mix: weight ~ 1 / (local error)^2
    let (mut num, mut den) = (0i64, 0i64);
    let wtt = wt_table();
    for (i, &pi) in p.iter().enumerate() {
        let mut e: u32 = 1;
        if y > 0 {
            let up = &c.pe[i][0];
            e += up[x] as u32;
            if x > 0 {
                e += up[x - 1] as u32;
            }
            if x + 1 < w {
                e += up[x + 1] as u32;
            }
        }
        if x > 0 {
            e += 2 * c.pe[i][1][x - 1] as u32;
        }
        let wt = if (e as usize) < WT_N { wtt[e as usize] } else { (1i64 << 40) / (e as i64 * e as i64) };
        num += wt * pi as i64;
        den += wt;
    }
    // den <= 6 * 2^40, |num| <= 255 * den < 2^51 and den >= 60: the quotient (|q| <= 256) is
    // either exact or at least 1/den > 2^-44 away from an integer, more than the f64 error.
    let blend = div_trunc(num + den / 2, den) as i32;
    let re = &c.re;
    let mut en = if x > 0 { re[1][x - 1] as i32 } else { 0 } + if y > 0 { re[0][x] as i32 } else { 0 };
    if y > 0 && x > 0 {
        en += (re[0][x - 1] >> 1) as i32;
    }
    if y > 0 && x + 1 < w {
        en += (re[0][x + 1] >> 1) as i32;
    }
    if k > 0 {
        en += r0mag >> 1;
    }
    let act = (ne - n).abs() + (n - nw).abs() + (nw - wv).abs() + ((wv - ww).abs() >> 1);
    let eq = qclass(en, &TERR);
    let ctx = qclass(act, &TACT) * NERR + eq;
    let bc = m.bcnt[k][ctx];
    // |bsum| < 2^20 and 1 <= bc < 32
    let bias = if bc != 0 { div_trunc(m.bsum[k][ctx] as i64, bc as i64) as i32 } else { 0 };
    Ctx { pred: blend + bias, ctx, eq, p }
}

fn learn(m: &mut Model, c: &mut Chan, x: usize, k: usize, o: &Ctx, v: i32) {
    for i in 0..NPRED {
        c.pe[i][1][x] = (v - o.p[i]).unsigned_abs().min(65535) as u16;
    }
    let r = v - o.pred;
    c.re[1][x] = r.unsigned_abs() as u16;
    let (bs, bc) = (&mut m.bsum[k][o.ctx], &mut m.bcnt[k][o.ctx]);
    *bs += r + if *bc != 0 { div_trunc(*bs as i64, *bc as i64) as i32 } else { 0 };
    *bc += 1;
    if *bc >= 32 {
        *bs /= 2;
        *bc /= 2;
    }
}

fn code_res(a: &mut Enc, m: &mut Model, k: usize, o: &Ctx, r: i32) {
    a.bit(&mut m.zero[k][o.ctx], r != 0);
    if r == 0 {
        return;
    }
    a.bit(&mut m.sign[k][o.ctx], r < 0);
    let v = r.unsigned_abs();
    let mut e = 0usize;
    while v >> (e + 1) != 0 {
        e += 1;
    }
    for i in 0..e {
        a.bit(&mut m.expo[k][o.ctx][i], true);
    }
    if e < MAXE {
        a.bit(&mut m.expo[k][o.ctx][e], false);
    }
    for b in (0..e).rev() {
        a.bit(&mut m.mant[k][o.eq][e][if b == e - 1 { 0 } else { 1 }], (v >> b) & 1 != 0);
    }
}
fn decode_res(a: &mut Dec, m: &mut Model, k: usize, o: &Ctx) -> i32 {
    if !a.bit(&mut m.zero[k][o.ctx]) {
        return 0;
    }
    let neg = a.bit(&mut m.sign[k][o.ctx]);
    let mut e = 0usize;
    while e < MAXE && a.bit(&mut m.expo[k][o.ctx][e]) {
        e += 1;
    }
    let mut v = 1i32;
    for b in (0..e).rev() {
        v = (v << 1) | a.bit(&mut m.mant[k][o.eq][e][if b == e - 1 { 0 } else { 1 }]) as i32;
    }
    if neg {
        -v
    } else {
        v
    }
}

fn encode_ct(px: &[u8], w: usize, h: usize, ch: usize, ct: bool) -> Vec<u8> {
    let mut c: Vec<Chan> = (0..ch).map(|_| Chan::new(w, h)).collect();
    for (i, p) in px.chunks_exact(ch).enumerate().take(w * h) {
        if ct && ch >= 3 {
            c[0].pl[i] = p[1] as i16;
            c[1].pl[i] = p[0] as i16 - p[1] as i16;
            c[2].pl[i] = p[2] as i16 - p[1] as i16;
            if ch == 4 {
                c[3].pl[i] = p[3] as i16;
            }
        } else {
            for k in 0..ch {
                c[k].pl[i] = p[k] as i16;
            }
        }
    }
    let mut m = Model::new();
    let mut a = Enc { x1: 0, x2: 0xFFFF_FFFF, out: vec![ct as u8] }; // first byte: color transform
    for y in 0..h {
        for x in 0..w {
            let mut r0 = 0;
            for k in 0..ch {
                let o = context(&m, &c[k], w, x, y, k, if ct { r0 } else { 0 });
                let v = c[k].pl[y * w + x] as i32;
                let r = v - o.pred;
                code_res(&mut a, &mut m, k, &o, r);
                learn(&mut m, &mut c[k], x, k, &o, v);
                if k == 0 {
                    r0 = r.abs();
                }
            }
        }
        for ck in c.iter_mut() {
            ck.next_row();
        }
    }
    for _ in 0..4 {
        a.out.push((a.x1 >> 24) as u8);
        a.x1 <<= 8;
    }
    a.out
}

/// Encodes w*h pixels with ch channels (1..4), both color-transform variants, the smaller wins.
pub fn encode(px: &[u8], w: u32, h: u32, ch: usize) -> Option<Vec<u8>> {
    let (w, h) = (w as usize, h as usize);
    if !(1..=4).contains(&ch) || w == 0 || h == 0 || px.len() < w * h * ch {
        return None;
    }
    let a = encode_ct(px, w, h, ch, false);
    if ch < 3 {
        return Some(a);
    }
    let b = encode_ct(px, w, h, ch, true);
    Some(if b.len() < a.len() { b } else { a })
}

/// Decodes into `px` (w*h*ch bytes). `false` on corrupted data.
pub fn decode(src: &[u8], px: &mut [u8], w: u32, h: u32, ch: usize) -> bool {
    let (w, h) = (w as usize, h as usize);
    if !(1..=4).contains(&ch) || w == 0 || h == 0 || px.len() < w * h * ch || src.is_empty() || src[0] > 1 {
        return false;
    }
    let ct = src[0] != 0 && ch >= 3;
    let mut c: Vec<Chan> = (0..ch).map(|_| Chan::new(w, h)).collect();
    let mut m = Model::new();
    let mut a = Dec { x1: 0, x2: 0xFFFF_FFFF, x: 0, src, ip: 1 };
    for _ in 0..4 {
        a.x = (a.x << 8) | a.next();
    }
    for y in 0..h {
        for x in 0..w {
            let mut r0 = 0;
            for k in 0..ch {
                let o = context(&m, &c[k], w, x, y, k, if ct { r0 } else { 0 });
                let r = decode_res(&mut a, &mut m, k, &o);
                let v = o.pred + r;
                let lo = if ct && k > 0 && k < 3 { -255 } else { 0 };
                if v < lo || v > 255 {
                    return false; // corrupted data
                }
                c[k].pl[y * w + x] = v as i16;
                learn(&mut m, &mut c[k], x, k, &o, v);
                if k == 0 {
                    r0 = r.abs();
                }
            }
        }
        for ck in c.iter_mut() {
            ck.next_row();
        }
    }
    for (i, p) in px.chunks_exact_mut(ch).enumerate().take(w * h) {
        if ct {
            let g = c[0].pl[i];
            p[1] = g as u8;
            p[0] = (c[1].pl[i] + g) as u8;
            p[2] = (c[2].pl[i] + g) as u8;
            if ch == 4 {
                p[3] = c[3].pl[i] as u8;
            }
        } else {
            for k in 0..ch {
                p[k] = c[k].pl[i] as u8;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;
    #[test]
    fn roundtrip_and_corrupt() {
        let mut r = Rng::new(11);
        for it in 0..24 {
            let (w, h, ch) = (1 + r.next() % 97, 1 + r.next() % 61, 1 + it % 4);
            let n = (w * h) as usize * ch;
            let px: Vec<u8> = (0..n)
                .map(|i| match it % 3 {
                    0 => r.next() as u8,
                    1 => ((i / ch) as u32 % w * 3 + r.next() % 5) as u8,
                    _ => {
                        if (i / 37) & 1 == 1 {
                            255
                        } else {
                            0
                        }
                    }
                })
                .collect();
            let c = encode(&px, w, h, ch).unwrap();
            let mut out = vec![0u8; n];
            assert!(decode(&c, &mut out, w, h, ch));
            assert_eq!(out, px);
            for k in 0..6 {
                let mut m = c.clone();
                let i = r.next() as usize % m.len();
                m[i] ^= 1 + (r.next() % 255) as u8;
                let cut = if k == 5 { m.len() / 2 } else { m.len() };
                let _ = decode(&m[..cut], &mut out, w, h, ch);
            }
        }
    }
}
