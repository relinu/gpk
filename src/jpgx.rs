//! Lossless, bit-exact JPEG recompression (port of `legacy-c/gpk/jpgx.c`, same format).
//!
//! Baseline/extended sequential (SOF0/SOF1), Huffman, 8 bits, 1–4 components, any
//! subsampling, one scan, restart intervals. The scan is decoded into DCT coefficients and
//! re-encoded with a context model; on unpacking the Huffman stream is recreated exactly.
//! Everything else is rejected.
//! Format: `"JPGX" | u32 head | head | u32 tail | tail | u32 model | model`.

const MAX_PIXELS: u64 = 1 << 28;

#[derive(Clone)]
struct Huff {
    present: bool,
    bits: [u8; 17],
    vals: [u8; 256],
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    code: [u16; 256],
    len: [u8; 256],
}
impl Default for Huff {
    fn default() -> Self {
        Huff {
            present: false,
            bits: [0; 17],
            vals: [0; 256],
            maxcode: [0; 18],
            valptr: [0; 17],
            mincode: [0; 17],
            code: [0; 256],
            len: [0; 256],
        }
    }
}
impl Huff {
    fn build(&mut self, nvals: usize) -> bool {
        let (mut code, mut k) = (0i32, 0usize);
        self.len = [0; 256];
        for l in 1..=16 {
            self.valptr[l] = k as i32;
            self.mincode[l] = code;
            for _ in 0..self.bits[l] {
                if k >= nvals {
                    return false;
                }
                let s = self.vals[k] as usize;
                k += 1;
                self.code[s] = code as u16;
                self.len[s] = l as u8;
                code += 1;
            }
            self.maxcode[l] = if self.bits[l] != 0 { code - 1 } else { -1 };
            if code > (1 << l) {
                return false; // not a valid prefix code
            }
            code <<= 1;
        }
        self.maxcode[17] = i32::MAX;
        self.present = true;
        true
    }
}

#[derive(Clone, Default)]
struct Comp {
    id: u8,
    h: usize,
    v: usize,
    td: usize,
    ta: usize,
    bw: usize,
    bh: usize,
    coef: Vec<i16>,
}

#[derive(Default)]
struct JInfo {
    x: usize,
    y: usize,
    nf: usize,
    ns: usize,
    ri: u32,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
    interleaved: bool,
    c: [Comp; 4],
    scan: [usize; 4],
    dc: [Huff; 4],
    ac: [Huff; 4],
    data_start: usize,
}

#[inline]
fn be16(d: &[u8], i: usize) -> u32 {
    (d[i] as u32) << 8 | d[i + 1] as u32
}

