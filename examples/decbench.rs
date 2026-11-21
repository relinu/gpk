//! Decode-only timing (minimum over several runs): decbench <tcm|lzrc> <file> [runs]
//! The compressed LZRC stream is cached next to the input (`<file>.lzrc`), packing is slow.
use std::time::Instant;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (codec, path) = (args[1].as_str(), &args[2]);
    let runs: usize = args.get(3).map_or(5, |s| s.parse().unwrap());
    let d = std::fs::read(path).unwrap();
    let z = match codec {
        "tcm" => gpk::tcm::compress(&d),
        "lzrc" => {
            let zp = format!("{path}.lzrc");
            std::fs::read(&zp).unwrap_or_else(|_| {
                let z = gpk::lzrc::compress(&d, &gpk::lzrc::Params::level(9));
                std::fs::write(&zp, &z).unwrap();
                z
            })
        }
        _ => panic!("codec: tcm or lzrc"),
    };
    let mut out = vec![0u8; d.len()];
    let mut best = f64::MAX;
    for _ in 0..runs {
        let t = Instant::now();
        match codec {
            "tcm" => gpk::tcm::decompress(&z, &mut out),
            _ => gpk::lzrc::decompress(&z, &mut out).unwrap(),
        }
        best = best.min(t.elapsed().as_secs_f64());
    }
    assert!(out == d, "roundtrip failed");
    println!("{codec}: {} B -> {} B, decode {:.3} s ({:.2} MB/s)", d.len(), z.len(), best, d.len() as f64 / 1e6 / best);
}
