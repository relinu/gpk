//! Deflate (RFC 1951) with zlib header, **bit-exact with zlib 1.4** (`deflate.c`, `trees.c`).
//!
//! Needed for PNG recompression: a stream produced by zlib can only be recreated bit-exactly
//! with exactly the same algorithm. Covered are the parameters that zlib-based PNG encoders
//! use: levels 1–9, memLevel 1–9, window 32 KiB, strategy "default" (0) and "filtered" (1).
//!
//! The structure deliberately follows zlib closely (same names, same order of operations),
//! including the quirks that affect the output:
//! - `longest_match` does not compare byte 2 (implied by equal hashes when `hash_bits >= 8`),
//! - matches may be compared beyond the end of the data (the length is clamped afterwards);
//!   therefore the window contents (sliding, zeros behind the data) are reproduced exactly too,
//! - in zlib, frequency/code and parent/length each share one field (`union`); same here.
//!
//! With [`matches`] the output is compared continuously with an expected stream and aborted at
//! the first differing block – this makes the parameter search for PNGs several times faster.

use std::sync::OnceLock;

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const W_BITS: usize = 15;
const W_SIZE: usize = 1 << W_BITS;
const W_MASK: usize = W_SIZE - 1;
const WINDOW_SIZE: usize = 2 * W_SIZE;
const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;
const WIN_INIT: usize = MAX_MATCH;
const TOO_FAR: usize = 4096;
const NIL: u16 = 0;

const LENGTH_CODES: usize = 29;
const LITERALS: usize = 256;
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
const D_CODES: usize = 30;
const BL_CODES: usize = 19;
const HEAP_SIZE: usize = 2 * L_CODES + 1;
const MAX_BITS: usize = 15;
const MAX_BL_BITS: usize = 7;
const END_BLOCK: usize = 256;
const REP_3_6: usize = 16;
const REPZ_3_10: usize = 17;
const REPZ_11_138: usize = 18;