/// Read the header up to and including SOS; `None` for unsupported files.
fn parse_header(d: &[u8]) -> Option<JInfo> {
    let n = d.len();
    let mut j = JInfo::default();
    if n < 4 || d[0] != 0xFF || d[1] != 0xD8 {
        return None;
    }
    let mut p = 2usize;
    let mut have_sof = false;
    loop {
        if p + 4 > n || d[p] != 0xFF {
            return None;
        }
        while p < n && d[p] == 0xFF {
            p += 1; // fill bytes
        }
        if p >= n {
            return None;
        }
        let m = d[p];
        p += 1;
        if m == 0xD8 || m == 0xD9 || (0xD0..=0xD7).contains(&m) || m == 0x01 || p + 2 > n {
            return None;
        }
        let l = be16(d, p) as usize;
        if l < 2 || p + l > n {
            return None;
        }
        let s = &d[p + 2..p + l];
        let sl = l - 2;
        match m {
            0xC0 | 0xC1 => {
                if have_sof || sl < 6 || s[0] != 8 {
                    return None;
                }
                j.y = be16(s, 1) as usize;
                j.x = be16(s, 3) as usize;
                j.nf = s[5] as usize;
                if j.x == 0 || j.y == 0 || !(1..=4).contains(&j.nf) || sl < 6 + 3 * j.nf {
                    return None;
                }
                if j.x as u64 * j.y as u64 > MAX_PIXELS {
                    return None;
                }
                for i in 0..j.nf {
                    let c = &mut j.c[i];
                    c.id = s[6 + 3 * i];
                    c.h = (s[7 + 3 * i] >> 4) as usize;
                    c.v = (s[7 + 3 * i] & 15) as usize;
                    if !(1..=4).contains(&c.h) || !(1..=4).contains(&c.v) {
                        return None;
                    }
                    j.hmax = j.hmax.max(c.h);
                    j.vmax = j.vmax.max(c.v);
                }
                have_sof = true;
            }
            0xC2..=0xCF if m != 0xC4 && m != 0xC8 && m != 0xCC => return None, // progressive, lossless, arithmetic
            0xC4 => {
                let mut q = 0usize;
                while q < sl {
                    if q + 17 > sl {
                        return None;
                    }
                    let (tc, th) = ((s[q] >> 4) as usize, (s[q] & 15) as usize);
                    if tc > 1 || th > 3 {
                        return None;
                    }
                    let h = if tc == 1 { &mut j.ac[th] } else { &mut j.dc[th] };
                    h.bits[0] = 0;
                    let mut tot = 0usize;
                    for l in 1..=16 {
                        h.bits[l] = s[q + l];
                        tot += h.bits[l] as usize;
                    }
                    if tot > 256 || q + 17 + tot > sl {
                        return None;
                    }
                    h.vals[..tot].copy_from_slice(&s[q + 17..q + 17 + tot]);
                    if !h.build(tot) {
                        return None;
                    }
                    q += 17 + tot;
                }
            }
            0xDD => {
                if sl < 2 {
                    return None;
                }
                j.ri = be16(s, 0);
            }
            0xDA => {
                if !have_sof || sl < 1 {
                    return None;
                }
                j.ns = s[0] as usize;
                if !(1..=4).contains(&j.ns) || sl != 4 + 2 * j.ns || j.ns != j.nf {
                    return None; // only one scan with all components
                }
                for i in 0..j.ns {
                    let cid = s[1 + 2 * i];
                    let mut k = None;
                    for jj in 0..j.nf {
                        if j.c[jj].id == cid {
                            k = Some(jj);
                        }
                    }
                    let k = k?;
                    if j.scan[..i].contains(&k) {
                        return None;
                    }
                    j.scan[i] = k;
                    j.c[k].td = (s[2 + 2 * i] >> 4) as usize;
                    j.c[k].ta = (s[2 + 2 * i] & 15) as usize;
                    if j.c[k].td > 3 || j.c[k].ta > 3 || !j.dc[j.c[k].td].present || !j.ac[j.c[k].ta].present {
                        return None;
                    }
                }
                let t = &s[1 + 2 * j.ns..];
                if t[0] != 0 || t[1] != 63 || t[2] != 0 {
                    return None; // Ss=0, Se=63, Ah=Al=0
                }
                j.data_start = p + l;
                break;
            }
            _ => {}
        }
        p += l;
    }
    // Geometry
    j.interleaved = j.ns > 1;
    if j.interleaved {
        j.mcux = j.x.div_ceil(8 * j.hmax);
        j.mcuy = j.y.div_ceil(8 * j.vmax);
        for i in 0..j.nf {
            j.c[i].bw = j.mcux * j.c[i].h;
            j.c[i].bh = j.mcuy * j.c[i].v;
        }
    } else {
        let k = j.scan[0];
        let (hmax, vmax) = (j.hmax, j.vmax);
        let c = &mut j.c[k];
        let cw = (j.x * c.h).div_ceil(hmax);
        let chh = (j.y * c.v).div_ceil(vmax);
        c.bw = cw.div_ceil(8);
        c.bh = chh.div_ceil(8);
        j.mcux = c.bw;
        j.mcuy = c.bh;
    }
    for i in 0..j.nf {
        let c = &mut j.c[i];
        c.coef = vec![0i16; c.bw * c.bh * 64];
    }
    Some(j)
}

/// Blocks in scan order: (component, coefficient offset) per MCU.
fn mcu_blocks(j: &JInfo, mcu: usize, out: &mut Vec<(usize, usize)>) {
    out.clear();
    let (my, mx) = (mcu / j.mcux, mcu % j.mcux);
    for &ci in &j.scan[..j.ns] {
        let cp = &j.c[ci];
        let (hh, vv) = if j.interleaved { (cp.h, cp.v) } else { (1, 1) };
        for v in 0..vv {
            for h in 0..hh {
                let (row, col) = (my * vv + v, mx * hh + h);
                out.push((ci, (row * cp.bw + col) * 64));
            }
        }
    }
}

