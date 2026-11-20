//! Kommandozeile von gpk (gleiche Optionen und Ausgaben wie die C-Version).

use gpk::archive::{self, Options, CODEC_XZ};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "Verwendung:
  gpk c [Optionen] <build-ordner> <archiv.gpk>
  gpk x [-t N] <archiv.gpk> <ziel-ordner>
  gpk l <archiv.gpk>
Optionen beim Packen:
  -l N         Stufe 1..9 (Standard 9)
  -t N         Threads (Standard 4)
  -b MiB       Blockgröße (Standard 64)
  --max-ram GB Speicherbudget: Threads und ggf. Blockgröße werden passend gewählt
  --xz         xz/LZMA2 statt LZRC als Kern (Vergleich)
  --no-cm      Text-Kern (Context Mixing) aus: schneller entpacken, Text größer
  --no-filter --no-sort --no-dedup --no-raw   einzelne Stufen abschalten
  --no-preproc alle Vorverarbeitungsstufen abschalten
  -V, --version Version ausgeben";

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
fn die(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("gpk: {msg}");
    ExitCode::from(1)
}
/// Ganzzahl wie C `atoi` lesen, aber Fehler melden statt still 0 anzunehmen.
fn num(s: &str, what: &str) -> Result<i64, String> {
    s.trim().parse::<i64>().map_err(|_| format!("{what}: ungültige Zahl '{s}'"))
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
                    _ => Err("--max-ram braucht einen Wert > 0 (GB)".into()),
                }
            }
            "--xz" => {
                if cfg!(feature = "xz") {
                    opt.codec = CODEC_XZ;
                    Ok(())
                } else {
                    Err("--xz ist in diesem Build nicht verfügbar (ohne liblzma gebaut)".into())
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
        return die("Blockgröße muss 1..1024 MiB sein");
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