const EXTRA_LBITS: [i32; LENGTH_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const EXTRA_DBITS: [i32; D_CODES] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const EXTRA_BLBITS: [i32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
const BL_ORDER: [usize; BL_CODES] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// zlib configuration per level: good, lazy, nice, chain; levels 1–3 use `deflate_fast`.
const CONFIG: [(usize, usize, usize, usize); 10] = [
    (0, 0, 0, 0),
    (4, 4, 8, 4),
    (4, 5, 16, 8),
    (4, 6, 32, 32),
    (4, 4, 16, 16),
    (8, 16, 32, 32),
    (8, 16, 128, 128),
    (8, 32, 128, 256),
    (32, 128, 258, 1024),
    (32, 258, 258, 4096),
];

/// Tree entry like zlib's `ct_data`: `fc` = frequency or code, `dl` = parent or length.
#[derive(Clone, Copy, Default)]
struct Ct {
    fc: u16,
    dl: u16,
}

struct Tables {
    length_code: [u8; 256],
    dist_code: [u8; 512],
    base_length: [i32; LENGTH_CODES],
    base_dist: [i32; D_CODES],
    static_ltree: [Ct; L_CODES + 2],
    static_dtree: [Ct; D_CODES],
}

fn bi_reverse(mut code: u32, mut len: u32) -> u32 {
    let mut res = 0;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len == 0 {
            return res >> 1;
        }
    }
}

fn gen_codes(tree: &mut [Ct], max_code: usize, bl_count: &[u16; MAX_BITS + 1]) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code = 0u32;
    for bits in 1..=MAX_BITS {
        code = (code + bl_count[bits - 1] as u32) << 1;
        next_code[bits] = code as u16;
    }
    for t in tree.iter_mut().take(max_code + 1) {
        let len = t.dl as usize;
        if len == 0 {
            continue;
        }
        t.fc = bi_reverse(next_code[len] as u32, len as u32) as u16;
        next_code[len] = next_code[len].wrapping_add(1);
    }
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tables {
            length_code: [0; 256],
            dist_code: [0; 512],
            base_length: [0; LENGTH_CODES],
            base_dist: [0; D_CODES],
            static_ltree: [Ct::default(); L_CODES + 2],
            static_dtree: [Ct::default(); D_CODES],
        };
        let mut length = 0usize;
        let mut code = 0usize;
        while code < LENGTH_CODES - 1 {
            t.base_length[code] = length as i32;
            for _ in 0..(1 << EXTRA_LBITS[code]) {
                t.length_code[length] = code as u8;
                length += 1;
            }
            code += 1;
        }
        t.length_code[length - 1] = code as u8;
        let mut dist = 0usize;
        code = 0;
        while code < 16 {
            t.base_dist[code] = dist as i32;
            for _ in 0..(1 << EXTRA_DBITS[code]) {
                t.dist_code[dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        dist >>= 7;
        while code < D_CODES {
            t.base_dist[code] = (dist << 7) as i32;
            for _ in 0..(1 << (EXTRA_DBITS[code] - 7)) {
                t.dist_code[256 + dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        let mut bl_count = [0u16; MAX_BITS + 1];
        for (n, e) in t.static_ltree.iter_mut().enumerate() {
            let l = match n {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
            e.dl = l;
            bl_count[l as usize] += 1;
        }
        gen_codes(&mut t.static_ltree, L_CODES + 1, &bl_count);
        for (n, e) in t.static_dtree.iter_mut().enumerate() {
            e.dl = 5;
            e.fc = bi_reverse(n as u32, 5) as u16;
        }
        t
    })
}

#[inline]
fn d_code(t: &Tables, dist: usize) -> usize {
    if dist < 256 {
        t.dist_code[dist] as usize
    } else {
        t.dist_code[256 + (dist >> 7)] as usize
    }
}

/// Bitwise output buffer (LSB first, like zlib).
struct BitOut {
    out: Vec<u8>,
    buf: u64,
    valid: u32,
}
impl BitOut {
    #[inline]
    fn send(&mut self, value: u32, length: u32) {
        self.buf |= (value as u64) << self.valid;
        self.valid += length;
        while self.valid >= 8 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.valid -= 8;
        }
    }
    #[inline]
    fn code(&mut self, c: usize, tree: &[Ct]) {
        self.send(tree[c].fc as u32, tree[c].dl as u32);
    }
    /// Pad to a byte boundary (zlib `bi_windup`).
    fn windup(&mut self) {
        if self.valid > 0 {
            self.out.push(self.buf as u8);
        }
        self.buf = 0;
        self.valid = 0;
    }
}

/// Shared state for building trees (heap, depths, bit length counts, length sums).
struct Heap {
    heap: [usize; HEAP_SIZE],
    heap_len: usize,
    heap_max: usize,
    depth: [u8; HEAP_SIZE],
    bl_count: [u16; MAX_BITS + 1],
    opt_len: u64,
    static_len: u64,
}

#[inline]
fn smaller(tree: &[Ct], n: usize, m: usize, depth: &[u8; HEAP_SIZE]) -> bool {
    tree[n].fc < tree[m].fc || (tree[n].fc == tree[m].fc && depth[n] <= depth[m])
}

fn pqdownheap(h: &mut Heap, tree: &[Ct], mut k: usize) {
    let v = h.heap[k];
    let mut j = k << 1;
    while j <= h.heap_len {
        if j < h.heap_len && smaller(tree, h.heap[j + 1], h.heap[j], &h.depth) {
            j += 1;
        }
        if smaller(tree, v, h.heap[j], &h.depth) {
            break;
        }
        h.heap[k] = h.heap[j];
        k = j;
        j <<= 1;
    }
    h.heap[k] = v;
}

/// Description of a tree (zlib `static_tree_desc`).
struct Desc<'a> {
    stree: Option<&'a [Ct]>,
    extra: &'a [i32],
    extra_base: usize,
    elems: usize,
    max_length: usize,
}

fn gen_bitlen(h: &mut Heap, tree: &mut [Ct], max_code: usize, d: &Desc) {
    for b in h.bl_count.iter_mut() {
        *b = 0;
    }
    let root = h.heap[h.heap_max];
    tree[root].dl = 0;
    let mut overflow = 0i32;
    let mut hh = h.heap_max + 1;
    while hh < HEAP_SIZE {
        let n = h.heap[hh];
        let mut bits = tree[tree[n].dl as usize].dl as usize + 1;
        if bits > d.max_length {
            bits = d.max_length;
            overflow += 1;
        }
        tree[n].dl = bits as u16;
        hh += 1;
        if n > max_code {
            continue;
        }
        h.bl_count[bits] += 1;
        let xbits = if n >= d.extra_base { d.extra[n - d.extra_base] as u64 } else { 0 };
        let f = tree[n].fc as u64;
        h.opt_len = h.opt_len.wrapping_add(f * (bits as u64 + xbits));
        if let Some(st) = d.stree {
            h.static_len = h.static_len.wrapping_add(f * (st[n].dl as u64 + xbits));
        }
    }
    if overflow == 0 {
        return;
    }
    loop {
        let mut bits = d.max_length - 1;
        while h.bl_count[bits] == 0 {
            bits -= 1;
        }
        h.bl_count[bits] -= 1;
        h.bl_count[bits + 1] += 2;
        h.bl_count[d.max_length] -= 1;
        overflow -= 2;
        if overflow <= 0 {
            break;
        }
    }
    let mut hh = HEAP_SIZE;
    for bits in (1..=d.max_length).rev() {
        let mut n = h.bl_count[bits];
        while n != 0 {
            hh -= 1;
            let m = h.heap[hh];
            if m > max_code {
                continue;
            }
            if tree[m].dl as usize != bits {
                let delta = (bits as u64).wrapping_sub(tree[m].dl as u64).wrapping_mul(tree[m].fc as u64);
                h.opt_len = h.opt_len.wrapping_add(delta);
                tree[m].dl = bits as u16;
            }
            n -= 1;
        }
    }
}

/// Build a Huffman tree (zlib `build_tree`); returns `max_code`.
fn build_tree(h: &mut Heap, tree: &mut [Ct], d: &Desc) -> usize {
    let mut max_code: isize = -1;
    h.heap_len = 0;
    h.heap_max = HEAP_SIZE;
    for n in 0..d.elems {
        if tree[n].fc != 0 {
            h.heap_len += 1;
            h.heap[h.heap_len] = n;
            max_code = n as isize;
            h.depth[n] = 0;
        } else {
            tree[n].dl = 0;
        }
    }
    while h.heap_len < 2 {
        let node = if max_code < 2 {
            max_code += 1;
            max_code as usize
        } else {
            0
        };
        h.heap_len += 1;
        h.heap[h.heap_len] = node;
        tree[node].fc = 1;
        h.depth[node] = 0;
        h.opt_len = h.opt_len.wrapping_sub(1);
        if let Some(st) = d.stree {
            h.static_len = h.static_len.wrapping_sub(st[node].dl as u64);
        }
    }
    let max_code = max_code as usize;
    for n in (1..=h.heap_len / 2).rev() {
        pqdownheap(h, tree, n);
    }
    let mut node = d.elems;
    loop {
        // pqremove
        let n = h.heap[1];
        h.heap[1] = h.heap[h.heap_len];
        h.heap_len -= 1;
        pqdownheap(h, tree, 1);
        let m = h.heap[1];
        h.heap_max -= 1;
        h.heap[h.heap_max] = n;
        h.heap_max -= 1;
        h.heap[h.heap_max] = m;
        tree[node].fc = tree[n].fc.wrapping_add(tree[m].fc);
        h.depth[node] = h.depth[n].max(h.depth[m]) + 1;
        tree[n].dl = node as u16;
        tree[m].dl = node as u16;
        h.heap[1] = node;
        node += 1;
        pqdownheap(h, tree, 1);
        if h.heap_len < 2 {
            break;
        }
    }
    h.heap_max -= 1;
    h.heap[h.heap_max] = h.heap[1];
    gen_bitlen(h, tree, max_code, d);
    let bl_count = h.bl_count;
    gen_codes(tree, max_code, &bl_count);
    max_code
}

/// Count the run lengths of the code lengths (zlib `scan_tree`).
fn scan_tree(tree: &mut [Ct], max_code: usize, bl_tree: &mut [Ct]) {
    let mut prevlen: i32 = -1;
    let mut nextlen = tree[0].dl as i32;
    let mut count = 0u16;
    let (mut max_count, mut min_count) = (7u16, 4u16);
    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }
    tree[max_code + 1].dl = 0xffff; // guard
    for n in 0..=max_code {
        let curlen = nextlen;
        nextlen = tree[n + 1].dl as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            bl_tree[curlen as usize].fc = bl_tree[curlen as usize].fc.wrapping_add(count);
        } else if curlen != 0 {
            if curlen != prevlen {
                bl_tree[curlen as usize].fc = bl_tree[curlen as usize].fc.wrapping_add(1);
            }
            bl_tree[REP_3_6].fc = bl_tree[REP_3_6].fc.wrapping_add(1);
        } else if count <= 10 {
            bl_tree[REPZ_3_10].fc = bl_tree[REPZ_3_10].fc.wrapping_add(1);
        } else {
            bl_tree[REPZ_11_138].fc = bl_tree[REPZ_11_138].fc.wrapping_add(1);
        }
        count = 0;
        prevlen = curlen;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        } else if curlen == nextlen {
            max_count = 6;
            min_count = 3;
        } else {
            max_count = 7;
            min_count = 4;
        }
    }
}

