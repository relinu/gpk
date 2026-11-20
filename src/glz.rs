//! GLZ – schneller blockweiser LZ77+Huffman-Kompressor (Port von `legacy-c/glz.c`).
//!
//! GPK nutzt GLZ als schnellen Schätzer („ist das schon komprimiert?“).
//! Format und Ausgabe sind bytegleich zur C-Referenz.
//!
//! Containerformat: `"GLZ1" | u64 Originalgröße | Blöcke`;
//! Block: `u8 filter | u8 stride | u8 mode (0=roh, 1=LZ) | u32 roh | u32 payload | payload`.

use crate::filters::{self, FSpec, FT_DELTA, FT_NONE, FT_SHUF, FT_SHUFDELTA};

const BLOCK_SIZE: usize = 1 << 20;
const MINMATCH: u32 = 4;
const HUF_MAXLEN: usize = 11;
const HUF_TABSIZE: usize = 1 << HUF_MAXLEN;
const HASH_BITS: u32 = 17;
const NIL: u32 = u32::MAX;
const BLK_HDR: usize = 11;
const FILE_HDR: usize = 12;
const TRIAL_BYTES: usize = 256 * 1024;

#[inline]
fn rd32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
#[inline]
fn bitlen32(v: u32) -> u32 {
    31 - v.leading_zeros()
}