// ---------------- Decode the Huffman scan ----------------
struct Br<'a> {
    d: &'a [u8],
    p: usize,
    byte: u32,
    cnt: u32,
    err: bool,
}
impl<'a> Br<'a> {
    fn bit(&mut self) -> i32 {
        if self.cnt == 0 {
            if self.p >= self.d.len() {
                self.err = true;
                return 0;
            }
            let x = self.d[self.p];
            self.p += 1;
            if x == 0xFF {
                if self.p >= self.d.len() || self.d[self.p] != 0x00 {
                    self.err = true; // marker in the middle of the data
                    return 0;
                }
                self.p += 1;
            }
            self.byte = x as u32;
            self.cnt = 8;
        }
        self.cnt -= 1;
        ((self.byte >> self.cnt) & 1) as i32
    }
    fn bits(&mut self, n: i32) -> i32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit();
        }
        v
    }
    fn huff(&mut self, h: &Huff) -> i32 {
        let mut code = 0i32;
        for l in 1..=16 {
            code = (code << 1) | self.bit();
            if h.bits[l] != 0 && code <= h.maxcode[l] && code >= h.mincode[l] {
                return h.vals[(h.valptr[l] + code - h.mincode[l]) as usize] as i32;
            }
        }
        self.err = true;
        0
    }
    /// The remaining bits up to the byte boundary must be 1 (standard padding).
    fn pad_ok(&mut self) -> bool {
        let mask = (1u32 << self.cnt) - 1;
        let ok = self.byte & mask == mask;
        self.cnt = 0;
        ok
    }
}
#[inline]
fn extend(v: i32, s: i32) -> i32 {
    if s != 0 && v < (1 << (s - 1)) {
        v - (1 << s) + 1
    } else {
        v
    }
}

fn decode_scan(d: &[u8], j: &mut JInfo) -> Option<usize> {
    let mut b = Br { d, p: j.data_start, byte: 0, cnt: 0, err: false };
    let mut pred = [0i32; 4];
    let mut rst = 0u8;
    let mut left = j.ri;
    let total = j.mcux * j.mcuy;
    let mut blocks = Vec::new();
    for mcu in 0..total {
        mcu_blocks(j, mcu, &mut blocks);
        for &(ci, off) in &blocks {
            let (td, ta) = (j.c[ci].td, j.c[ci].ta);
            let s = b.huff(&j.dc[td]);
            if s > 11 {
                return None;
            }
            let diff = extend(b.bits(s), s);
            pred[ci] += diff;
            let blk = &mut j.c[ci].coef[off..off + 64];
            blk[0] = pred[ci] as i16;
            let mut k = 1usize;
            while k < 64 {
                let rs = b.huff(&j.ac[ta]);
                let (r, s) = ((rs >> 4) as usize, rs & 15);
                if s == 0 {
                    if r != 15 {
                        break;
                    }
                    k += 16;
                    if k > 64 {
                        return None;
                    }
                    continue;
                }
                k += r;
                if k > 63 || s > 10 {
                    return None;
                }
                blk[k] = extend(b.bits(s), s) as i16;
                k += 1;
            }
            if b.err {
                return None;
            }
        }
        if j.ri != 0 {
            left -= 1;
            if left == 0 && mcu + 1 < total {
                // restart interval ends
                if !b.pad_ok() || b.p + 2 > d.len() || d[b.p] != 0xFF || d[b.p + 1] != 0xD0 + rst {
                    return None;
                }
                b.p += 2;
                rst = (rst + 1) & 7;
                left = j.ri;
                pred = [0; 4];
            }
        }
    }
    if b.err || !b.pad_ok() {
        return None;
    }
    Some(b.p)
}

