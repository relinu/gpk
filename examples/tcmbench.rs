//! TCM-Geschwindigkeit messen und Ausgabe mit der C-Referenz vergleichen.
use gpk::cref::*;
use std::time::Instant;
fn main() {
    let d = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let mb = d.len() as f64 / 1e6;
    let t = Instant::now();
    let z = gpk::tcm::compress(&d);
    let ct = t.elapsed().as_secs_f64();
    let mut out = vec![0u8; d.len()];
    let t = Instant::now();
    gpk::tcm::decompress(&z, &mut out);
    let dt = t.elapsed().as_secs_f64();
    assert!(out == d, "Roundtrip falsch");
    let mut cb = vec![0u8; unsafe { tcm_bound(d.len()) }];
    let cn = unsafe { tcm_compress(d.as_ptr(), d.len(), cb.as_mut_ptr(), cb.len()) };
    println!(
        "TCM {mb:.1} MB -> {} B | packen {:.2} MB/s, entpacken {:.2} MB/s | {}",
        z.len(),
        mb / ct,
        mb / dt,
        if cb[..cn] == z[..] { "bytegleich zu C" } else { "ABWEICHUNG zu C" }
    );
}
