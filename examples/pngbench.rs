//! Measure PNG recompression: pngbench <directory with .png files>
use std::time::Instant;
fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mut files: Vec<_> =
        std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "png")).collect();
    files.sort();
    let (mut tf, mut ti, mut ok, mut bytes) = (0.0, 0.0, 0, 0usize);
    let mut outs = Vec::new();
    for f in &files {
        let png = std::fs::read(f).unwrap();
        bytes += png.len();
        let t = Instant::now();
        let r = gpk::pngx::forward(&png, 1);
        tf += t.elapsed().as_secs_f64();
        if let Some(t1) = r {
            ok += 1;
            let mut out = vec![0u8; png.len()];
            let t = Instant::now();
            assert!(gpk::pngx::inverse(&t1, &mut out) && out == png);
            ti += t.elapsed().as_secs_f64();
            outs.push(t1.len());
        }
    }
    println!("{} PNGs ({:.1} MB), {} reversible | search {:.2} s, inverse {:.2} s", files.len(), bytes as f64 / 1e6, ok, tf, ti);
}