// ---------------- Recreate the Huffman scan exactly ----------------
struct Bw {
    o: Vec<u8>,
    acc: u32,
    cnt: u32,
}
impl Bw {
    fn bits(&mut self, v: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.cnt += 1;
            if self.cnt == 8 {
                let b = self.acc as u8;
                self.o.push(b);
                if b == 0xFF {
                    self.o.push(0x00);
                }
                self.acc = 0;
                self.cnt = 0;
            }
        }
    }
    fn pad(&mut self) {
        if self.cnt != 0 {
            let n = 8 - self.cnt;
            self.bits((1 << n) - 1, n);
        }
    }
    fn emit(&mut self, h: &Huff, sym: usize) -> bool {
        if h.len[sym] == 0 {
            return false;
        }
        self.bits(h.code[sym] as u32, h.len[sym] as u32);
        true
    }
}
fn bitlen(v: i32) -> u32 {
    32 - v.unsigned_abs().leading_zeros()
}

fn encode_scan(j: &JInfo) -> Option<Vec<u8>> {
    let mut w = Bw { o: Vec::new(), acc: 0, cnt: 0 };
    let mut pred = [0i32; 4];
    let mut rst = 0u8;
    let mut left = j.ri;
    let total = j.mcux * j.mcuy;
    let mut blocks = Vec::new();
    for mcu in 0..total {
        mcu_blocks(j, mcu, &mut blocks);
        for &(ci, off) in &blocks {
            let cp = &j.c[ci];
            let blk = &cp.coef[off..off + 64];
            let diff = blk[0] as i32 - pred[ci];
            pred[ci] = blk[0] as i32;
            let s = bitlen(diff);
            if !w.emit(&j.dc[cp.td], s as usize) {
                return None;
            }
            if s != 0 {
                w.bits((if diff < 0 { diff - 1 } else { diff }) as u32 & ((1u32 << s) - 1), s);
            }
            let mut run = 0usize;
            for &v in &blk[1..] {
                let v = v as i32;
                if v == 0 {
                    run += 1;
                    continue;
                }
                while run > 15 {
                    if !w.emit(&j.ac[cp.ta], 0xF0) {
                        return None;
                    }
                    run -= 16;
                }
                let s = bitlen(v);
                if !w.emit(&j.ac[cp.ta], (run << 4) | s as usize) {
                    return None;
                }
                w.bits((if v < 0 { v - 1 } else { v }) as u32 & ((1u32 << s) - 1), s);
                run = 0;
            }
            if run != 0 && !w.emit(&j.ac[cp.ta], 0x00) {
                return None;
            }
        }
        if j.ri != 0 {
            left -= 1;
            if left == 0 && mcu + 1 < total {
                w.pad();
                w.o.extend_from_slice(&[0xFF, 0xD0 + rst]);
                rst = (rst + 1) & 7;
                left = j.ri;
                pred = [0; 4];
            }
        }
    }
    w.pad();
    Some(w.o)
}

// ---------------- Context model for the coefficients ----------------
#[derive(Clone, Copy)]
struct Prob {
    f: u16,
    s: u16,
}
const P_INIT: Prob = Prob { f: 32768, s: 32768 };

/// Binary arithmetic coder; the same model function encodes (dec=false) or reads (dec=true).
struct Ac<'a> {
    x1: u32,
    x2: u32,
    x: u32,
    out: Vec<u8>,
    src: &'a [u8],
    ip: usize,
    dec: bool,
}
impl<'a> Ac<'a> {
    fn bit(&mut self, p: &mut Prob, bit: bool) -> bool {
        let pr = ((p.f as u32 + p.s as u32) >> 5).clamp(1, 4095);
        let xmid = self.x1 + (((self.x2 - self.x1) as u64 * pr as u64) >> 12) as u32;
        let bit = if self.dec { self.x <= xmid } else { bit };
        if bit {
            self.x2 = xmid;
            p.f += (65535 - p.f) >> 4;
            p.s += (65535 - p.s) >> 7;
        } else {
            self.x1 = xmid + 1;
            p.f -= p.f >> 4;
            p.s -= p.s >> 7;
        }
        while (self.x1 ^ self.x2) & 0xFF00_0000 == 0 {
            if self.dec {
                let b = self.src.get(self.ip).copied().unwrap_or(0);
                self.ip += 1;
                self.x = (self.x << 8) | b as u32;
            } else {
                self.out.push((self.x2 >> 24) as u8);
            }
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 255;
        }
        bit
    }
}

