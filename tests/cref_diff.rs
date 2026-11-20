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