/// Send the code lengths of a tree (zlib `send_tree`).
fn send_tree(bw: &mut BitOut, tree: &[Ct], max_code: usize, bl_tree: &[Ct]) {
    let mut prevlen: i32 = -1;
    let mut nextlen = tree[0].dl as i32;
    let mut count = 0u32;
    let (mut max_count, mut min_count) = (7u32, 4u32);
    if nextlen == 0 {
        max_count = 138;
        min_count = 3;
    }
    for n in 0..=max_code {
        let curlen = nextlen;
        nextlen = tree[n + 1].dl as i32;
        count += 1;
        if count < max_count && curlen == nextlen {
            continue;
        } else if count < min_count {
            while count != 0 {
                bw.code(curlen as usize, bl_tree);
                count -= 1;
            }
        } else if curlen != 0 {
            if curlen != prevlen {
                bw.code(curlen as usize, bl_tree);
                count -= 1;
            }
            bw.code(REP_3_6, bl_tree);
            bw.send(count - 3, 2);
        } else if count <= 10 {
            bw.code(REPZ_3_10, bl_tree);
            bw.send(count - 3, 3);
        } else {
            bw.code(REPZ_11_138, bl_tree);
            bw.send(count - 11, 7);
        }
        count = 0;
        prevlen = curlen;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        } else if curlen == nextlen {
            max_count = 6;
            min_count = 3;
        } else {
            max_count = 7;
            min_count = 4;
        }
    }
}

