//! Kerne Rust gegen C messen: `cargo run --release --features cref --example prof -- <build-ordner>`
use gpk::cref::*;
use gpk::{lzrc, tcm};
use std::time::Instant;

fn collect(dir: &std::path::Path, ext: &[&str], out: &mut Vec<u8>, max: usize) {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        if out.len() >= max {
            return;
        }
        if p.is_dir() {
            collect(&p, ext, out, max);
        } else if p.extension().is_some_and(|e| ext.iter().any(|x| e == *x)) {
            out.extend(std::fs::read(&p).unwrap());
        }
    }
}

fn t<R>(f: impl FnOnce() -> R) -> (R, f64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64())
}

fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("Build-Ordner"));
    let level: i32 = std::env::args().nth(2).map(|s| s.parse().unwrap()).unwrap_or(9);
    let mut exe = std::fs::read(root.join("MyGame.x86_64")).unwrap();
    exe.truncate(12 << 20);
    let mut text = Vec::new();
    collect(&root, &["lua", "json", "txt", "xml", "cfg", "shader", "glsl", "hlsl", "csv", "ini", "md", "js", "py"], &mut text, 8 << 20);
    text.truncate(8 << 20);
    let mut mesh = Vec::new();
    collect(&root, &["mesh"], &mut mesh, 12 << 20);
    mesh.truncate(12 << 20);
    for (name, d) in [("x86", &exe), ("mesh", &mesh), ("text", &text)] {
        let mb = d.len() as f64 / 1e6;
        let p = lzrc::Params::level(level);
        let (rz, rt) = t(|| lzrc::compress(d, &p));
        let mut cp = CLzrcParams::default();
        unsafe { lzrc_default_params(&mut cp, level) };
        let mut cbuf = vec![0u8; unsafe { lzrc_bound(d.len()) }];
        let (cn, ct) = t(|| unsafe { lzrc_compress(d.as_ptr(), d.len(), cbuf.as_mut_ptr(), cbuf.len(), &cp) });
        let mut out = vec![0u8; d.len()];
        let (_, rd) = t(|| lzrc::decompress(&rz, &mut out));
        let (_, cd) = t(|| unsafe { lzrc_decompress(cbuf.as_ptr(), cn, out.as_mut_ptr(), d.len()) });
        println!(
            "{name:5} {mb:5.1} MB  LZRC{level} {:6.2} MB | packen R {:5.2} MB/s C {:5.2} MB/s | entpacken R {:6.1} MB/s C {:6.1} MB/s{}",
            rz.len() as f64 / 1e6,
            mb / rt,
            mb / ct,
            mb / rd,
            mb / cd,
            if rz.len() == cn { "" } else { "  (GRÖSSE ABWEICHEND)" }
        );
        if name == "text" {
            let (tz, rt) = t(|| tcm::compress(d));
            let mut tb = vec![0u8; unsafe { tcm_bound(d.len()) }];
            let (tn, ct) = t(|| unsafe { tcm_compress(d.as_ptr(), d.len(), tb.as_mut_ptr(), tb.len()) });
            let (_, rd) = t(|| tcm::decompress(&tz, &mut out));
            let (_, cd) = t(|| unsafe { tcm_decompress(tb.as_ptr(), tn, out.as_mut_ptr(), d.len()) });
            println!(
                "{name:5} {mb:5.1} MB  TCM    {:6.2} MB | packen R {:5.2} MB/s C {:5.2} MB/s | entpacken R {:6.1} MB/s C {:6.1} MB/s",
                tz.len() as f64 / 1e6,
                mb / rt,
                mb / ct,
                mb / rd,
                mb / cd
            );
        }
    }
}
