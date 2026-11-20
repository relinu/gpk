//! Own Deflate/Inflate implementation vs. zlib (speed).
use std::time::Instant;
fn t<R>(f: impl FnOnce() -> R) -> (R, f64) {
    let s = Instant::now();
    let r = f();
    (r, s.elapsed().as_secs_f64())
}
fn main() {
    // Image-like raw data (PNG rows) and text from the file given as argument
    let mut r = gpk::testutil::Rng::new(5);
    let mut img = Vec::new();
    for y in 0..2000u32 {
        img.push((y % 5) as u8);
        for x in 0..2400u32 {
            img.push(((x * 3 + y * 7) & 0xFF) as u8 ^ (r.next() % 8) as u8);
        }
    }
    let text = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    for (name, d) in [("image", &img), ("text", &text)] {
        let mb = d.len() as f64 / 1e6;
        for lvl in [1, 6, 9] {
            let (z, tz) = t(|| gpk::zlib::deflate_with(d, lvl, 8, 0).unwrap());
            let (o, to) = t(|| gpk::deflate::deflate_with(d, lvl, 8, 0).unwrap());
            assert!(o == z);
            let (a, iz) = t(|| gpk::zlib::inflate_all(&z, 1 << 30).unwrap());
            let (b, io) = t(|| gpk::inflate::inflate_zlib(&z, 1 << 30).unwrap());
            assert!(a == b);
            println!(
                "{name} {mb:5.1} MB level {lvl}: deflate zlib {:6.1} MB/s, own {:6.1} MB/s | inflate zlib {:6.0} MB/s, own {:6.0} MB/s",
                mb / tz,
                mb / to,
                mb / iz,
                mb / io
            );
        }
    }
}
