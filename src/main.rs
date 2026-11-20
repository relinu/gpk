//! Command line of gpk (same options as the C version).

use gpk::archive::{self, Options, CODEC_XZ};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "Usage:
  gpk c [options] <build-dir> <archive.gpk>
  gpk x [-t N] <archive.gpk> <target-dir>
  gpk l <archive.gpk>
Options for packing:
  -l N         level 1..9 (default 9)
  -t N         threads (default 4)
  -b MiB       block size (default 64)
  --max-ram GB memory budget: threads and, if needed, block size are chosen to fit
  --xz         xz/LZMA2 instead of LZRC as the core (comparison; needs --features xz)
  --no-cm      text core (context mixing) off: faster unpacking, larger text
  --no-filter --no-sort --no-dedup --no-raw   disable individual stages
  --no-preproc disable all preprocessing stages
  -V, --version print the version";

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
fn die(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("gpk: {msg}");
    ExitCode::from(1)
}
/// Parse an integer like C `atoi`, but report errors instead of silently assuming 0.
fn num(s: &str, what: &str) -> Result<i64, String> {
    s.trim().parse::<i64>().map_err(|_| format!("{what}: invalid number '{s}'"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        return usage();
    }
    let cmd = args[1].as_str();
    if cmd == "-V" || cmd == "--version" {
        println!("gpk {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let mut opt = Options::default();
    let mut block_mib: i64 = 64;
    let mut threads: i64 = 4;
    let mut pos: Vec<&str> = Vec::new();
    let mut i = 2;
    while i < args.len() {
        let s = args[i].as_str();
        let has_val = i + 1 < args.len();
        let r: Result<(), String> = match s {
            "-l" if has_val => {
                i += 1;
                num(&args[i], "-l").map(|v| opt.level = v.clamp(0, 9) as i32)
            }
            "-t" if has_val => {
                i += 1;
                num(&args[i], "-t").map(|v| threads = v)
            }
            "-b" if has_val => {
                i += 1;
                num(&args[i], "-b").map(|v| block_mib = v)
            }
            "--max-ram" if has_val => {
                i += 1;
                match args[i].trim().parse::<f64>() {
                    Ok(v) if v > 0.0 && v.is_finite() => {
                        opt.max_ram_gb = v;
                        Ok(())
                    }
                    _ => Err("--max-ram needs a value > 0 (GB)".into()),
                }
            }
            "--xz" => {
                if cfg!(feature = "xz") {
                    opt.codec = CODEC_XZ;
                    Ok(())
                } else {
                    Err("--xz is not available in this build (built without liblzma; use --features xz)".into())
                }
            }
            "--no-filter" => {
                opt.filter = false;
                Ok(())
            }
            "--no-cm" => {
                opt.cm = false;
                Ok(())
            }
            "--no-sort" => {
                opt.sort = false;
                Ok(())
            }
            "--no-dedup" => {
                opt.dedup = false;
                Ok(())
            }
            "--no-raw" => {
                opt.raw = false;
                Ok(())
            }
            "--no-preproc" => {
                opt.filter = false;
                opt.sort = false;
                opt.dedup = false;
                opt.raw = false;
                Ok(())
            }
            _ if pos.len() < 2 && !(s.starts_with('-') && s.len() > 1) => {
                pos.push(s);
                Ok(())
            }
            _ => return usage(),
        };
        if let Err(e) = r {
            return die(e);
        }
        i += 1;
    }
    if !(1..=1024).contains(&block_mib) {
        return die("block size must be 1..1024 MiB");
    }
    opt.block = (block_mib as u64) << 20;
    opt.threads = threads.clamp(1, 64) as usize;

    let res = match (cmd, pos.len()) {
        ("c", 2) => archive::compress(Path::new(pos[0]), Path::new(pos[1]), &opt),
        ("x", 2) => archive::extract(Path::new(pos[0]), Path::new(pos[1]), opt.threads),
        ("l", 1) => archive::list(Path::new(pos[0])),
        _ => return usage(),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => die(e),
    }
}