/// Compressor state (corresponds to zlib's `deflate_state` for a single `deflate(Z_FINISH)` call).
struct State<'a> {
    t: &'static Tables,
    input: &'a [u8],
    in_pos: usize,
    // Configuration
    good_match: usize,
    max_lazy: usize,
    nice_match: usize,
    max_chain: usize,
    filtered: bool,
    // Window and hash chains
    window: Box<[u8; WINDOW_SIZE]>,
    prev: Box<[u16; W_SIZE]>,
    head: Vec<u16>,
    hash_mask: usize,
    hash_shift: usize,
    ins_h: usize,
    high_water: usize,
    strstart: usize,
    block_start: isize,
    lookahead: usize,
    match_start: usize,
    match_length: usize,
    prev_length: usize,
    prev_match: usize,
    match_available: bool,
    insert: usize,
    // Symbol buffer (distance, length-3 or literal)
    sym: Vec<(u16, u8)>,
    sym_end: usize,
    // Trees
    dyn_ltree: [Ct; HEAP_SIZE],
    dyn_dtree: [Ct; 2 * D_CODES + 1],
    bl_tree: [Ct; 2 * BL_CODES + 1],
    h: Heap,
    bw: BitOut,
    // Comparison with an expected stream (early abort)
    expect: Option<&'a [u8]>,
    checked: usize,
}

impl<'a> State<'a> {
    #[inline]
    fn update_hash(&self, h: usize, c: u8) -> usize {
        ((h << self.hash_shift) ^ c as usize) & self.hash_mask
    }
    /// zlib `INSERT_STRING`: returns the previous head of the hash chain.
    #[inline]
    fn insert_string(&mut self, s: usize) -> usize {
        self.ins_h = self.update_hash(self.ins_h, self.window[s + MIN_MATCH - 1]);
        let mh = self.head[self.ins_h];
        self.prev[s & W_MASK] = mh;
        self.head[self.ins_h] = s as u16;
        mh as usize
    }

    fn slide_hash(&mut self) {
        for m in self.head.iter_mut().chain(self.prev.iter_mut()) {
            *m = if *m as usize >= W_SIZE { *m - W_SIZE as u16 } else { NIL };
        }
    }