const NKB: usize = 12;
const NMQ: usize = 7;
const NRQ: usize = 8;
const NNZ: usize = 11;
const MAXE: usize = 12;
struct Model {
    nz: [[[Prob; 64]; NNZ]; 2],
    zero: [[[[Prob; NRQ]; NMQ]; NKB]; 2],
    ex: [[[[Prob; MAXE]; NMQ]; NKB]; 2],
    mt: [[[Prob; 2]; MAXE]; 2],
    sg: [[[Prob; 9]; NKB]; 2],
    dz: [[Prob; 8]; 2],
    dsg: [[Prob; 8]; 2],
    dex: [[[Prob; MAXE]; 8]; 2],
    dmt: [[[Prob; 2]; MAXE]; 2],
}
impl Model {
    fn new() -> Box<Self> {
        Box::new(Model {
            nz: [[[P_INIT; 64]; NNZ]; 2],
            zero: [[[[P_INIT; NRQ]; NMQ]; NKB]; 2],
            ex: [[[[P_INIT; MAXE]; NMQ]; NKB]; 2],
            mt: [[[P_INIT; 2]; MAXE]; 2],
            sg: [[[P_INIT; 9]; NKB]; 2],
            dz: [[P_INIT; 8]; 2],
            dsg: [[P_INIT; 8]; 2],
            dex: [[[P_INIT; MAXE]; 8]; 2],
            dmt: [[[P_INIT; 2]; MAXE]; 2],
        })
    }
}

const KB: [u8; 64] = [
    0, 0, 1, 2, 3, 4, 5, 5, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 9, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 11,
    11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11,
];
fn qmag(v: i32) -> usize {
    match v {
        0 => 0,
        1 => 1,
        2 => 2,
        3..=4 => 3,
        5..=8 => 4,
        9..=16 => 5,
        _ => 6,
    }
}
fn qnz(v: i32) -> usize {
    const T: [i32; NNZ - 1] = [0, 1, 2, 3, 4, 6, 9, 14, 22, 34];
    let mut c = 0;
    while c < NNZ - 1 && v > T[c] {
        c += 1;
    }
    c
}
/// Magnitude >= 1: exponent in unary + mantissa
fn code_mag(a: &mut Ac, ex: &mut [Prob; MAXE], mt: &mut [[Prob; 2]; MAXE], v: i32) -> i32 {
    let mut e = 0usize;
    if !a.dec {
        while (v >> (e + 1)) != 0 && e < MAXE - 1 {
            e += 1;
        }
    }
    let mut ee = 0usize;
    while ee < MAXE - 1 && a.bit(&mut ex[ee], ee < e) {
        ee += 1;
    }
    let e = ee;
    let mut r = 1i32;
    for b in (0..e).rev() {
        r = (r << 1) | a.bit(&mut mt[e][if b == e - 1 { 0 } else { 1 }], (v >> b) & 1 != 0) as i32;
    }
    r
}
fn nzcount(b: &[i16]) -> i32 {
    b[1..64].iter().filter(|&&v| v != 0).count() as i32
}
fn sign(v: i16) -> i32 {
    (v > 0) as i32 - (v < 0) as i32
}

