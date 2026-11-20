//! Differenztests: Rust-Port gegen C-Referenz (legacy-c/). Nur mit `--features cref`.
#![cfg(feature = "cref")]
use gpk::cref::*;
use gpk::filters::{self, FSpec, FT_COUNT, FT_SPLIT, FT_WPRED};
use gpk::testutil::Rng;

fn to_c(f: &FSpec) -> CFspec {
    CFspec { id: f.id, stride: f.stride, nfields: f.nfields, fields: f.fields, skip: f.skip }
}
fn random_spec(r: &mut Rng) -> FSpec {
    let mut f = FSpec::new((r.next() % FT_COUNT as u32) as u8);
    f.stride = 1 + (r.next() % 64) as u8;
    if f.id == FT_WPRED {
        let w = if r.next() % 2 == 1 { 2 } else { 4 };
        f.fields = [w, 1 + (r.next() % 2) as u8, (r.next() % 2) as u8, 0, 0, 0];
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
/// Testdaten mit Struktur (x86-Opcodes, Wiederholungen, Rauschen)
pub fn test_data(r: &mut Rng, n: usize) -> Vec<u8> {
    let mode = r.next() % 4;
    (0..n)
        .map(|i| match mode {
            0 => r.next() as u8,
            1 => [0xE8u8, 0xE9, 0x0F, 0x85, 0x00, 0xFF, 0x48, 0x8B][(r.next() % 8) as usize],
            2 => ((i as u32 * 7) ^ (i as u32 >> 5)) as u8,
            _ => (i % 251) as u8,
        })
        .collect()
}

#[test]
fn filters_identisch_zu_c() {
    let mut r = Rng::new(42);
    for _ in 0..3000 {
        let n = (r.next() % 30000) as usize;
        let data = test_data(&mut r, n);
        let f = random_spec(&mut r);
        let off = if r.next() % 3 == 0 { 0 } else { r.next() as u64 * 13 };
        let mut rust = vec![0u8; n];
        filters::encode(&f, &data, &mut rust, off);
        let mut c = vec![0u8; n];
        unsafe { filter_encode(&to_c(&f), data.as_ptr(), c.as_mut_ptr(), n, off) };
        assert_eq!(rust, c, "encode weicht ab: {:?} n={} off={}", f, n, off);
        // Kreuz: C-kodiertes mit Rust dekodieren
        let mut t = c.clone();
        let mut back = vec![0u8; n];
        filters::decode(&f, &mut t, &mut back, off);
        assert_eq!(back, data, "decode weicht ab: {:?}", f);
    }
}

fn mixed_data(r: &mut Rng, n: usize) -> Vec<u8> {
    let words: [&[u8]; 6] = [b"Drache ", b"Schwert ", b"{\"id\": 42}, ", b"\x00\x00\x80\x3f", b"\xE8\x10\x20\x00\x00", b"func() {\n    return 1;\n}\n"];
    let mut v = Vec::with_capacity(n);
    let noise = r.next() % 3;
    while v.len() < n {
        if r.next() % 4 < noise {
            v.push(r.next() as u8);
        } else {
            v.extend_from_slice(words[(r.next() % 6) as usize]);
        }
    }
    v.truncate(n);
    v
}

#[test]
fn glz_identisch_zu_c() {
    let mut r = Rng::new(7);
    for case in 0..40 {
        let n = [0usize, 3, 100, 5000, 70_000, 300_000, 1_200_000][case % 7];
        let d = if case % 5 == 4 { test_data(&mut r, n) } else { mixed_data(&mut r, n) };
        let level = 1 + (case % 9) as i32;
        let nf = case % 3 != 0;
        let rust = gpk::glz::compress(&d, level, nf);
        let cap = unsafe { glz_bound(n) };
        let mut c = vec![0u8; cap];
        let cn = unsafe { glz_compress(d.as_ptr(), n, c.as_mut_ptr(), cap, level, if nf { 1 } else { 0 }) };
        c.truncate(cn);
        assert_eq!(rust, c, "GLZ weicht ab: Fall {case}, n={n}, Stufe {level}, ohne Filter {nf}");
        // Kreuz-Dekodierung
        assert_eq!(gpk::glz::decompress(&c, usize::MAX).unwrap(), d);
        let mut back = vec![0u8; n + 1];
        let bn = unsafe { glz_decompress(rust.as_ptr(), rust.len(), back.as_mut_ptr(), n + 1) };
        assert_eq!(bn, n);
        assert_eq!(&back[..n], &d[..]);
    }
}

#[test]
fn lzrc_identisch_zu_c() {
    let mut r = Rng::new(11);
    for case in 0..24 {
        let n = [0usize, 1, 5, 300, 9000, 100_000, 700_000, 2_000_000][case % 8];
        let d = if case % 4 == 3 { test_data(&mut r, n) } else { mixed_data(&mut r, n) };
        let level = [2, 5, 7, 9][case % 4];
        let mut p = gpk::lzrc::Params::level(level);
        if case % 3 == 1 { p.lc = 4; p.pb = 0; }
        if case % 5 == 2 { p.lc = 0; p.lp = 2; p.pb = 2; }
        let rust = gpk::lzrc::compress(&d, &p);
        let cp = CLzrcParams { lc: p.lc as i32, lp: p.lp as i32, pb: p.pb as i32, depth: p.depth as i32, nice: p.nice as i32 };
        let cap = unsafe { lzrc_bound(n) };
        let mut c = vec![0u8; cap];
        let cn = unsafe { lzrc_compress(d.as_ptr(), n, c.as_mut_ptr(), cap, &cp) };
        c.truncate(cn);
        assert_eq!(rust.len(), c.len(), "LZRC-Größe weicht ab: Fall {case}, n={n}, {p:?}");
        assert!(rust == c, "LZRC-Bytes weichen ab: Fall {case}, n={n}, {p:?}");
        let mut back = vec![0u8; n];
        gpk::lzrc::decompress(&c, &mut back).expect("Rust dekodiert C-Daten");
        assert_eq!(back, d);
        let mut back2 = vec![0u8; n];
        assert_eq!(unsafe { lzrc_decompress(rust.as_ptr(), rust.len(), back2.as_mut_ptr(), n) }, 1);
        assert_eq!(back2, d);
    }
}