    fn fill_window(&mut self) {
        loop {
            let mut more = WINDOW_SIZE - self.lookahead - self.strstart;
            if self.strstart >= W_SIZE + MAX_DIST {
                self.window.copy_within(W_SIZE..W_SIZE + (W_SIZE - more), 0);
                self.match_start = self.match_start.wrapping_sub(W_SIZE);
                self.strstart -= W_SIZE;
                self.block_start -= W_SIZE as isize;
                if self.insert > self.strstart {
                    self.insert = self.strstart;
                }
                self.slide_hash();
                more += W_SIZE;
            }
            let avail = self.input.len() - self.in_pos;
            if avail == 0 {
                break;
            }
            let n = avail.min(more);
            let dst = self.strstart + self.lookahead;
            self.window[dst..dst + n].copy_from_slice(&self.input[self.in_pos..self.in_pos + n]);
            self.in_pos += n;
            self.lookahead += n;
            if self.lookahead + self.insert >= MIN_MATCH {
                let mut s = self.strstart - self.insert;
                self.ins_h = self.window[s] as usize;
                self.ins_h = self.update_hash(self.ins_h, self.window[s + 1]);
                while self.insert > 0 {
                    self.ins_h = self.update_hash(self.ins_h, self.window[s + MIN_MATCH - 1]);
                    self.prev[s & W_MASK] = self.head[self.ins_h];
                    self.head[self.ins_h] = s as u16;
                    s += 1;
                    self.insert -= 1;
                    if self.lookahead + self.insert < MIN_MATCH {
                        break;
                    }
                }
            }
            if self.lookahead >= MIN_LOOKAHEAD || self.in_pos == self.input.len() {
                break;
            }
        }
        // Zero the bytes behind the data like zlib (WIN_INIT) so that comparisons there are deterministic
        if self.high_water < WINDOW_SIZE {
            let curr = self.strstart + self.lookahead;
            if self.high_water < curr {
                let init = (WINDOW_SIZE - curr).min(WIN_INIT);
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                let init = (curr + WIN_INIT - self.high_water).min(WINDOW_SIZE - self.high_water);
                self.window[self.high_water..self.high_water + init].fill(0);
                self.high_water += init;
            }
        }
    }