/// Encode/decode all coefficients of a component (blocks in raster order).
fn code_comp(a: &mut Ac, m: &mut Model, c: &mut Comp, cls: usize) -> bool {
    for r in 0..c.bh {
        for col in 0..c.bw {
            let o = (r * c.bw + col) * 64;
            let (prev, cur) = c.coef.split_at_mut(o);
            let b = &mut cur[..64];
            let a_blk: Option<&[i16]> = (r > 0).then(|| &prev[o - c.bw * 64..o - c.bw * 64 + 64]);
            let l_blk: Option<&[i16]> = (col > 0).then(|| &prev[o - 64..o]);
            let al_blk: Option<&[i16]> = (r > 0 && col > 0).then(|| &prev[o - c.bw * 64 - 64..o - c.bw * 64]);
            // DC: prediction MED(A, L, AL)
            let pa = a_blk.map_or_else(|| l_blk.map_or(0, |l| l[0] as i32), |x| x[0] as i32);
            let pl = l_blk.map_or(pa, |l| l[0] as i32);
            let pal = al_blk.map_or(pa, |x| x[0] as i32);
            let (mx, mn) = (pa.max(pl), pa.min(pl));
            let pred = if pal >= mx {
                mn
            } else if pal <= mn {
                mx
            } else {
                pa + pl - pal
            };
            let dq = qmag((pa - pl).abs()).min(7);
            let mut dr = if a.dec { 0 } else { b[0] as i32 - pred };
            if a.bit(&mut m.dz[cls][dq], dr != 0) {
                let neg = a.bit(&mut m.dsg[cls][dq], dr < 0);
                let mag = code_mag(a, &mut m.dex[cls][dq], &mut m.dmt[cls], dr.abs());
                dr = if neg { -mag } else { mag };
            } else {
                dr = 0;
            }
            if a.dec {
                let v = pred + dr;
                if !(-32768..=32767).contains(&v) {
                    return false;
                }
                b[0] = v as i16;
            }
            // number of non-zero AC coefficients
            let na = a_blk.map_or(-1, nzcount);
            let nl = l_blk.map_or(-1, nzcount);
            let avg = if na < 0 {
                nl.max(0)
            } else if nl < 0 {
                na
            } else {
                (na + nl + 1) / 2
            };
            let nzq = qnz(avg);
            let nnz_in = if a.dec { 0 } else { nzcount(b) };
            let mut node = 1usize;
            for bit in (0..6).rev() {
                node = (node << 1) | a.bit(&mut m.nz[cls][nzq][node], (nnz_in >> bit) & 1 != 0) as usize;
            }
            let mut rem = node as i32 - 64;
            // AC in zigzag order
            let mut k = 1usize;
            while k < 64 && rem > 0 {
                let ma = a_blk.map_or(-1, |x| (x[k] as i32).abs());
                let ml = l_blk.map_or(-1, |x| (x[k] as i32).abs());
                let mag = if ma < 0 {
                    if ml < 0 {
                        0
                    } else {
                        2 * ml
                    }
                } else if ml < 0 {
                    2 * ma
                } else {
                    ma + ml
                };
                let (mq, kb) = (qmag(mag), KB[k] as usize);
                let rq = if rem > NRQ as i32 { NRQ - 1 } else { (rem - 1) as usize };
                if rem > 64 - k as i32 {
                    return false;
                }
                let v = if a.dec { 0 } else { b[k] as i32 };
                let isnz = if rem == 64 - k as i32 { true } else { a.bit(&mut m.zero[cls][kb][mq][rq], v != 0) };
                if !isnz {
                    if a.dec {
                        b[k] = 0;
                    }
                    k += 1;
                    continue;
                }
                let sa = a_blk.map_or(0, |x| sign(x[k]));
                let sl = l_blk.map_or(0, |x| sign(x[k]));
                let neg = a.bit(&mut m.sg[cls][kb][((sa + 1) * 3 + sl + 1) as usize], v < 0);
                let am = code_mag(a, &mut m.ex[cls][kb][mq], &mut m.mt[cls], v.abs());
                if am > 1023 {
                    return false;
                }
                if a.dec {
                    b[k] = if neg { -am } else { am } as i16;
                }
                rem -= 1;
                k += 1;
            }
            if rem != 0 {
                return false;
            }
        }
    }
    true
}

