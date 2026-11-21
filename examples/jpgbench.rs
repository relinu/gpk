//! Measure JPEG recompression: jpgbench <directory with .jpg files>
use std::time::Instant;
fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mut files: Vec<_> =
        std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jpg")).collect();
    files.sort();
    let (mut tf, mut ti, mut ok, mut bytes, mut tbytes) = (0.0, f64::MAX, 0, 0usize, 0usize);
    let mut ts = Vec::new();
    for f in &files {
        let jpg = std::fs::read(f).unwrap();
        bytes += jpg.len();
        let t = Instant::now();
        let r = gpk::jpgx::forward(&jpg);
        tf += t.elapsed().as_secs_f64();
        if let Some(t1) = r {
            ok += 1;
            tbytes += t1.len();
            ts.push((t1, jpg));
        }
    }
    for _ in 0..3 {
        let t = Instant::now();
        for (t1, jpg) in &ts {
            let mut out = vec![0u8; jpg.len()];
            assert!(gpk::jpgx::inverse(t1, &mut out) && out == *jpg);
        }
        ti = ti.min(t.elapsed().as_secs_f64());
    }
    println!(
        "{} JPEGs ({:.2} MB -> {:.2} MB), {} ok | forward {:.2} s, inverse {:.3} s",
        files.len(),
        bytes as f64 / 1e6,
        tbytes as f64 / 1e6,
        ok,
        tf,
        ti
    );
}