    /// zlib `longest_match` (variant without UNALIGNED_OK).
    fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain = self.max_chain;
        let scan = self.strstart;
        let mut best_len = self.prev_length;
        let mut nice = self.nice_match;
        let limit = self.strstart.saturating_sub(MAX_DIST);
        let w: &[u8; WINDOW_SIZE] = &self.window;
        let prev: &[u16; W_SIZE] = &self.prev;
        let mut scan_end1 = w[scan + best_len - 1];
        let mut scan_end = w[scan + best_len];
        if self.prev_length >= self.good_match {
            chain >>= 2;
        }
        if nice > self.lookahead {
            nice = self.lookahead;
        }
        let s3 = &w[scan + 3..scan + MAX_MATCH];
        loop {
            let m = cur_match;
            if w[m + best_len] == scan_end && w[m + best_len - 1] == scan_end1 && w[m] == w[scan] && w[m + 1] == w[scan + 1] {
                // Byte 2 is not checked, as in zlib; compare from byte 3, at most up to 258
                let len = 3 + common_prefix(s3, &w[m + 3..m + MAX_MATCH]);
                if len > best_len {
                    self.match_start = cur_match;
                    best_len = len;
                    if len >= nice {
                        break;
                    }
                    scan_end1 = w[scan + best_len - 1];
                    scan_end = w[scan + best_len];
                }
            }
            cur_match = prev[cur_match & W_MASK] as usize;
            if cur_match <= limit {
                break;
            }
            chain -= 1;
            if chain == 0 {
                break;
            }
        }
        best_len.min(self.lookahead)
    }

    #[inline]
    fn tally_lit(&mut self, c: u8) -> bool {
        self.sym.push((0, c));
        self.dyn_ltree[c as usize].fc = self.dyn_ltree[c as usize].fc.wrapping_add(1);
        self.sym.len() == self.sym_end
    }
    #[inline]
    fn tally_dist(&mut self, dist: usize, lc: usize) -> bool {
        self.sym.push((dist as u16, lc as u8));
        let t = self.t;
        let li = t.length_code[lc] as usize + LITERALS + 1;
        self.dyn_ltree[li].fc = self.dyn_ltree[li].fc.wrapping_add(1);
        let di = d_code(t, dist - 1);
        self.dyn_dtree[di].fc = self.dyn_dtree[di].fc.wrapping_add(1);
        self.sym.len() == self.sym_end
    }

    fn init_block(&mut self) {
        for t in self.dyn_ltree.iter_mut().take(L_CODES) {
            t.fc = 0;
        }
        for t in self.dyn_dtree.iter_mut().take(D_CODES) {
            t.fc = 0;
        }
        for t in self.bl_tree.iter_mut().take(BL_CODES) {
            t.fc = 0;
        }
        self.dyn_ltree[END_BLOCK].fc = 1;
        self.h.opt_len = 0;
        self.h.static_len = 0;
        self.sym.clear();
    }

    fn compress_block(&mut self, stat: bool) {
        let t = self.t;
        let (lt, dt): (&[Ct], &[Ct]) = if stat { (&t.static_ltree, &t.static_dtree) } else { (&self.dyn_ltree, &self.dyn_dtree) };
        for &(dist, lc) in &self.sym {
            if dist == 0 {
                self.bw.code(lc as usize, lt);
            } else {
                let lc = lc as usize;
                let code = t.length_code[lc] as usize;
                self.bw.code(code + LITERALS + 1, lt);
                let extra = EXTRA_LBITS[code];
                if extra != 0 {
                    self.bw.send((lc as i32 - t.base_length[code]) as u32, extra as u32);
                }
                let d = dist as usize - 1;
                let code = d_code(t, d);
                self.bw.code(code, dt);
                let extra = EXTRA_DBITS[code];
                if extra != 0 {
                    self.bw.send((d as i32 - t.base_dist[code]) as u32, extra as u32);
                }
            }
        }
        self.bw.code(END_BLOCK, lt);
    }

    /// zlib `_tr_flush_block`.
    fn tr_flush_block(&mut self, buf_ok: bool, stored_len: usize, last: bool) {
        let t = self.t;
        let ld = Desc { stree: Some(&t.static_ltree), extra: &EXTRA_LBITS, extra_base: LITERALS + 1, elems: L_CODES, max_length: MAX_BITS };
        let dd = Desc { stree: Some(&t.static_dtree), extra: &EXTRA_DBITS, extra_base: 0, elems: D_CODES, max_length: MAX_BITS };
        let bd = Desc { stree: None, extra: &EXTRA_BLBITS, extra_base: 0, elems: BL_CODES, max_length: MAX_BL_BITS };
        let l_max = build_tree(&mut self.h, &mut self.dyn_ltree, &ld);
        let d_max = build_tree(&mut self.h, &mut self.dyn_dtree, &dd);
        // build_bl_tree
        scan_tree(&mut self.dyn_ltree, l_max, &mut self.bl_tree);
        scan_tree(&mut self.dyn_dtree, d_max, &mut self.bl_tree);
        build_tree(&mut self.h, &mut self.bl_tree, &bd);
        let mut max_blindex = BL_CODES - 1;
        while max_blindex >= 3 {
            if self.bl_tree[BL_ORDER[max_blindex]].dl != 0 {
                break;
            }
            max_blindex -= 1;
        }
        self.h.opt_len = self.h.opt_len.wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);

        let mut opt_lenb = self.h.opt_len.wrapping_add(3 + 7) >> 3;
        let static_lenb = self.h.static_len.wrapping_add(3 + 7) >> 3;
        if static_lenb <= opt_lenb {
            opt_lenb = static_lenb;
        }
        if stored_len as u64 + 4 <= opt_lenb && buf_ok {
            // stored block
            self.bw.send(last as u32, 3);
            self.bw.windup();
            let start = self.block_start as usize;
            let len = stored_len as u16;
            self.bw.out.extend_from_slice(&len.to_le_bytes());
            self.bw.out.extend_from_slice(&(!len).to_le_bytes());
            self.bw.out.extend_from_slice(&self.window[start..start + stored_len]);
        } else if static_lenb == opt_lenb {
            self.bw.send((1 << 1) + last as u32, 3);
            self.compress_block(true);
        } else {
            self.bw.send((2 << 1) + last as u32, 3);
            // send_all_trees
            let (lcodes, dcodes, blcodes) = (l_max + 1, d_max + 1, max_blindex + 1);
            self.bw.send(lcodes as u32 - 257, 5);
            self.bw.send(dcodes as u32 - 1, 5);
            self.bw.send(blcodes as u32 - 4, 4);
            for &o in BL_ORDER.iter().take(blcodes) {
                self.bw.send(self.bl_tree[o].dl as u32, 3);
            }
            send_tree(&mut self.bw, &self.dyn_ltree, lcodes - 1, &self.bl_tree);
            send_tree(&mut self.bw, &self.dyn_dtree, dcodes - 1, &self.bl_tree);
            self.compress_block(false);
        }
        self.init_block();
        if last {
            self.bw.windup();
        }
    }

    /// zlib `FLUSH_BLOCK_ONLY`; returns `false` if the output differs from the expected stream.
    fn flush_block(&mut self, last: bool) -> bool {
        let buf_ok = self.block_start >= 0;
        let stored_len = (self.strstart as isize - self.block_start) as usize;
        self.tr_flush_block(buf_ok, stored_len, last);
        self.block_start = self.strstart as isize;
        self.check()
    }

    /// Compare the bytes produced so far with the expected stream.
    fn check(&mut self) -> bool {
        let Some(e) = self.expect else { return true };
        let o = &self.bw.out;
        if o.len() > e.len() || o[self.checked..] != e[self.checked..o.len()] {
            return false;
        }
        self.checked = o.len();
        true
    }

    /// zlib `deflate_fast` (levels 1–3).
    fn deflate_fast(&mut self) -> bool {
        loop {
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window();
                if self.lookahead == 0 {
                    break;
                }
            }
            let mut hash_head = 0usize;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert_string(self.strstart);
            }
            if hash_head != 0 && self.strstart - hash_head <= MAX_DIST {
                self.match_length = self.longest_match(hash_head);
            }
            let bflush;
            if self.match_length >= MIN_MATCH {
                bflush = self.tally_dist(self.strstart.wrapping_sub(self.match_start), self.match_length - MIN_MATCH);
                self.lookahead -= self.match_length;
                if self.match_length <= self.max_lazy && self.lookahead >= MIN_MATCH {
                    self.match_length -= 1; // the string at strstart is already in the table
                    loop {
                        self.strstart += 1;
                        self.insert_string(self.strstart);
                        self.match_length -= 1;
                        if self.match_length == 0 {
                            break;
                        }
                    }
                    self.strstart += 1;
                } else {
                    self.strstart += self.match_length;
                    self.match_length = 0;
                    self.ins_h = self.window[self.strstart] as usize;
                    self.ins_h = self.update_hash(self.ins_h, self.window[self.strstart + 1]);
                }
            } else {
                bflush = self.tally_lit(self.window[self.strstart]);
                self.lookahead -= 1;
                self.strstart += 1;
            }
            if bflush && !self.flush_block(false) {
                return false;
            }
        }
        self.flush_block(true)
    }

    /// zlib `deflate_slow` (levels 4–9, lazy match evaluation).
    fn deflate_slow(&mut self) -> bool {
        loop {
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window();
                if self.lookahead == 0 {
                    break;
                }
            }
            let mut hash_head = 0usize;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert_string(self.strstart);
            }
            self.prev_length = self.match_length;
            self.prev_match = self.match_start;
            self.match_length = MIN_MATCH - 1;
            if hash_head != 0 && self.prev_length < self.max_lazy && self.strstart - hash_head <= MAX_DIST {
                self.match_length = self.longest_match(hash_head);
                if self.match_length <= 5
                    && (self.filtered || (self.match_length == MIN_MATCH && self.strstart.wrapping_sub(self.match_start) > TOO_FAR))
                {
                    self.match_length = MIN_MATCH - 1;
                }
            }
            if self.prev_length >= MIN_MATCH && self.match_length <= self.prev_length {
                let max_insert = self.strstart + self.lookahead - MIN_MATCH;
                // Modular distance as in zlib (prev_match may have wrapped around after sliding)
                let bflush = self.tally_dist(self.strstart.wrapping_sub(1).wrapping_sub(self.prev_match), self.prev_length - MIN_MATCH);
                self.lookahead -= self.prev_length - 1;
                self.prev_length -= 2;
                loop {
                    self.strstart += 1;
                    if self.strstart <= max_insert {
                        self.insert_string(self.strstart);
                    }
                    self.prev_length -= 1;
                    if self.prev_length == 0 {
                        break;
                    }
                }
                self.match_available = false;
                self.match_length = MIN_MATCH - 1;
                self.strstart += 1;
                if bflush && !self.flush_block(false) {
                    return false;
                }
            } else if self.match_available {
                let bflush = self.tally_lit(self.window[self.strstart - 1]);
                if bflush && !self.flush_block(false) {
                    return false;
                }
                self.strstart += 1;
                self.lookahead -= 1;
            } else {
                self.match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }
        }
        if self.match_available {
            self.tally_lit(self.window[self.strstart - 1]);
            self.match_available = false;
        }
        self.flush_block(true)
    }
}