/// Transformed data or `None` (unsupported / not exactly reproducible).
pub fn forward(jpg: &[u8]) -> Option<Vec<u8>> {
    let n = jpg.len();
    let mut j = parse_header(jpg)?;
    let scan_end = decode_scan(jpg, &mut j)?;
    // EOI must follow the scan (the rest is copied unchanged)
    if scan_end + 2 > n || jpg[scan_end] != 0xFF || jpg[scan_end + 1] != 0xD9 {
        return None;
    }
    let mut m = Model::new();
    let mut a = Ac { x1: 0, x2: 0xFFFF_FFFF, x: 0, out: Vec::new(), src: &[], ip: 0, dec: false };
    for i in 0..j.nf {
        if !code_comp(&mut a, &mut m, &mut j.c[i], (i > 0) as usize) {
            return None;
        }
    }
    for _ in 0..4 {
        a.out.push((a.x1 >> 24) as u8);
        a.x1 <<= 8;
    }
    let (head, tail) = (j.data_start, n - scan_end);
    let mut t = Vec::with_capacity(16 + head + tail + a.out.len());
    t.extend_from_slice(b"JPGX");
    t.extend_from_slice(&(head as u32).to_le_bytes());
    t.extend_from_slice(&jpg[..head]);
    t.extend_from_slice(&(tail as u32).to_le_bytes());
    t.extend_from_slice(&jpg[scan_end..]);
    t.extend_from_slice(&(a.out.len() as u32).to_le_bytes());
    t.extend_from_slice(&a.out);
    // Safety net: check the complete reconstruction
    let mut chk = vec![0u8; n];
    (inverse(&t, &mut chk) && chk == jpg).then_some(t)
}

/// Reconstructs the original (`out.len()` = original size).
pub fn inverse(t: &[u8], out: &mut [u8]) -> bool {
    let tn = t.len();
    if tn < 16 || &t[..4] != b"JPGX" {
        return false;
    }
    let rd = |i: usize| u32::from_le_bytes([t[i], t[i + 1], t[i + 2], t[i + 3]]) as usize;
    let head = rd(4);
    if head > tn - 16 {
        return false;
    }
    let tail = rd(8 + head);
    if tail > tn - 16 - head {
        return false;
    }
    let mn = rd(12 + head + tail);
    if 16 + head + tail + mn != tn {
        return false;
    }
    let hd = &t[8..8 + head];
    let tl = &t[12 + head..12 + head + tail];
    let Some(mut j) = parse_header(hd) else { return false };
    if j.data_start != head {
        return false;
    }
    let mut m = Model::new();
    let mut a = Ac { x1: 0, x2: 0xFFFF_FFFF, x: 0, out: Vec::new(), src: &t[16 + head + tail..], ip: 0, dec: true };
    for _ in 0..4 {
        let b = a.src.get(a.ip).copied().unwrap_or(0);
        a.ip += 1;
        a.x = (a.x << 8) | b as u32;
    }
    for i in 0..j.nf {
        if !code_comp(&mut a, &mut m, &mut j.c[i], (i > 0) as usize) {
            return false;
        }
    }
    let Some(scan) = encode_scan(&j) else { return false };
    if head + scan.len() + tail != out.len() {
        return false;
    }
    out[..head].copy_from_slice(hd);
    out[head..head + scan.len()].copy_from_slice(&scan);
    out[head + scan.len()..].copy_from_slice(tl);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    /// Sample JPEGs from the C reference (tests/sample_jpegs.h, created with Pillow)
    use crate::testutil::jpeg_samples as samples;

    #[test]
    fn roundtrip_samples() {
        for s in samples() {
            let t = forward(&s).expect("sample JPEG transformable");
            let mut out = vec![0u8; s.len()];
            assert!(inverse(&t, &mut out));
            assert_eq!(out, s);
        }
    }

    #[test]
    fn corrupt_never_panics() {
        let mut r = Rng::new(5);
        for s in samples() {
            let t = forward(&s).unwrap();
            for it in 0..300 {
                let mut m = s.clone();
                for _ in 0..1 + r.next() % 4 {
                    let i = r.next() as usize % m.len();
                    m[i] ^= 1 + (r.next() % 255) as u8;
                }
                let cut = if it % 5 == 0 { r.next() as usize % m.len() } else { m.len() };
                if let Some(t2) = forward(&m[..cut]) {
                    let mut o = vec![0u8; cut];
                    assert!(inverse(&t2, &mut o) && o == m[..cut], "accepted, but reconstructed incorrectly");
                }
                let mut x = t.clone();
                for _ in 0..1 + r.next() % 4 {
                    let i = r.next() as usize % x.len();
                    x[i] ^= 1 + (r.next() % 255) as u8;
                }
                let mut o = vec![0u8; s.len()];
                let _ = inverse(&x[..if it % 5 == 1 { r.next() as usize % x.len() } else { x.len() }], &mut o);
            }
        }
    }
}
