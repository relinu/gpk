//! Inflate (RFC 1950/1951): decompress a complete zlib stream, in safe Rust.
//!
//! Rejects the same errors as zlib: wrong header, preset dictionary, over-subscribed or
//! (except for the special case of a single code) incomplete Huffman codes, invalid symbols,
//! distances before the start, wrong Adler-32 checksum, data behind the stream.

const FAST_BITS: u32 = 10;

/// Canonical Huffman decoder: fast table for short codes, otherwise the counting method.
struct Huff {
    /// Index = the next FAST_BITS bits; value = symbol << 4 | length (0 = not in the table)
    fast: Vec<u16>,
    count: [u16; 16],
    symbol: Vec<u16>,
}

#[derive(PartialEq)]
enum Kind {
    CodeLens,
    Lens,
    Dists,
}

impl Huff {
    fn new(lengths: &[u8], kind: Kind) -> Option<Huff> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        let max = (1..16).rev().find(|&l| count[l] != 0).unwrap_or(0);
        let mut h = Huff { fast: vec![0; 1 << FAST_BITS], count, symbol: vec![0; lengths.len()] };
        if max == 0 {
            // no codes: only allowed as long as nothing is decoded (like zlib)
            return if kind == Kind::CodeLens { None } else { Some(h) };
        }
        let mut left: i32 = 1;
        for len in 1..16 {
            left <<= 1;
            left -= count[len] as i32;
            if left < 0 {
                return None; // over-subscribed
            }
        }
        if left > 0 && (kind == Kind::CodeLens || max != 1) {
            return None; // incomplete
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + count[len];
        }
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                h.symbol[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        // fast table: enter the canonical codes (MSB first) bit-reversed
        let mut code = 0u32;
        let mut idx = 0usize;
        for len in 1..=FAST_BITS as usize {
            for _ in 0..count[len] {
                let sym = h.symbol[idx];
                idx += 1;
                let mut rev = 0u32;
                for b in 0..len {
                    rev |= ((code >> b) & 1) << (len - 1 - b);
                }
                let mut i = rev as usize;
                while i < 1 << FAST_BITS {
                    h.fast[i] = (sym << 4) | len as u16;
                    i += 1 << len;
                }
                code += 1;
            }
            code <<= 1;
        }
        Some(h)
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    n: u32,
}
impl<'a> Bits<'a> {
    #[inline]
    fn refill(&mut self) {
        // Fast path: 8 bytes at once. Bits above n then already hold the following bytes;
        // a later refill ORs the same values into the same position.
        if self.pos + 8 <= self.data.len() {
            let v = u64::from_le_bytes(self.data[self.pos..self.pos + 8].try_into().unwrap());
            self.buf |= v << self.n;
            self.pos += ((63 - self.n) >> 3) as usize;
            self.n |= 56;
            return;
        }
        while self.n <= 56 {
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.buf |= (b as u64) << self.n;
            self.n += 8;
            self.pos += 1;
        }
    }
    #[inline]
    fn get(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        if self.n < k {
            self.refill();
        }
        let v = (self.buf & ((1u64 << k) - 1)) as u32;
        self.buf >>= k;
        self.n -= k;
        v
    }
    /// Consumed bytes (including a partially used byte); > data.len() means: input too short.
    fn consumed(&self) -> usize {
        self.pos - (self.n / 8) as usize
    }
    fn overrun(&self) -> bool {
        self.consumed() > self.data.len()
    }
    fn align(&mut self) {
        let drop = self.n % 8;
        self.buf >>= drop;
        self.n -= drop;
    }
    #[inline]
    fn decode(&mut self, h: &Huff) -> Option<usize> {
        if self.n < 15 {
            self.refill();
        }
        let e = h.fast[(self.buf & ((1 << FAST_BITS) - 1)) as usize];
        if e != 0 {
            let len = (e & 15) as u32;
            self.buf >>= len;
            self.n -= len;
            return Some((e >> 4) as usize);
        }
        // slow path (codes longer than FAST_BITS or invalid)
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= (self.buf & 1) as i32;
            self.buf >>= 1;
            self.n -= 1;
            let count = h.count[len] as i32;
            if code - count < first {
                return Some(h.symbol[(index + (code - first)) as usize] as usize);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LBASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385,
    24577,
];
const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

fn codes(b: &mut Bits, out: &mut Vec<u8>, lit: &Huff, dist: &Huff, max_out: usize) -> Option<()> {
    loop {
        let sym = b.decode(lit)?;
        if sym < 256 {
            if out.len() >= max_out {
                return None;
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Some(());
        } else {
            let s = sym - 257;
            if s >= 29 {
                return None;
            }
            let len = LBASE[s] as usize + b.get(LEXT[s] as u32) as usize;
            let ds = b.decode(dist)?;
            if ds >= 30 {
                return None;
            }
            let d = DBASE[ds] as usize + b.get(DEXT[ds] as u32) as usize;
            if d > out.len() || out.len() + len > max_out {
                return None;
            }
            let start = out.len() - d;
            if d >= len {
                out.extend_from_within(start..start + len);
            } else {
                for i in 0..len {
                    let c = out[start + i];
                    out.push(c);
                }
            }
        }
        if b.overrun() {
            return None;
        }
    }
}

/// Decompresses a complete zlib stream. `None` on any error, if not exactly all input bytes
/// are consumed, or if the output exceeds `max_out`.
pub fn inflate_zlib(zs: &[u8], max_out: usize) -> Option<Vec<u8>> {
    if zs.len() < 6 {
        return None;
    }
    let (cmf, flg) = (zs[0], zs[1]);
    if cmf & 15 != 8 || cmf >> 4 > 7 || ((cmf as u32) << 8 | flg as u32) % 31 != 0 || flg & 0x20 != 0 {
        return None;
    }
    let mut b = Bits { data: &zs[2..], pos: 0, buf: 0, n: 0 };
    let mut out = Vec::with_capacity(zs.len().saturating_mul(4).min(max_out));
    loop {
        let last = b.get(1);
        match b.get(2) {
            0 => {
                b.align();
                let len = b.get(16);
                let nlen = b.get(16);
                if len != !nlen & 0xFFFF || out.len() + len as usize > max_out {
                    return None;
                }
                for _ in 0..len {
                    out.push(b.get(8) as u8);
                }
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                let lit = Huff::new(&l, Kind::Lens)?;
                // 32 codes of length 5; the symbols 30 and 31 are invalid (rejected in `codes`)
                let dist = Huff::new(&[5u8; 32], Kind::Dists)?;
                codes(&mut b, &mut out, &lit, &dist, max_out)?;
            }
            2 => {
                let nlen = b.get(5) as usize + 257;
                let ndist = b.get(5) as usize + 1;
                let ncode = b.get(4) as usize + 4;
                if nlen > 286 || ndist > 30 {
                    return None;
                }
                let mut cl = [0u8; 19];
                for &o in CL_ORDER.iter().take(ncode) {
                    cl[o] = b.get(3) as u8;
                }
                let clh = Huff::new(&cl, Kind::CodeLens)?;
                let mut lens = [0u8; 286 + 30];
                let mut i = 0;
                while i < nlen + ndist {
                    let sym = b.decode(&clh)?;
                    let (val, rep) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => {
                            if i == 0 {
                                return None;
                            }
                            (lens[i - 1], 3 + b.get(2) as usize)
                        }
                        17 => (0, 3 + b.get(3) as usize),
                        _ => (0, 11 + b.get(7) as usize),
                    };
                    if i + rep > nlen + ndist {
                        return None;
                    }
                    lens[i..i + rep].fill(val);
                    i += rep;
                    if b.overrun() {
                        return None;
                    }
                }
                if lens[256] == 0 {
                    return None; // no end-of-block code
                }
                let lit = Huff::new(&lens[..nlen], Kind::Lens)?;
                let dist = Huff::new(&lens[nlen..nlen + ndist], Kind::Dists)?;
                codes(&mut b, &mut out, &lit, &dist, max_out)?;
            }
            _ => return None,
        }
        if b.overrun() {
            return None;
        }
        if last == 1 {
            break;
        }
    }
    // The checksum follows big-endian directly after the stream (padded to a byte boundary)
    b.align();
    let end = 2 + b.consumed();
    if b.overrun() || end + 4 != zs.len() {
        return None;
    }
    let want = u32::from_be_bytes(zs[end..end + 4].try_into().ok()?);
    if crate::deflate::adler32(&out) != want {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deflate::deflate_with;
    use crate::testutil::Rng;

    #[test]
    fn roundtrip_with_own_deflate() {
        let mut r = Rng::new(3);
        let text = "The dragon guards the sword. ".repeat(500).into_bytes();
        for d in [Vec::new(), b"a".to_vec(), text, r.bytes(70_000), vec![0u8; 200_000]] {
            for (lvl, mem, st) in [(1, 8, 0), (6, 8, 0), (9, 9, 1), (4, 1, 0)] {
                let z = deflate_with(&d, lvl, mem, st).unwrap();
                assert_eq!(inflate_zlib(&z, 1 << 22).unwrap(), d);
            }
        }
    }
    #[test]
    fn corrupt_and_limits() {
        let d = "abc abc abc xyz ".repeat(300).into_bytes();
        let z = deflate_with(&d, 6, 8, 0).unwrap();
        assert!(inflate_zlib(&z, d.len() - 1).is_none(), "max_out exceeded");
        let mut long = z.clone();
        long.push(0);
        assert!(inflate_zlib(&long, 1 << 20).is_none(), "data behind the stream");
        assert!(inflate_zlib(&z[..z.len() - 1], 1 << 20).is_none(), "truncated");
        let mut r = Rng::new(4);
        for _ in 0..2000 {
            let mut m = z.clone();
            let i = r.next() as usize % m.len();
            m[i] ^= 1 << (r.next() % 8);
            if let Some(o) = inflate_zlib(&m, 1 << 20) {
                assert_eq!(o, d, "corrupted stream accepted with wrong content");
            }
        }
    }
}