/// Length of the common prefix of two ranges of equal length.
#[inline]
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(a[i..i + 8].try_into().unwrap()) ^ u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// Adler-32 (zlib checksum).
pub fn adler32(data: &[u8]) -> u32 {
    const BASE: u32 = 65521;
    const NMAX: usize = 5552;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(NMAX) {
        for &c in chunk {
            a += c as u32;
            b += a;
        }
        a %= BASE;
        b %= BASE;
    }
    (b << 16) | a
}

/// Common core: compresses `raw`; with `expect` the output is compared continuously and aborted.
fn run(raw: &[u8], level: i32, mem_level: i32, strategy: i32, expect: Option<&[u8]>) -> Option<Vec<u8>> {
    if !(1..=9).contains(&level) || !(1..=9).contains(&mem_level) || !(0..=1).contains(&strategy) {
        return None;
    }
    let (good, lazy, nice, chain) = CONFIG[level as usize];
    let hash_bits = mem_level as usize + 7;
    let lit_bufsize = 1usize << (mem_level + 6);
    let mut s = State {
        t: tables(),
        input: raw,
        in_pos: 0,
        good_match: good,
        max_lazy: lazy,
        nice_match: nice,
        max_chain: chain,
        filtered: strategy == 1,
        window: vec![0u8; WINDOW_SIZE].into_boxed_slice().try_into().unwrap(),
        prev: vec![0u16; W_SIZE].into_boxed_slice().try_into().unwrap(),
        head: vec![0u16; 1 << hash_bits],
        hash_mask: (1 << hash_bits) - 1,
        hash_shift: hash_bits.div_ceil(MIN_MATCH),
        ins_h: 0,
        high_water: 0,
        strstart: 0,
        block_start: 0,
        lookahead: 0,
        match_start: 0,
        match_length: MIN_MATCH - 1,
        prev_length: MIN_MATCH - 1,
        prev_match: 0,
        match_available: false,
        insert: 0,
        sym: Vec::with_capacity(lit_bufsize),
        sym_end: lit_bufsize - 1,
        dyn_ltree: [Ct::default(); HEAP_SIZE],
        dyn_dtree: [Ct::default(); 2 * D_CODES + 1],
        bl_tree: [Ct::default(); 2 * BL_CODES + 1],
        h: Heap {
            heap: [0; HEAP_SIZE],
            heap_len: 0,
            heap_max: 0,
            depth: [0; HEAP_SIZE],
            bl_count: [0; MAX_BITS + 1],
            opt_len: 0,
            static_len: 0,
        },
        bw: BitOut { out: Vec::with_capacity(expect.map_or(raw.len() / 2 + 64, |e| e.len() + 16)), buf: 0, valid: 0 },
        expect,
        checked: 0,
    };
    s.init_block();
    // zlib header: CMF/FLG with the level class
    let level_flags = match level {
        ..=1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let mut header: u32 = (8 + ((W_BITS as u32 - 8) << 4)) << 8;
    header |= level_flags << 6;
    header += 31 - (header % 31);
    s.bw.out.extend_from_slice(&(header as u16).to_be_bytes());
    if !s.check() {
        return None;
    }
    let ok = if level <= 3 { s.deflate_fast() } else { s.deflate_slow() };
    if !ok {
        return None;
    }
    s.bw.out.extend_from_slice(&adler32(raw).to_be_bytes());
    if !s.check() {
        return None;
    }
    Some(s.bw.out)
}

/// Compresses like `zlib deflateInit2(level, Z_DEFLATED, 15, mem_level, strategy)` + `deflate(Z_FINISH)`.
/// Supports levels 1–9, memLevel 1–9, strategy 0 (default) and 1 (filtered); otherwise `None`.
pub fn deflate_with(raw: &[u8], level: i32, mem_level: i32, strategy: i32) -> Option<Vec<u8>> {
    run(raw, level, mem_level, strategy, None)
}

/// Does zlib produce exactly `expect` with these parameters? Stops at the first differing block.
pub fn matches(raw: &[u8], level: i32, mem_level: i32, strategy: i32, expect: &[u8]) -> bool {
    run(raw, level, mem_level, strategy, Some(expect)).is_some_and(|o| o.len() == expect.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adler() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let big = vec![0xFFu8; 100_000];
        // Reference: step by step without NMAX blocking (u64, no overflow)
        let (mut a, mut b) = (1u64, 0u64);
        for &c in &big {
            a = (a + c as u64) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(adler32(&big), ((b << 16) | a) as u32);
    }
    #[test]
    fn header_and_empty() {
        // Empty input: zlib produces 78 xx 03 00 00 00 00 01
        let o = deflate_with(b"", 6, 8, 0).unwrap();
        assert_eq!(o, [0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(deflate_with(b"", 9, 8, 0).unwrap()[..2], [0x78, 0xDA]);
        assert_eq!(deflate_with(b"", 1, 8, 0).unwrap()[..2], [0x78, 0x01]);
    }
    #[test]
    fn invalid_parameters() {
        assert!(deflate_with(b"abc", 0, 8, 0).is_none());
        assert!(deflate_with(b"abc", 6, 10, 0).is_none());
        assert!(deflate_with(b"abc", 6, 8, 2).is_none());
    }
}