// ---------------- Bit-I/O (LSB zuerst) ----------------
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}
impl BitWriter {
    fn new() -> Self {
        BitWriter { out: Vec::new(), acc: 0, n: 0 }
    }
    #[inline]
    fn put(&mut self, v: u32, nb: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += nb;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    fn flush(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

struct BitReader<'a> {
    d: &'a [u8],
    p: usize,
    buf: u64,
    n: u32,
}
impl<'a> BitReader<'a> {
    fn new(d: &'a [u8]) -> Self {
        BitReader { d, p: 0, buf: 0, n: 0 }
    }
    #[inline]
    fn refill(&mut self) {
        if self.d.len() - self.p >= 8 {
            let v = u64::from_le_bytes(self.d[self.p..self.p + 8].try_into().unwrap());
            self.buf |= v << self.n;
            self.p += ((63 - self.n) >> 3) as usize;
            self.n |= 56;
        } else {
            while self.n <= 56 {
                let b = if self.p < self.d.len() {
                    self.p += 1;
                    self.d[self.p - 1]
                } else {
                    0
                };
                self.buf |= (b as u64) << self.n;
                self.n += 8;
            }
        }
    }
    #[inline]
    fn bits(&mut self, nb: u32) -> u32 {
        let v = (self.buf & ((1u64 << nb) - 1)) as u32;
        self.buf >>= nb;
        self.n -= nb;
        v
    }
}

// ---------------- Kanonisches Huffman, max. 11 Bit ----------------
fn huf_build_lengths(freq_in: &[u32; 256], len: &mut [u8; 256]) {
    let mut f = *freq_in;
    loop {
        let mut w = [0u64; 512];
        let mut parent = [0usize; 512];
        let mut sym = [0usize; 256];
        let mut m = 0;
        for s in 0..256 {
            if f[s] != 0 {
                sym[m] = s;
                w[m] = f[s] as u64;
                m += 1;
            }
        }
        *len = [0; 256];
        if m == 0 {
            return;
        }
        if m == 1 {
            len[sym[0]] = 1;
            return;
        }
        let mut alive = [0usize; 512];
        let mut na = m;
        for (i, a) in alive.iter_mut().enumerate().take(m) {
            *a = i;
        }
        let mut nn = m;
        while na > 1 {
            let (mut a, mut b) = (0usize, 1usize);
            if w[alive[b]] < w[alive[a]] {
                a = 1;
                b = 0;
            }
            for i in 2..na {
                if w[alive[i]] < w[alive[a]] {
                    b = a;
                    a = i;
                } else if w[alive[i]] < w[alive[b]] {
                    b = i;
                }
            }
            let (x, y) = (alive[a], alive[b]);
            w[nn] = w[x] + w[y];
            parent[x] = nn;
            parent[y] = nn;
            let (hi, lo) = if a > b { (a, b) } else { (b, a) };
            na -= 1;
            alive[hi] = alive[na];
            alive[lo] = nn;
            nn += 1;
        }
        let mut depth = [0u32; 512];
        let mut maxl = 0;
        for i in (0..nn - 1).rev() {
            depth[i] = depth[parent[i]] + 1;
        }
        for i in 0..m {
            len[sym[i]] = depth[i] as u8;
            maxl = maxl.max(depth[i]);
        }
        if maxl as usize <= HUF_MAXLEN {
            return;
        }
        for v in f.iter_mut() {
            if *v != 0 {
                *v = (*v + 1) >> 1; // abflachen, neu bauen
            }
        }
    }
}

fn rev_bits(mut v: u32, n: u32) -> u32 {
    let mut r = 0;
    for _ in 0..n {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    r
}

/// Kanonische Codes (bitgespiegelt für LSB-zuerst); `None` bei verletzter Kraft-Ungleichung.
fn huf_codes(len: &[u8; 256]) -> Option<[u32; 256]> {
    let mut cnt = [0u32; 16];
    for &l in len.iter() {
        cnt[l as usize & 15] += 1;
    }
    cnt[0] = 0;
    let mut kraft = 0u32;
    for (b, &c) in cnt.iter().enumerate().take(HUF_MAXLEN + 1).skip(1) {
        kraft += c << (HUF_MAXLEN - b);
    }
    if kraft > HUF_TABSIZE as u32 {
        return None;
    }
    let mut next = [0u32; 16];
    let mut c = 0u32;
    for b in 1..16 {
        c = (c + cnt[b - 1]) << 1;
        next[b] = c;
    }
    let mut code = [0u32; 256];
    for s in 0..256 {
        let l = len[s] as usize;
        if l != 0 {
            code[s] = rev_bits(next[l], l as u32);
            next[l] += 1;
        }
    }
    Some(code)
}

/// Stream: `mode | u32 count | ...` mit mode 0 = roh, 1 = RLE, 2 = Huffman.
fn huf_encode(src: &[u8], dst: &mut Vec<u8>) {
    let n = src.len() as u32;
    let mut freq = [0u32; 256];
    for &b in src {
        freq[b as usize] += 1;
    }
    let used = freq.iter().filter(|&&f| f != 0).count();
    if used == 1 {
        dst.push(1);
        dst.extend_from_slice(&n.to_le_bytes());
        dst.push(src[0]);
        return;
    }
    if n >= 64 && used > 1 {
        let mut len = [0u8; 256];
        huf_build_lengths(&freq, &mut len);
        let bits: u64 = (0..256).map(|s| freq[s] as u64 * len[s] as u64).sum();
        let hsize = 1 + 4 + 4 + 128 + bits.div_ceil(8) as usize;
        if hsize < 5 + n as usize {
            if let Some(code) = huf_codes(&len) {
                dst.push(2);
                dst.extend_from_slice(&n.to_le_bytes());
                let mut bw = BitWriter::new();
                for &b in src {
                    bw.put(code[b as usize], len[b as usize] as u32);
                }
                let body = bw.flush();
                dst.extend_from_slice(&(body.len() as u32).to_le_bytes());
                for s in (0..256).step_by(2) {
                    dst.push(len[s] | (len[s + 1] << 4));
                }
                dst.extend_from_slice(&body);
                return;
            }
        }
    }
    dst.push(0);
    dst.extend_from_slice(&n.to_le_bytes());
    dst.extend_from_slice(src);
}

/// Dekodiert einen Stream ab `p`; liefert (Daten, neue Position).
fn huf_decode(d: &[u8], mut p: usize, maxn: usize) -> Option<(Vec<u8>, usize)> {
    if d.len() < p + 5 {
        return None;
    }
    let mode = d[p];
    let n = rd32(d, p + 1) as usize;
    p += 5;
    if n > maxn {
        return None;
    }
    match mode {
        0 => {
            let s = d.get(p..p + n)?;
            Some((s.to_vec(), p + n))
        }
        1 => {
            let b = *d.get(p)?;
            Some((vec![b; n], p + 1))
        }
        2 => {
            if d.len() < p + 4 + 128 {
                return None;
            }
            let nb = rd32(d, p) as usize;
            p += 4;
            let mut len = [0u8; 256];
            for s in (0..256).step_by(2) {
                len[s] = d[p + s / 2] & 15;
                len[s + 1] = d[p + s / 2] >> 4;
            }
            p += 128;
            let body = d.get(p..p + nb)?;
            if len.iter().any(|&l| l as usize > HUF_MAXLEN) {
                return None;
            }
            let code = huf_codes(&len)?;
            let mut tab = vec![1u16; HUF_TABSIZE]; // Lücken (korrupte Daten): harmlos
            for s in 0..256 {
                let l = len[s] as usize;
                if l == 0 {
                    continue;
                }
                let e = ((s as u16) << 4) | l as u16;
                let mut j = code[s] as usize;
                while j < HUF_TABSIZE {
                    tab[j] = e;
                    j += 1 << l;
                }
            }
            let mut out = vec![0u8; n];
            let mut br = BitReader::new(body);
            let mut i = 0;
            while i < n {
                br.refill();
                for _ in 0..4.min(n - i) {
                    let e = tab[(br.buf & (HUF_TABSIZE as u64 - 1)) as usize];
                    out[i] = (e >> 4) as u8;
                    i += 1;
                    let l = (e & 15) as u32;
                    br.buf >>= l;
                    br.n -= l;
                }
            }
            Some((out, p + nb))
        }
        _ => None,
    }
}

// ---------------- Entropie-Hilfen (nur Encoder) ----------------
fn delta_entropy(p: &[u8], s: usize) -> f64 {
    let mut h = [0u32; 256];
    for i in s..p.len() {
        h[p[i].wrapping_sub(p[i - s]) as usize] += 1;
    }
    let tot = (p.len() - s) as f64;
    let mut e = 0.0;
    for &c in h.iter() {
        if c != 0 {
            e -= c as f64 * (c as f64 / tot).log2();
        }
    }
    e
}
fn order0_entropy(p: &[u8]) -> f64 {
    let mut h = [0u32; 256];
    for &b in p {
        h[b as usize] += 1;
    }
    let mut e = 0.0;
    for &c in h.iter() {
        if c != 0 {
            e -= c as f64 * (c as f64 / p.len() as f64).log2();
        }
    }
    e
}

// ---------------- LZ77-Encoder ----------------
#[derive(Clone, Copy)]
struct LevelParams {
    depth: i32,
    lazy: bool,
    nice: u32,
}
const LEVELS: [LevelParams; 10] = [
    LevelParams { depth: 4, lazy: false, nice: 32 },
    LevelParams { depth: 4, lazy: false, nice: 32 },
    LevelParams { depth: 8, lazy: false, nice: 32 },
    LevelParams { depth: 16, lazy: true, nice: 64 },
    LevelParams { depth: 32, lazy: true, nice: 128 },
    LevelParams { depth: 64, lazy: true, nice: 128 },
    LevelParams { depth: 128, lazy: true, nice: 256 },
    LevelParams { depth: 256, lazy: true, nice: 512 },
    LevelParams { depth: 512, lazy: true, nice: 1024 },
    LevelParams { depth: 1024, lazy: true, nice: 4096 },
];

#[derive(Clone, Copy, Default)]
struct Match {
    len: u32,
    off: u32,
    gain: i32,
}

struct Enc {
    head: Vec<u32>,
    prev: Vec<u32>,
    rep0: u32,
    rep1: u32,
    litcost: i32,
}

#[inline]
fn match_len(d: &[u8], a: usize, b: usize) -> u32 {
    // Anzahl gleicher Bytes ab a und b (b < a), bis zum Ende von d
    let n = d.len() - a;
    let (x, y) = (&d[a..], &d[b..b + n]);
    let mut i = 0;
    while i + 8 <= n {
        let v = u64::from_le_bytes(x[i..i + 8].try_into().unwrap()) ^ u64::from_le_bytes(y[i..i + 8].try_into().unwrap());
        if v != 0 {
            return (i + (v.trailing_zeros() >> 3) as usize) as u32;
        }
        i += 8;
    }
    while i < n && x[i] == y[i] {
        i += 1;
    }
    i as u32
}
#[inline]
fn match_gain(len: u32, off: u32, rep0: u32, rep1: u32, litcost: i32) -> i32 {
    let cost = if off == rep0 || off == rep1 { 4 * 16 } else { (9 + bitlen32(off) as i32) * 16 };
    len as i32 * litcost - cost
}
#[inline]
fn hash4(d: &[u8], i: usize) -> usize {
    (rd32(d, i).wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
}

impl Enc {
    fn new() -> Self {
        Enc { head: vec![NIL; 1 << HASH_BITS], prev: vec![0; BLOCK_SIZE], rep0: 1, rep1: 4, litcost: 0 }
    }
    fn find_match(&self, src: &[u8], ip: u32, lp: &LevelParams) -> Match {
        let mut best = Match::default();
        let n = src.len() as u32;
        for off in [self.rep0, self.rep1] {
            if off > ip {
                continue;
            }
            let l = match_len(src, ip as usize, (ip - off) as usize);
            if l >= MINMATCH {
                let g = match_gain(l, off, self.rep0, self.rep1, self.litcost);
                if g > best.gain {
                    best = Match { len: l, off, gain: g };
                }
            }
        }
        let mut cand = self.head[hash4(src, ip as usize)];
        let mut depth = lp.depth;
        let avail = n - ip;
        while cand != NIL && depth > 0 {
            depth -= 1;
            let (c, cur) = (cand as usize, ip as usize);
            if best.len < avail && src[c + best.len as usize] == src[cur + best.len as usize] {
                let l = match_len(src, cur, c);
                if l >= MINMATCH {
                    let off = ip - cand;
                    let g = match_gain(l, off, self.rep0, self.rep1, self.litcost);
                    if g > best.gain {
                        best = Match { len: l, off, gain: g };
                        if l >= lp.nice {
                            break;
                        }
                    }
                }
            }
            cand = self.prev[c];
        }
        best
    }

    fn insert_upto(&mut self, src: &[u8], ins: &mut u32, to: u32) {
        while *ins < to {
            let h = hash4(src, *ins as usize);
            self.prev[*ins as usize] = self.head[h];
            self.head[h] = *ins;
            *ins += 1;
        }
    }

    /// Komprimiert einen (gefilterten) Block zur LZ-Payload.
    fn lz_block(&mut self, src: &[u8], level: usize) -> Vec<u8> {
        let lp = LEVELS[level];
        let n = src.len() as u32;
        self.head.fill(NIL);
        self.rep0 = 1;
        self.rep1 = 4;
        let en = if n > 16 { order0_entropy(src) } else { 8.0 * n as f64 };
        self.litcost = ((16.0 * (en / (n.max(1)) as f64 + 0.5)) as i32).clamp(16, 8 * 16);

        let (mut lit, mut tok, mut ofc, mut ext) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut ob = BitWriter::new();
        let put_varint = |ext: &mut Vec<u8>, mut v: u32| {
            while v >= 128 {
                ext.push((v | 128) as u8);
                v >>= 7;
            }
            ext.push(v as u8);
        };
        let (mut ip, mut anchor, mut ins) = (0u32, 0u32, 0u32);
        if n > MINMATCH {
            let limit = n - MINMATCH;
            while ip <= limit {
                self.insert_upto(src, &mut ins, ip);
                let mut m = self.find_match(src, ip, &lp);
                if m.gain <= 0 {
                    ip += 1;
                    continue;
                }
                while lp.lazy && ip < limit {
                    self.insert_upto(src, &mut ins, ip + 1);
                    let m2 = self.find_match(src, ip + 1, &lp);
                    if m2.gain - self.litcost > m.gain {
                        ip += 1;
                        m = m2;
                    } else {
                        break;
                    }
                }
                // Sequenz ausgeben
                let ll = ip - anchor;
                lit.extend_from_slice(&src[anchor as usize..ip as usize]);
                let mm = m.len - MINMATCH;
                tok.push(((ll.min(15) << 4) | mm.min(15)) as u8);
                if ll >= 15 {
                    put_varint(&mut ext, ll - 15);
                }
                if mm >= 15 {
                    put_varint(&mut ext, mm - 15);
                }
                let oc = if m.off == self.rep0 {
                    0
                } else if m.off == self.rep1 {
                    self.rep1 = self.rep0;
                    self.rep0 = m.off;
                    1
                } else {
                    let b = bitlen32(m.off);
                    ob.put(m.off - (1 << b), b);
                    self.rep1 = self.rep0;
                    self.rep0 = m.off;
                    2 + b as u8
                };
                ofc.push(oc);
                ip += m.len;
                anchor = ip;
            }
        }
        lit.extend_from_slice(&src[anchor as usize..]);
        let obits = ob.flush();
        let mut out = Vec::with_capacity(src.len() / 2 + 64);
        out.extend_from_slice(&(tok.len() as u32).to_le_bytes());
        huf_encode(&lit, &mut out);
        huf_encode(&tok, &mut out);
        huf_encode(&ofc, &mut out);
        out.extend_from_slice(&(ext.len() as u32).to_le_bytes());
        out.extend_from_slice(&ext);
        out.extend_from_slice(&(obits.len() as u32).to_le_bytes());
        out.extend_from_slice(&obits);
        out
    }

    /// Stride-Erkennung + Probekompression: besten Filter wählen.
    fn choose_filter(&mut self, src: &[u8]) -> (u8, u8) {
        let n = src.len();
        if n < 4096 {
            return (FT_NONE, 0);
        }
        let sample = &src[..n.min(65536)];
        let (mut best_s, mut best_e) = (1usize, 1e300f64);
        for s in 1..=32 {
            let e = delta_entropy(sample, s);
            if e < best_e * 0.995 {
                best_e = e;
                best_s = s;
            }
        }
        let bs = best_s as u8;
        let mut cand = vec![(FT_NONE, 0u8), (FT_DELTA, bs)];
        if best_s > 1 {
            cand.push((FT_SHUFDELTA, bs));
            cand.push((FT_SHUF, bs));
        }
        if best_s > 4 && best_s % 4 == 0 {
            cand.push((FT_SHUFDELTA, 4));
        }
        if best_s != 1 {
            cand.push((FT_DELTA, 1));
        }
        let trial = &src[..n.min(TRIAL_BYTES)];
        let mut best = usize::MAX;
        let mut res = (FT_NONE, 0);
        let mut buf = vec![0u8; trial.len()];
        for (f, s) in cand {
            filters::encode(&FSpec::with_stride(f, s), trial, &mut buf, 0);
            let sz = self.lz_block(&buf, 2).len();
            if sz < best {
                best = sz;
                res = (f, s);
            }
        }
        res
    }
}

pub fn bound(n: usize) -> usize {
    FILE_HDR + n + (n / BLOCK_SIZE + 1) * BLK_HDR + 64
}

/// Komprimiert `src` (Stufe 1..9). `no_filter`: Vorfilter-Auswahl abschalten.
pub fn compress(src: &[u8], level: i32, no_filter: bool) -> Vec<u8> {
    let level = level.clamp(1, 9) as usize;
    let mut out = Vec::with_capacity(src.len() / 2 + 64);
    out.extend_from_slice(b"GLZ1");
    out.extend_from_slice(&(src.len() as u64).to_le_bytes());
    let mut e = Enc::new();
    let mut filtered = Vec::new();
    for b in src.chunks(BLOCK_SIZE) {
        let (f, s) = if no_filter { (FT_NONE, 0) } else { e.choose_filter(b) };
        filtered.resize(b.len(), 0);
        filters::encode(&FSpec::with_stride(f, s), b, &mut filtered, 0);
        let k = e.lz_block(&filtered, level);
        if !k.is_empty() && k.len() < b.len() {
            out.extend_from_slice(&[f, s, 1]);
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(&(k.len() as u32).to_le_bytes());
            out.extend_from_slice(&k);
        } else {
            out.extend_from_slice(&[0, 0, 0]);
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
    }
    out
}

fn rd_varint(d: &[u8], p: &mut usize) -> Option<u32> {
    let (mut r, mut sh) = (0u32, 0u32);
    while *p < d.len() {
        let c = d[*p];
        *p += 1;
        r |= ((c & 127) as u32) << sh;
        if c & 128 == 0 {
            return Some(r);
        }
        sh += 7;
        if sh > 28 {
            return None;
        }
    }
    None
}

fn lz_decode_block(d: &[u8], out: &mut [u8]) -> Option<()> {
    let rawsize = out.len();
    if d.len() < 4 {
        return None;
    }
    let nseq = rd32(d, 0) as usize;
    if nseq > rawsize / MINMATCH as usize + 1 {
        return None;
    }
    let (lit, p) = huf_decode(d, 4, rawsize)?;
    let (tok, p) = huf_decode(d, p, nseq)?;
    let (ofc, p) = huf_decode(d, p, nseq)?;
    if tok.len() != nseq || ofc.len() != nseq || d.len() < p + 4 {
        return None;
    }
    let extlen = rd32(d, p) as usize;
    let ext = d.get(p + 4..p + 4 + extlen)?;
    let p = p + 4 + extlen;
    if d.len() < p + 4 {
        return None;
    }
    let oblen = rd32(d, p) as usize;
    let obits = d.get(p + 4..p + 4 + oblen)?;
    let mut ob = BitReader::new(obits);
    let (mut lp, mut op, mut ep) = (0usize, 0usize, 0usize);
    let (mut rep0, mut rep1) = (1usize, 4usize);
    for i in 0..nseq {
        let t = tok[i] as usize;
        let mut ll = t >> 4;
        if ll == 15 {
            ll += rd_varint(ext, &mut ep)? as usize;
        }
        if ll > lit.len() - lp || ll > rawsize - op {
            return None;
        }
        out[op..op + ll].copy_from_slice(&lit[lp..lp + ll]);
        op += ll;
        lp += ll;
        let mut ml = t & 15;
        if ml == 15 {
            ml += rd_varint(ext, &mut ep)? as usize;
        }
        ml += MINMATCH as usize;
        let oc = ofc[i] as u32;
        let off = match oc {
            0 => rep0,
            1 => {
                let o = rep1;
                rep1 = rep0;
                rep0 = o;
                o
            }
            _ => {
                let b = oc - 2;
                if b > 24 {
                    return None;
                }
                ob.refill();
                let o = (1usize << b) + ob.bits(b) as usize;
                rep1 = rep0;
                rep0 = o;
                o
            }
        };
        if off > op || ml > rawsize - op {
            return None;
        }
        if off >= ml {
            out.copy_within(op - off..op - off + ml, op);
        } else {
            for k in 0..ml {
                out[op + k] = out[op - off + k];
            }
        }
        op += ml;
    }
    let rest = rawsize - op;
    if rest != lit.len() - lp {
        return None;
    }
    out[op..].copy_from_slice(&lit[lp..]);
    Some(())
}

pub fn decompressed_size(src: &[u8]) -> Option<usize> {
    if src.len() < FILE_HDR || &src[..4] != b"GLZ1" {
        return None;
    }
    usize::try_from(u64::from_le_bytes(src[4..12].try_into().unwrap())).ok()
}

/// Dekomprimiert; `None` bei ungültigen Daten oder `max_out` überschritten.
pub fn decompress(src: &[u8], max_out: usize) -> Option<Vec<u8>> {
    let total = decompressed_size(src)?;
    if total > max_out {
        return None;
    }
    let mut out = vec![0u8; total];
    let mut p = FILE_HDR;
    let mut op = 0;
    let mut tmp = Vec::new();
    while op < total {
        let h = src.get(p..p + BLK_HDR)?;
        let (f, s, mode) = (h[0], h[1], h[2]);
        let raw = rd32(h, 3) as usize;
        let plen = rd32(h, 7) as usize;
        p += BLK_HDR;
        if raw == 0 || raw > BLOCK_SIZE || raw > total - op {
            return None;
        }
        let payload = src.get(p..p + plen)?;
        if f > FT_SHUFDELTA || (f != FT_NONE && !(1..=32).contains(&s)) {
            return None;
        }
        let dst = &mut out[op..op + raw];
        match mode {
            0 => {
                if plen != raw {
                    return None;
                }
                dst.copy_from_slice(payload);
            }
            1 => {
                if f == FT_NONE {
                    lz_decode_block(payload, dst)?;
                } else {
                    tmp.resize(raw, 0);
                    lz_decode_block(payload, &mut tmp)?;
                    filters::decode(&FSpec::with_stride(f, s), &mut tmp, dst, 0);
                }
            }
            _ => return None,
        }
        p += plen;
        op += raw;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Rng;

    fn sample(r: &mut Rng, n: usize) -> Vec<u8> {
        let words = [b"Drache ".as_slice(), b"Schwert ", b"{\"id\": 42}, ", b"\x00\x00\x80\x3f"];
        let mut v = Vec::with_capacity(n);
        while v.len() < n {
            if r.next() % 4 == 0 {
                v.push(r.next() as u8);
            } else {
                v.extend_from_slice(words[(r.next() % 4) as usize]);
            }
        }
        v.truncate(n);
        v
    }

    #[test]
    fn roundtrip() {
        let mut r = Rng::new(3);
        for &(n, lvl, nf) in &[(0, 1, true), (5, 1, true), (5000, 2, true), (300_000, 5, false), (1_500_000, 1, true)] {
            let d = sample(&mut r, n);
            let c = compress(&d, lvl, nf);
            assert_eq!(decompress(&c, usize::MAX).unwrap(), d);
        }
    }

    #[test]
    fn corrupt_never_panics() {
        let mut r = Rng::new(4);
        let d = sample(&mut r, 200_000);
        let c = compress(&d, 3, false);
        for _ in 0..500 {
            let mut m = c.clone();
            for _ in 0..1 + r.next() % 6 {
                let i = r.next() as usize % m.len();
                m[i] ^= 1 + (r.next() % 255) as u8;
            }
            let cut = if r.next() % 5 == 0 { r.next() as usize % m.len() } else { m.len() };
            let _ = decompress(&m[..cut], 1 << 24);
        }
    }
}
