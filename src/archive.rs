//! Archivformat, Packen, Entpacken und Auflisten (Port von `legacy-c/gpk/gpk.c`).
//!
//! Format (Little Endian, identisch zur C-Version):
//! `"GPK1" | u64 index_off | u64 index_csize | u64 index_usize | u8 index_codec | Blöcke | Index`
//!
//! Gleiche Eingabe und Optionen ergeben bytegleiche Archive wie die C-Version.
//! Dateien werden nicht eingeblendet (kein mmap), sondern bereichsweise gelesen:
//! kein `unsafe`, und der RAM-Bedarf bleibt unabhängig von der Build-Größe.

use crate::filters::{self, FSpec, FILTER_NAMES, FT_BCJ, FT_COUNT, FT_DELTA, FT_NONE, FT_SHUF, FT_SHUFDELTA, FT_SPLIT, FT_WPRED, FT_X86S};
use crate::{glz, jpgx, lzrc, pngx, tcm};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

const NONE: u32 = u32::MAX;
const SAMPLE: usize = 384 << 10;
const HDR_SIZE: usize = 4 + 8 + 8 + 8 + 1;
const MAX_XFORM_SSIZE: u64 = 1 << 30;

pub const CODEC_STORED: u8 = 0;
pub const CODEC_LZRC: u8 = 1;
pub const CODEC_XZ: u8 = 2;
pub const CODEC_TCM: u8 = 3;

const CL_TEXT: usize = 0;
const CL_EXE: usize = 1;
const CL_TEXTURE: usize = 2;
const CL_BINARY: usize = 3;
const CL_RAW: usize = 4;
const CL_N: usize = 5;
const CLASS_NAMES: [&str; CL_N] = ["Text/Skripte", "x86-Programmcode", "BCn-Texturen (DDS)", "sonstige Binärdaten", "bereits komprimiert"];

const E_FILE: u8 = 0;
const E_DIR: u8 = 1;
const E_LINK: u8 = 2;
const XF_NONE: u8 = 0;
const XF_PNG: u8 = 1;
const XF_JPG: u8 = 2;

/// Fehler mit verständlicher Meldung (wird von `main` ausgegeben).
#[derive(Debug)]
pub struct Error(pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}
fn io_err(ctx: &str, p: &Path, e: io::Error) -> Error {
    Error(format!("{ctx}: {} ({e})", p.display()))
}

/// Optionen beim Packen.
#[derive(Clone, Debug)]
pub struct Options {
    pub level: i32,
    pub threads: usize,
    pub block: u64,
    pub max_ram_gb: f64,
    pub codec: u8,
    pub filter: bool,
    pub sort: bool,
    pub dedup: bool,
    pub raw: bool,
    pub cm: bool,
}
impl Default for Options {
    fn default() -> Self {
        Options {
            level: 9,
            threads: 4,
            block: 64 << 20,
            max_ram_gb: 0.0,
            codec: CODEC_LZRC,
            filter: true,
            sort: true,
            dedup: true,
            raw: true,
            cm: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Datenstrukturen
// ---------------------------------------------------------------------------
#[derive(Clone, Debug, Default)]
struct Entry {
    path: Vec<u8>, // relativ, '/' als Trenner, Bytes wie im Archiv
    typ: u8,
    mode: u32,
    mtime: i64,
    size: u64,
    crc: u32,
    content: u32,
    link: Vec<u8>,
}
#[derive(Clone, Debug, Default)]
struct Content {
    size: u64,
    crc: u32,
    f: FSpec,
    cls: usize,
    path: Vec<u8>,
    xform: u8,
    ssize: u64,
    spill_off: u64,
}
impl Content {
    fn base(&self) -> &[u8] {
        match self.path.iter().rposition(|&b| b == b'/') {
            Some(i) => &self.path[i + 1..],
            None => &self.path,
        }
    }
    fn ext(&self) -> &[u8] {
        let b = self.base();
        match b.iter().rposition(|&c| c == b'.') {
            Some(i) => &b[i + 1..],
            None => b"",
        }
    }
}
#[derive(Clone, Copy, Debug)]
struct Piece {
    content: u32,
    off: u64,
    len: u32,
}
#[derive(Clone, Debug, Default)]
struct Block {
    pieces: Vec<Piece>,
    usize: u64,
    off: u64,
    csize: u64,
    codec: u8,
}

// ---------------------------------------------------------------------------
// Plattform: Pfade, positioniertes Lesen/Schreiben
// ---------------------------------------------------------------------------
#[cfg(unix)]
fn path_from_bytes(root: &Path, rel: &[u8]) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    Ok(root.join(std::ffi::OsStr::from_bytes(rel)))
}
#[cfg(not(unix))]
fn path_from_bytes(root: &Path, rel: &[u8]) -> Result<PathBuf> {
    match std::str::from_utf8(rel) {
        Ok(s) => Ok(root.join(s)),
        Err(_) => err(format!("Pfad ist kein UTF-8: {}", String::from_utf8_lossy(rel))),
    }
}
#[cfg(unix)]
fn name_bytes(n: &std::ffi::OsStr) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Ok(n.as_bytes().to_vec())
}
#[cfg(not(unix))]
fn name_bytes(n: &std::ffi::OsStr) -> Result<Vec<u8>> {
    match n.to_str() {
        Some(s) => Ok(s.as_bytes().to_vec()),
        None => err(format!("Dateiname nicht als Unicode darstellbar: {}", n.to_string_lossy())),
    }
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}
#[cfg(windows)]
fn read_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, off)?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        buf = &mut buf[n..];
        off += n as u64;
    }
    Ok(())
}
#[cfg(unix)]
fn write_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, off)
}
#[cfg(windows)]
fn write_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn portable_mode(md: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o7777
}
/// Windows kennt keine Unix-Rechte: portable Standardwerte (Ordner 0755, Dateien 0644, schreibgeschützt 0444).
#[cfg(not(unix))]
fn portable_mode(md: &fs::Metadata) -> u32 {
    if md.is_dir() {
        0o755
    } else if md.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}
fn mtime_secs(md: &fs::Metadata) -> i64 {
    match md.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64) - (e.duration().subsec_nanos() > 0) as i64,
        },
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// Parallelität
// ---------------------------------------------------------------------------
fn parallel_for(n: usize, threads: usize, f: impl Fn(usize) + Sync) {
    let next = AtomicUsize::new(0);
    let t = threads.clamp(1, 64).min(n.max(1));
    std::thread::scope(|s| {
        for _ in 0..t {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                f(i);
            });
        }
    });
}
/// Paralleles Map mit Ergebnissen in Eingabereihenfolge.
fn parallel_map<T: Send>(n: usize, threads: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let out: Mutex<Vec<Option<T>>> = Mutex::new((0..n).map(|_| None).collect());
    parallel_for(n, threads, |i| {
        let v = f(i);
        out.lock().unwrap()[i] = Some(v);
    });
    out.into_inner().unwrap().into_iter().map(|v| v.expect("Ergebnis fehlt")).collect()
}

// ---------------------------------------------------------------------------
// Scannen, Hashen, Deduplizieren
// ---------------------------------------------------------------------------
fn scan(root: &Path, rel: &[u8], out: &mut Vec<Entry>) -> Result<()> {
    let dir = path_from_bytes(root, rel)?;
    let mut names = Vec::new();
    for de in fs::read_dir(&dir).map_err(|e| io_err("Kann Ordner nicht öffnen", &dir, e))? {
        let de = de.map_err(|e| io_err("Kann Ordner nicht lesen", &dir, e))?;
        names.push(name_bytes(&de.file_name())?);
    }
    names.sort();
    for name in names {
        let mut r = rel.to_vec();
        if !r.is_empty() {
            r.push(b'/');
        }
        r.extend_from_slice(&name);
        let f = path_from_bytes(root, &r)?;
        let md = if cfg!(unix) { fs::symlink_metadata(&f) } else { fs::metadata(&f) }.map_err(|e| io_err("stat fehlgeschlagen", &f, e))?;
        let mut e = Entry { path: r.clone(), mode: portable_mode(&md), mtime: mtime_secs(&md), content: NONE, ..Default::default() };
        let ft = md.file_type();
        if ft.is_dir() {
            e.typ = E_DIR;
            out.push(e);
            scan(root, &r, out)?;
        } else if ft.is_file() {
            e.typ = E_FILE;
            e.size = md.len();
            out.push(e);
        } else if ft.is_symlink() {
            let t = fs::read_link(&f).map_err(|e| io_err("readlink fehlgeschlagen", &f, e))?;
            e.typ = E_LINK;
            e.link = name_bytes(t.as_os_str())?;
            out.push(e);
        }
    }
    Ok(())
}

fn crc_file(p: &Path) -> Result<u32> {
    let mut f = File::open(p).map_err(|e| io_err("Kann Datei nicht lesen", p, e))?;
    let mut h = crc32fast::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| io_err("Lesefehler", p, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize())
}
fn same_file(a: &Path, b: &Path) -> Result<bool> {
    let mut fa = File::open(a).map_err(|e| io_err("Kann Datei nicht lesen", a, e))?;
    let mut fb = File::open(b).map_err(|e| io_err("Kann Datei nicht lesen", b, e))?;
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let na = fa.read(&mut ba).map_err(|e| io_err("Lesefehler", a, e))?;
        if na == 0 {
            return Ok(fb.read(&mut bb[..1]).map_err(|e| io_err("Lesefehler", b, e))? == 0);
        }
        fb.read_exact(&mut bb[..na]).map_err(|e| io_err("Lesefehler", b, e))?;
        if ba[..na] != bb[..na] {
            return Ok(false);
        }
    }
}

// ---------------------------------------------------------------------------
// Klassifikation & Filterwahl
// ---------------------------------------------------------------------------
fn is_text(s: &[u8]) -> bool {
    let s = &s[..s.len().min(65536)];
    if s.is_empty() || s.contains(&0) {
        return false;
    }
    let good = s.iter().filter(|&&c| c >= 32 || c == b'\n' || c == b'\r' || c == b'\t').count();
    good >= s.len() * 97 / 100
}
fn le16(d: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([d[i], d[i + 1]])
}
fn le32(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])
}
/// x86/x64-Programm (ELF oder PE)? `read` liest Bytes an beliebiger Position.
fn is_x86(head: &[u8], n: u64, read: &dyn Fn(u64, usize) -> Option<Vec<u8>>) -> bool {
    if n >= 20 && head.len() >= 20 && &head[..4] == b"\x7fELF" {
        let m = le16(head, 18);
        return m == 3 || m == 62;
    }
    if n >= 64 && head.len() >= 64 && head[0] == b'M' && head[1] == b'Z' {
        let pe = le32(head, 0x3C) as u64;
        if pe + 6 <= n {
            if let Some(b) = read(pe, 6) {
                if &b[..4] == b"PE\0\0" {
                    let m = le16(&b, 4);
                    return m == 0x14c || m == 0x8664;
                }
            }
        }
    }
    false
}
/// DDS mit BC1..BC5: Feld-Split (Endpunkte vs. Indizes).
fn dds_split(d: &[u8], n: u64) -> Option<FSpec> {
    if n < 128 || d.len() < 128 || &d[..4] != b"DDS " {
        return None;
    }
    let cc = &d[84..88];
    let (mut skip, mut dxgi) = (128u32, 0u32);
    if cc == b"DX10" {
        if n < 148 || d.len() < 148 {
            return None;
        }
        dxgi = le32(d, 128);
        skip = 148;
    }
    let fl: &[u8] = if cc == b"DXT1" || (70..=72).contains(&dxgi) {
        &[4, 4]
    } else if cc == b"DXT3" || (73..=75).contains(&dxgi) {
        &[8, 4, 4]
    } else if cc == b"DXT5" || (76..=78).contains(&dxgi) {
        &[2, 6, 4, 4]
    } else if cc == b"ATI1" || cc == b"BC4U" || (79..=81).contains(&dxgi) {
        &[2, 6]
    } else if cc == b"ATI2" || cc == b"BC5U" || (82..=84).contains(&dxgi) {
        &[2, 6, 2, 6]
    } else {
        return None;
    };
    let mut f = FSpec { id: FT_SPLIT, skip, nfields: fl.len() as u8, ..Default::default() };
    f.fields[..fl.len()].copy_from_slice(fl);
    Some(f)
}
fn delta_entropy(p: &[u8], s: usize) -> f64 {
    let mut h = [0u32; 256];
    for i in s..p.len() {
        h[p[i].wrapping_sub(p[i - s]) as usize] += 1;
    }
    let tot = (p.len() - s) as f64;
    let mut e = 0.0;
    for &c in h.iter() {
        if c != 0 {
            let q = c as f64 / tot;
            e -= c as f64 * q.log2();
        }
    }
    e
}
fn lz_size(d: &[u8]) -> usize {
    lzrc::compress(d, &lzrc::Params::level(2)).len()
}
/// Größenschätzung nach LZRC; für PNG-Rohdaten zusätzlich mit den naheliegenden Filtern.
fn lz_estimate(d: &[u8], bpp: usize) -> usize {
    let mut best = lz_size(d);
    if bpp > 1 {
        let mut tmp = vec![0u8; d.len()];
        for id in [FT_DELTA, FT_SHUFDELTA] {
            filters::encode(&FSpec::with_stride(id, bpp as u8), d, &mut tmp, 0);
            best = best.min(lz_size(&tmp));
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Packen
// ---------------------------------------------------------------------------
struct Spill {
    path: PathBuf,
    w: Mutex<Option<(File, u64)>>,
}
impl Spill {
    fn write(&self, d: &[u8]) -> Result<u64> {
        let mut g = self.w.lock().unwrap();
        if g.is_none() {
            let f = File::create(&self.path).map_err(|e| io_err("Kann Auslagerungsdatei nicht anlegen", &self.path, e))?;
            *g = Some((f, 0));
        }
        let (f, end) = g.as_mut().unwrap();
        let off = *end;
        write_at(f, d, off).map_err(|e| io_err("Schreibfehler in Auslagerungsdatei", &self.path, e))?;
        *end += d.len() as u64;
        Ok(off)
    }
}

struct Packer<'a> {
    root: &'a Path,
    opt: &'a Options,
    spill: Spill,
    spill_read: Option<File>,
}
impl<'a> Packer<'a> {
    fn orig_path(&self, c: &Content) -> Result<PathBuf> {
        path_from_bytes(self.root, &c.path)
    }
    /// Bereich des zu komprimierenden Datenstroms (Original oder ausgelagerte Umwandlung).
    fn read_stream(&self, c: &Content, off: u64, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        if c.xform != XF_NONE {
            let f = self.spill_read.as_ref().ok_or_else(|| Error("Auslagerungsdatei fehlt".into()))?;
            read_at(f, &mut buf, c.spill_off + off).map_err(|e| io_err("Lesefehler", &self.spill.path, e))?;
        } else {
            let p = self.orig_path(c)?;
            let f = File::open(&p).map_err(|e| io_err("Kann Datei nicht lesen", &p, e))?;
            read_at(&f, &mut buf, off).map_err(|e| io_err("Lesefehler (Datei verändert?)", &p, e))?;
        }
        Ok(buf)
    }

    /// PNG-/JPEG-Rekompression versuchen (nur übernehmen, wenn sie gewinnt).
    fn xform_one(&self, c: &Content) -> Result<Option<(u8, u64, u64)>> {
        if c.size < 64 {
            return Ok(None);
        }
        let head = self.read_stream(c, 0, 3)?;
        let is_jpg = head == [0xFF, 0xD8, 0xFF];
        let is_png = head[0] == 0x89 && &head[1..3] == b"PN";
        if !is_jpg && !is_png {
            return Ok(None);
        }
        let data = self.read_stream(c, 0, c.size as usize)?;
        if is_jpg {
            let Some(t) = jpgx::forward(&data) else { return Ok(None) };
            let tn = t.len() as u64;
            let mut base = c.size as f64;
            if tn <= self.opt.block && (tn as f64) <= c.size as f64 * 0.97 && c.size < (1 << 20) {
                let lz = lz_estimate(&data, 0) as f64;
                if lz < base {
                    base = lz;
                }
            }
            if tn > self.opt.block || tn as f64 > base * 0.97 {
                return Ok(None);
            }
            let off = self.spill.write(&t)?;
            return Ok(Some((XF_JPG, tn, off)));
        }
        if &data[1..4] != b"PNG" {
            return Ok(None);
        }
        let bpp = pngx::channels(&data);
        let t1 = pngx::forward(&data, 1);
        let t2 = if t1.is_some() { pngx::forward(&data, 2) } else { None };
        let z1 = match &t1 {
            Some(t) if t.len() as u64 <= self.opt.block => lz_estimate(t, bpp),
            _ => usize::MAX,
        };
        let z2 = match &t2 {
            Some(t) if t.len() as u64 <= self.opt.block => t.len(),
            _ => usize::MAX,
        };
        let (t, z) = if z2 < z1 { (t2, z2) } else { (t1, z1) };
        let Some(t) = t else { return Ok(None) };
        if z == usize::MAX || z as f64 > c.size as f64 * 0.97 {
            return Ok(None);
        }
        let off = self.spill.write(&t)?;
        Ok(Some((XF_PNG, t.len() as u64, off)))
    }

    /// Klasse und Filter je Inhalt (Probekompression mit LZRC).
    fn classify_one(&self, c: &Content) -> Result<(usize, FSpec)> {
        let n = c.ssize;
        let none = FSpec::default();
        if n == 0 {
            return Ok((CL_BINARY, none));
        }
        let head = self.read_stream(c, 0, n.min(65536) as usize)?;
        if is_text(&head) {
            return Ok((CL_TEXT, none));
        }
        let opt = self.opt;
        if (!opt.filter && !opt.raw) || n < 4096 {
            return Ok((CL_BINARY, none));
        }
        let sn = n.min(SAMPLE as u64) as usize;
        let soff = if n > 2 * SAMPLE as u64 { (n / 2 - SAMPLE as u64 / 2) & !63 } else { 0 };
        let s = self.read_stream(c, soff, sn)?;
        // Schneller Vortest: bereits komprimiert?
        let g = glz::compress(&s, 1, true).len();
        if opt.raw && g as f64 > sn as f64 * 0.99 {
            return Ok((CL_RAW, none));
        }
        let mut cls = CL_BINARY;
        let mut cand: Vec<FSpec> = vec![none];
        let mut add = |f: FSpec| {
            if !cand.contains(&f) {
                cand.push(f);
            }
        };
        if opt.filter {
            let reader = |off: u64, len: usize| self.read_stream(c, off, len).ok();
            if is_x86(&head, n, &reader) {
                cls = CL_EXE;
                add(FSpec::new(FT_BCJ));
                add(FSpec::new(FT_X86S));
            } else if let Some(t) = dds_split(&head, n) {
                cls = CL_TEXTURE;
                add(t);
            } else {
                // Stride-Erkennung: die zwei Abstände mit der kleinsten Delta-Entropie
                let en = sn.min(65536);
                let (mut st, mut be) = ([1usize, 1], [1e300f64, 1e300]);
                for k in 1..=64usize {
                    let e = delta_entropy(&s[..en], k) * (1.0 + 0.002 * k as f64);
                    if e < be[0] {
                        be[1] = be[0];
                        st[1] = st[0];
                        be[0] = e;
                        st[0] = k;
                    } else if e < be[1] {
                        be[1] = e;
                        st[1] = k;
                    }
                }
                for &sv in &st {
                    let sb = sv as u8;
                    add(FSpec::with_stride(FT_DELTA, sb));
                    if sv > 1 {
                        add(FSpec::with_stride(FT_SHUFDELTA, sb));
                        add(FSpec::with_stride(FT_SHUF, sb));
                    }
                    for w in [4usize, 2] {
                        if sv % w != 0 {
                            continue;
                        }
                        for o in 1..=2u8 {
                            let mut t = FSpec::with_stride(FT_WPRED, sb);
                            t.fields[0] = w as u8;
                            t.fields[1] = o;
                            t.fields[2] = (sv > w) as u8;
                            add(t);
                        }
                    }
                }
            }
        }
        let (mut best, mut none_size, mut chosen) = (usize::MAX, 0usize, none);
        let mut tmp = vec![0u8; sn];
        for (k, f) in cand.iter().enumerate() {
            filters::encode(f, &s, &mut tmp, soff);
            let z = lz_size(&tmp);
            if k == 0 {
                none_size = z;
            }
            if z < best {
                best = z;
                chosen = *f;
            }
        }
        if chosen.id != FT_NONE && best as f64 > none_size as f64 * 0.995 {
            chosen = none;
            best = none_size;
        }
        if opt.raw && best as f64 > sn as f64 * 0.98 {
            return Ok((CL_RAW, none));
        }
        Ok((cls, chosen))
    }
}

fn class_rank(cls: usize) -> i32 {
    [1, 0, 2, 3, 4][cls]
}

fn est_ram(opt: &Options, threads: usize, block: u64) -> f64 {
    let per = if opt.codec == CODEC_XZ {
        674.0 * (1u64 << 20) as f64 + 2.1 * block as f64
    } else {
        10.2 * block as f64 + 6.0 * (1u64 << 20) as f64
    };
    threads as f64 * per + (threads + 1) as f64 * block as f64 + 96.0 * (1u64 << 20) as f64
}
fn plan_memory(opt: &mut Options) {
    if opt.max_ram_gb > 0.0 {
        let budget = opt.max_ram_gb * 1e9;
        while opt.threads > 1 && est_ram(opt, opt.threads, opt.block) > budget {
            opt.threads -= 1;
        }
        while est_ram(opt, opt.threads, opt.block) > budget && opt.block > (4 << 20) {
            opt.block /= 2;
        }
        if est_ram(opt, opt.threads, opt.block) > budget {
            eprintln!("gpk: Warnung: --max-ram {:.1} GB ist auch mit 1 Thread und 4 MiB Blöcken nicht einhaltbar", opt.max_ram_gb);
        }
    }
    eprintln!(
        "Speicherplanung: {} Threads, Blockgröße {} MiB, geschätzter Spitzen-RAM ~{:.0} MB{}",
        opt.threads,
        opt.block >> 20,
        est_ram(opt, opt.threads, opt.block) / 1e6,
        if opt.max_ram_gb > 0.0 { "" } else { " (begrenzen mit --max-ram GB)" }
    );
}

fn put_str(ix: &mut Vec<u8>, s: &[u8]) -> Result<()> {
    let l = u16::try_from(s.len()).map_err(|_| Error(format!("Pfad zu lang: {}", String::from_utf8_lossy(s))))?;
    ix.extend_from_slice(&l.to_le_bytes());
    ix.extend_from_slice(s);
    Ok(())
}
fn write_fspec(ix: &mut Vec<u8>, f: &FSpec) {
    ix.extend_from_slice(&[f.id, f.stride, f.nfields]);
    ix.extend_from_slice(&f.fields);
    ix.extend_from_slice(&f.skip.to_le_bytes());
}

/// Geordneter Writer: fertige Blöcke gehen sofort in fester Reihenfolge ins Archiv.
struct Writer {
    out: File,
    off: u64,
    next: usize,
    pending: Vec<Option<(u8, Vec<u8>)>>,
    placed: Vec<(u64, u64, u8)>, // (off, csize, codec)
    err: Option<Error>,
}

/// Packt den Ordner `dir` nach `outpath`.
pub fn compress(dir: &Path, outpath: &Path, opt_in: &Options) -> Result<()> {
    let t0 = Instant::now();
    let mut opt = opt_in.clone();
    plan_memory(&mut opt);
    let opt = &opt;
    let mut entries = Vec::new();
    scan(dir, b"", &mut entries)?;

    // CRC aller Dateien (parallel)
    let crcs = parallel_map(entries.len(), opt.threads, |i| {
        let e = &entries[i];
        if e.typ != E_FILE || e.size == 0 {
            return Ok(0);
        }
        crc_file(&path_from_bytes(dir, &e.path)?)
    });
    for (e, c) in entries.iter_mut().zip(crcs) {
        e.crc = c?;
    }

    // Deduplizieren
    let mut fi: Vec<usize> = (0..entries.len()).filter(|&i| entries[i].typ == E_FILE && entries[i].size > 0).collect();
    fi.sort_by(|&a, &b| {
        let (x, y) = (&entries[a], &entries[b]);
        x.size.cmp(&y.size).then(x.crc.cmp(&y.crc)).then(x.path.cmp(&y.path))
    });
    let mut contents: Vec<Content> = Vec::new();
    let (mut total, mut dup_bytes, mut dup_files) = (0u64, 0u64, 0u32);
    let mut group_start = 0usize;
    for k in 0..fi.len() {
        let ei = fi[k];
        total += entries[ei].size;
        if k > 0 && (entries[fi[k - 1]].size != entries[ei].size || entries[fi[k - 1]].crc != entries[ei].crc) {
            group_start = contents.len();
        }
        let mut found = None;
        if opt.dedup {
            for (c, ct) in contents.iter().enumerate().skip(group_start) {
                if ct.size == entries[ei].size
                    && ct.crc == entries[ei].crc
                    && same_file(&path_from_bytes(dir, &ct.path)?, &path_from_bytes(dir, &entries[ei].path)?)?
                {
                    found = Some(c);
                    break;
                }
            }
        }
        if let Some(c) = found {
            entries[ei].content = c as u32;
            dup_bytes += entries[ei].size;
            dup_files += 1;
            continue;
        }
        let e = &entries[ei];
        contents.push(Content { size: e.size, crc: e.crc, path: e.path.clone(), ssize: e.size, ..Default::default() });
        entries[ei].content = (contents.len() - 1) as u32;
    }

    // Umwandlungen (PNG/JPEG), dann klassifizieren
    let mut spill_name = outpath.as_os_str().to_owned();
    spill_name.push(".auslagerung.tmp");
    let mut pk = Packer { root: dir, opt, spill: Spill { path: PathBuf::from(spill_name), w: Mutex::new(None) }, spill_read: None };
    if opt.filter {
        let xs = parallel_map(contents.len(), opt.threads, |i| pk.xform_one(&contents[i]));
        for (c, x) in contents.iter_mut().zip(xs) {
            if let Some((xf, ssize, off)) = x? {
                c.xform = xf;
                c.ssize = ssize;
                c.spill_off = off;
            }
        }
    }
    let spilled = pk.spill.w.lock().unwrap().take().is_some();
    if spilled {
        pk.spill_read = Some(File::open(&pk.spill.path).map_err(|e| io_err("Kann Auslagerungsdatei nicht lesen", &pk.spill.path, e))?);
    }
    let (mut npng, mut njpg, mut png_bytes, mut jpg_bytes, mut jpg_after) = (0u32, 0u32, 0u64, 0u64, 0u64);
    for c in &contents {
        if c.xform == XF_PNG {
            npng += 1;
            png_bytes += c.size;
        }
        if c.xform == XF_JPG {
            njpg += 1;
            jpg_bytes += c.size;
            jpg_after += c.ssize;
        }
    }
    let cls = parallel_map(contents.len(), opt.threads, |i| pk.classify_one(&contents[i]));
    for (c, r) in contents.iter_mut().zip(cls) {
        let (cl, f) = r?;
        c.cls = cl;
        c.f = f;
    }

    // Sortieren
    let mut order: Vec<usize> = (0..contents.len()).collect();
    if opt.sort {
        order.sort_by(|&a, &b| {
            let (x, y) = (&contents[a], &contents[b]);
            class_rank(x.cls)
                .cmp(&class_rank(y.cls))
                .then_with(|| x.ext().cmp(y.ext()))
                .then(x.f.id.cmp(&y.f.id))
                .then_with(|| x.base().cmp(y.base()))
                .then_with(|| x.path.cmp(&y.path))
        });
    }

    // Statistik
    let (mut cls_bytes, mut cls_files, mut filt_cnt) = ([0u64; CL_N], [0u32; CL_N], [0u32; FT_COUNT as usize]);
    for c in &contents {
        cls_bytes[c.cls] += c.size;
        cls_files[c.cls] += 1;
        filt_cnt[c.f.id as usize] += 1;
    }
    eprintln!("Build: {} Einträge, {:.1} MB in Dateien", entries.len(), total as f64 / 1e6);
    eprintln!("Deduplizierung: {dup_files} doppelte Dateien, {:.1} MB gespart", dup_bytes as f64 / 1e6);
    for k in 0..CL_N {
        if cls_files[k] > 0 {
            eprintln!("  {:<24} {:5} Dateien {:9.1} MB", CLASS_NAMES[k], cls_files[k], cls_bytes[k] as f64 / 1e6);
        }
    }
    if npng > 0 {
        eprintln!("PNG-Rekompression: {npng} Dateien ({:.1} MB) bitgenau umkehrbar", png_bytes as f64 / 1e6);
    }
    if njpg > 0 {
        eprintln!(
            "JPEG-Rekompression: {njpg} Dateien, {:.1} -> {:.1} MB ({:.1} %), bitgenau umkehrbar",
            jpg_bytes as f64 / 1e6,
            jpg_after as f64 / 1e6,
            100.0 * (jpg_after as f64 / jpg_bytes as f64 - 1.0)
        );
    }
    let filt: String =
        (0..FT_COUNT as usize).filter(|&k| filt_cnt[k] > 0).map(|k| format!(" {}={}", FILTER_NAMES[k], filt_cnt[k])).collect();
    eprintln!("Filter:{filt}");

    // Blöcke bilden
    let mut blocks: Vec<Block> = Vec::new();
    for raw in [false, true] {
        let mut cur: Option<usize> = None;
        let mut last_text = false;
        for &ci in &order {
            let c = &contents[ci];
            if (c.cls == CL_RAW) != raw {
                continue;
            }
            // Text mit Context-Mixing-Kern: 8-MiB-Blöcke (paralleles Entpacken)
            let cap = if opt.cm && opt.codec == CODEC_LZRC && c.cls == CL_TEXT && opt.block > (8 << 20) { 8 << 20 } else { opt.block };
            let mut off = 0u64;
            while off < c.ssize {
                let len = (c.ssize - off).min(cap) as u32;
                let is_text = c.cls == CL_TEXT;
                let need_new = match cur {
                    None => true,
                    Some(b) => {
                        (blocks[b].usize + len as u64 > cap && !blocks[b].pieces.is_empty())
                            || (!blocks[b].pieces.is_empty() && is_text != last_text)
                    }
                };
                if need_new {
                    blocks.push(Block { codec: if raw { CODEC_STORED } else { opt.codec }, ..Default::default() });
                    cur = Some(blocks.len() - 1);
                }
                last_text = is_text;
                let b = &mut blocks[cur.unwrap()];
                b.pieces.push(Piece { content: ci as u32, off, len });
                b.usize += len as u64;
                off += len as u64;
            }
        }
    }

    // Komprimieren (parallel) mit geordnetem, sofortigem Schreiben
    let mut out = File::create(outpath).map_err(|e| io_err("Kann Archiv nicht schreiben", outpath, e))?;
    out.write_all(&[0u8; HDR_SIZE]).map_err(|e| io_err("Schreibfehler", outpath, e))?;
    let nb = blocks.len();
    let ahead = opt.threads.max(1) + 2;
    let writer = Mutex::new(Writer {
        out,
        off: HDR_SIZE as u64,
        next: 0,
        pending: (0..nb).map(|_| None).collect(),
        placed: vec![(0, 0, 0); nb],
        err: None,
    });
    let cv = Condvar::new();
    let failed = AtomicBool::new(false);
    parallel_for(nb, opt.threads, |bi| {
        {
            // nicht zu weit vorauslaufen
            let mut w = writer.lock().unwrap();
            while bi >= w.next + ahead && w.err.is_none() {
                w = cv.wait(w).unwrap();
            }
            if w.err.is_some() {
                return;
            }
        }
        let res = (|| -> Result<(u8, Vec<u8>)> {
            let b = &blocks[bi];
            let mut buf = vec![0u8; b.usize as usize];
            let (mut pos, mut text) = (0usize, 0u64);
            for p in &b.pieces {
                let c = &contents[p.content as usize];
                let src = pk.read_stream(c, p.off, p.len as usize)?;
                filters::encode(&c.f, &src, &mut buf[pos..pos + p.len as usize], p.off);
                if c.cls == CL_TEXT {
                    text += p.len as u64;
                }
                pos += p.len as usize;
            }
            let mut codec = b.codec;
            let mut cdata: Option<Vec<u8>> = None;
            if codec == CODEC_LZRC {
                let mut pr = lzrc::Params::level(opt.level);
                if text * 2 > b.usize {
                    pr.lc = 4;
                    pr.pb = 0;
                }
                let lz = lzrc::compress(&buf, &pr);
                cdata = Some(lz);
                if opt.cm && text * 2 > b.usize {
                    let t = tcm::compress(&buf);
                    if t.len() < cdata.as_ref().unwrap().len() {
                        cdata = Some(t);
                        codec = CODEC_TCM;
                    }
                }
            } else if codec == CODEC_XZ {
                cdata = xz_compress(&buf)?;
            }
            Ok(match cdata {
                Some(c) if !c.is_empty() && (c.len() as u64) < b.usize => (codec, c),
                _ => (CODEC_STORED, buf),
            })
        })();
        let mut w = writer.lock().unwrap();
        match res {
            Ok((codec, data)) => {
                let b = &blocks[bi];
                let name = match codec {
                    CODEC_STORED => "gespeichert",
                    CODEC_LZRC => "LZRC",
                    CODEC_TCM => "Text-CM",
                    _ => "xz",
                };
                eprintln!("  Block {:3}/{}: {:7.1} MB -> {:7.1} MB  ({})", bi + 1, nb, b.usize as f64 / 1e6, data.len() as f64 / 1e6, name);
                w.pending[bi] = Some((codec, data));
                // alle lückenlos fertigen Blöcke schreiben und freigeben
                while w.next < nb && w.pending[w.next].is_some() {
                    let i = w.next;
                    let (codec, data) = w.pending[i].take().unwrap();
                    let off = w.off;
                    if let Err(e) = w.out.write_all(&data) {
                        w.err = Some(io_err("Schreibfehler (Platte voll?)", outpath, e));
                        failed.store(true, Ordering::Relaxed);
                        break;
                    }
                    w.off += data.len() as u64;
                    w.placed[i] = (off, data.len() as u64, codec);
                    w.next += 1;
                }
            }
            Err(e) => {
                w.err = Some(e);
                failed.store(true, Ordering::Relaxed);
            }
        }
        cv.notify_all();
    });
    let mut w = writer.into_inner().unwrap();
    if let Some(e) = w.err.take() {
        let _ = fs::remove_file(&pk.spill.path);
        return Err(e);
    }
    if w.next != nb {
        return err("interner Fehler: nicht alle Blöcke geschrieben");
    }
    for (b, &(off, csize, codec)) in blocks.iter_mut().zip(&w.placed) {
        b.off = off;
        b.csize = csize;
        b.codec = codec;
    }
    drop(pk.spill_read.take());
    if spilled {
        let _ = fs::remove_file(&pk.spill.path);
    }
    let data_end = w.off;

    // Index
    let mut ix = Vec::new();
    ix.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in &entries {
        ix.push(e.typ);
        put_str(&mut ix, &e.path)?;
        ix.extend_from_slice(&e.mode.to_le_bytes());
        ix.extend_from_slice(&e.mtime.to_le_bytes());
        ix.extend_from_slice(&e.size.to_le_bytes());
        ix.extend_from_slice(&e.content.to_le_bytes());
        if e.typ == E_LINK {
            put_str(&mut ix, &e.link)?;
        }
    }
    ix.extend_from_slice(&(contents.len() as u32).to_le_bytes());
    for c in &contents {
        ix.extend_from_slice(&c.size.to_le_bytes());
        ix.extend_from_slice(&c.crc.to_le_bytes());
        write_fspec(&mut ix, &c.f);
        ix.push(c.xform);
        ix.extend_from_slice(&c.ssize.to_le_bytes());
    }
    ix.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for b in &blocks {
        ix.extend_from_slice(&b.off.to_le_bytes());
        ix.extend_from_slice(&b.csize.to_le_bytes());
        ix.extend_from_slice(&b.usize.to_le_bytes());
        ix.push(b.codec);
        ix.extend_from_slice(&(b.pieces.len() as u32).to_le_bytes());
        for p in &b.pieces {
            ix.extend_from_slice(&p.content.to_le_bytes());
            ix.extend_from_slice(&p.off.to_le_bytes());
            ix.extend_from_slice(&p.len.to_le_bytes());
        }
    }
    let mut ic = lzrc::compress(&ix, &lzrc::Params::level(9));
    let mut icodec = CODEC_LZRC;
    if ic.is_empty() || ic.len() >= ix.len() {
        ic = ix.clone();
        icodec = CODEC_STORED;
    }
    let mut out = w.out;
    out.write_all(&ic).map_err(|e| io_err("Schreibfehler", outpath, e))?;
    let mut hdr = [0u8; HDR_SIZE];
    hdr[..4].copy_from_slice(b"GPK1");
    hdr[4..12].copy_from_slice(&data_end.to_le_bytes());
    hdr[12..20].copy_from_slice(&(ic.len() as u64).to_le_bytes());
    hdr[20..28].copy_from_slice(&(ix.len() as u64).to_le_bytes());
    hdr[28] = icodec;
    write_at(&out, &hdr, 0).map_err(|e| io_err("Schreibfehler", outpath, e))?;
    out.sync_all().ok();
    let arch = data_end + ic.len() as u64;
    let dt = t0.elapsed().as_secs_f64();
    eprintln!(
        "Archiv: {:.2} MB (Rate {:.3}, {:.1}% des Builds), Index {:.1} KB, {:.1} s ({:.1} MB/s)",
        arch as f64 / 1e6,
        total as f64 / arch as f64,
        100.0 * arch as f64 / total as f64,
        ic.len() as f64 / 1e3,
        dt,
        total as f64 / dt / 1e6
    );
    Ok(())
}

#[cfg(feature = "xz")]
fn xz_compress(buf: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut s = xz2::stream::Stream::new_easy_encoder(9 | 0x8000_0000, xz2::stream::Check::None).map_err(|e| Error(format!("xz: {e}")))?;
    let mut out = Vec::with_capacity(buf.len() + buf.len() / 2 + 65536);
    match s.process_vec(buf, &mut out, xz2::stream::Action::Finish) {
        Ok(xz2::stream::Status::StreamEnd) => Ok(Some(out)),
        _ => Ok(None),
    }
}
#[cfg(not(feature = "xz"))]
fn xz_compress(_: &[u8]) -> Result<Option<Vec<u8>>> {
    err("--xz ist in diesem Build nicht verfügbar (ohne liblzma gebaut)")
}
#[cfg(feature = "xz")]
fn xz_decompress(src: &[u8], out: &mut [u8]) -> bool {
    let Ok(mut s) = xz2::stream::Stream::new_stream_decoder(u64::MAX, 0) else { return false };
    let mut v = Vec::with_capacity(out.len());
    match s.process_vec(src, &mut v, xz2::stream::Action::Finish) {
        Ok(xz2::stream::Status::StreamEnd) if v.len() == out.len() => {
            out.copy_from_slice(&v);
            true
        }
        _ => false,
    }
}
#[cfg(not(feature = "xz"))]
fn xz_decompress(_: &[u8], _: &mut [u8]) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Archiv lesen
// ---------------------------------------------------------------------------
struct Archive {
    path: PathBuf,
    entries: Vec<Entry>,
    contents: Vec<Content>,
    blocks: Vec<Block>,
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.d.len() - self.p < n {
            return err("Index beschädigt");
        }
        self.p += n;
        Ok(&self.d[self.p - n..self.p])
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    fn str(&mut self) -> Result<Vec<u8>> {
        let l = self.u16()? as usize;
        Ok(self.bytes(l)?.to_vec())
    }
}

/// Pfad aus dem Archiv sicher? (relativ, keine '..'/'.'-Komponenten, unter Windows kein '\' oder ':')
fn safe_path(p: &[u8]) -> bool {
    if p.is_empty() || p[0] == b'/' || p.contains(&0) {
        return false;
    }
    if cfg!(windows) && p.iter().any(|&c| c == b'\\' || c == b':') {
        return false;
    }
    p.split(|&c| c == b'/').all(|c| !c.is_empty() && c != b"." && c != b"..")
}

fn load_archive(path: &Path) -> Result<Archive> {
    let f = File::open(path).map_err(|e| io_err("Kann Archiv nicht öffnen", path, e))?;
    let size = f.metadata().map_err(|e| io_err("Kann Archiv nicht öffnen", path, e))?.len();
    let mut hdr = [0u8; HDR_SIZE];
    if size < HDR_SIZE as u64 || read_at(&f, &mut hdr, 0).is_err() || &hdr[..4] != b"GPK1" {
        return err(format!("Kein GPK-Archiv: {}", path.display()));
    }
    let ioff = u64::from_le_bytes(hdr[4..12].try_into().unwrap());
    let ics = u64::from_le_bytes(hdr[12..20].try_into().unwrap());
    let ius = u64::from_le_bytes(hdr[20..28].try_into().unwrap());
    let icodec = hdr[28];
    if ioff > size || ics > size - ioff || ius > (1 << 32) || ics > (1 << 32) {
        return err("Index beschädigt");
    }
    let mut ic = vec![0u8; ics as usize];
    read_at(&f, &mut ic, ioff).map_err(|_| Error("Index nicht lesbar".into()))?;
    let mut ix = vec![0u8; ius as usize];
    match icodec {
        CODEC_STORED if ics == ius => ix.copy_from_slice(&ic),
        CODEC_LZRC => lzrc::decompress(&ic, &mut ix).ok_or_else(|| Error("Index beschädigt".into()))?,
        _ => return err("Index beschädigt"),
    }
    let mut r = Rd { d: &ix, p: 0 };
    let ne = r.u32()? as u64;
    if ne > ius {
        return err("Index beschädigt");
    }
    let mut entries = Vec::with_capacity(ne as usize);
    for _ in 0..ne {
        let mut e = Entry { typ: r.u8()?, path: r.str()?, ..Default::default() };
        e.mode = r.u32()?;
        e.mtime = r.u64()? as i64;
        e.size = r.u64()?;
        e.content = r.u32()?;
        if e.typ == E_LINK {
            e.link = r.str()?;
        }
        if e.typ > E_LINK || !safe_path(&e.path) {
            return err(format!("Unsicherer Pfad im Archiv: {}", String::from_utf8_lossy(&e.path)));
        }
        entries.push(e);
    }
    let nc = r.u32()? as u64;
    if nc > ius {
        return err("Index beschädigt");
    }
    let mut contents = Vec::with_capacity(nc as usize);
    for _ in 0..nc {
        let mut c = Content { size: r.u64()?, crc: r.u32()?, ..Default::default() };
        let b = r.bytes(13)?;
        c.f = FSpec {
            id: b[0],
            stride: b[1],
            nfields: b[2],
            fields: b[3..9].try_into().unwrap(),
            skip: u32::from_le_bytes(b[9..13].try_into().unwrap()),
        };
        c.xform = r.u8()?;
        c.ssize = r.u64()?;
        if !c.f.is_valid()
            || c.xform > XF_JPG
            || (c.xform == XF_NONE && c.ssize != c.size)
            || (c.xform != XF_NONE && c.ssize > MAX_XFORM_SSIZE)
        {
            return err("Index beschädigt");
        }
        contents.push(c);
    }
    let nb = r.u32()? as u64;
    if nb > ius {
        return err("Index beschädigt");
    }
    let mut blocks = Vec::with_capacity(nb as usize);
    for _ in 0..nb {
        let mut b = Block { off: r.u64()?, csize: r.u64()?, usize: r.u64()?, codec: r.u8()?, ..Default::default() };
        let np = r.u32()? as u64;
        if np > ius || b.off > size || b.csize > size - b.off || b.usize > (1 << 31) {
            return err("Index beschädigt");
        }
        let mut sum = 0u64;
        for _ in 0..np {
            let p = Piece { content: r.u32()?, off: r.u64()?, len: r.u32()? };
            let c = contents.get(p.content as usize).ok_or_else(|| Error("Index beschädigt".into()))?;
            if p.off > c.ssize || p.len as u64 > c.ssize - p.off || (c.xform != XF_NONE && (p.off != 0 || p.len as u64 != c.ssize)) {
                return err("Index beschädigt");
            }
            sum += p.len as u64;
            b.pieces.push(p);
        }
        if sum != b.usize {
            return err("Index beschädigt");
        }
        blocks.push(b);
    }
    for e in &entries {
        if e.typ == E_FILE && e.size > 0 && contents.get(e.content as usize).is_none_or(|c| c.size != e.size) {
            return err("Index beschädigt");
        }
    }
    Ok(Archive { path: path.to_path_buf(), entries, contents, blocks })
}

fn crc32_combine(a: u32, b: u32, len_b: u64) -> u32 {
    let mut ha = crc32fast::Hasher::new_with_initial(a);
    ha.combine(&crc32fast::Hasher::new_with_initial_len(b, len_b));
    ha.finalize()
}

/// Entpackt `archive` nach `outdir` (parallel, CRC-geprüft).
pub fn extract(archive: &Path, outdir: &Path, threads: usize) -> Result<()> {
    let t0 = Instant::now();
    let a = load_archive(archive)?;
    fs::create_dir_all(outdir).map_err(|e| io_err("Kann Ordner nicht anlegen", outdir, e))?;
    let mut targets: Vec<Vec<usize>> = vec![Vec::new(); a.contents.len()];
    for (i, e) in a.entries.iter().enumerate() {
        let f = path_from_bytes(outdir, &e.path)?;
        if let Some(parent) = f.parent() {
            fs::create_dir_all(parent).map_err(|er| io_err("Kann Ordner nicht anlegen", parent, er))?;
        }
        match e.typ {
            E_DIR => fs::create_dir_all(&f).map_err(|er| io_err("Kann Ordner nicht anlegen", &f, er))?,
            E_LINK => {}
            _ => {
                let file = File::create(&f).map_err(|er| io_err("Kann Datei nicht anlegen", &f, er))?;
                if e.size > 0 {
                    file.set_len(e.size).map_err(|er| io_err("Kann Dateigröße nicht setzen", &f, er))?;
                    targets[e.content as usize].push(i);
                }
            }
        }
    }
    let failed = AtomicBool::new(false);
    let piece_crc: Vec<Vec<u32>> = parallel_map(a.blocks.len(), threads, |bi| {
        let b = &a.blocks[bi];
        let mut crcs = vec![0u32; b.pieces.len()];
        let fail = |msg: String| {
            eprintln!("gpk: {msg}");
            failed.store(true, Ordering::Relaxed);
        };
        let mut src = vec![0u8; b.csize as usize];
        let ok = File::open(&a.path).and_then(|f| read_at(&f, &mut src, b.off)).is_ok();
        if !ok {
            fail(format!("Block {bi} nicht lesbar"));
            return crcs;
        }
        let mut buf = vec![0u8; b.usize as usize];
        let ok = match b.codec {
            CODEC_STORED => {
                if b.csize == b.usize {
                    buf.copy_from_slice(&src);
                    true
                } else {
                    false
                }
            }
            CODEC_LZRC => lzrc::decompress(&src, &mut buf).is_some(),
            CODEC_TCM => {
                tcm::decompress(&src, &mut buf);
                true
            }
            CODEC_XZ => xz_decompress(&src, &mut buf),
            _ => false,
        };
        drop(src);
        if !ok {
            fail(format!("Block {bi} beschädigt"));
            return crcs;
        }
        let mut pos = 0usize;
        for (k, p) in b.pieces.iter().enumerate() {
            let c = &a.contents[p.content as usize];
            let mut tmp = vec![0u8; p.len as usize];
            filters::decode(&c.f, &mut buf[pos..pos + p.len as usize], &mut tmp, p.off);
            pos += p.len as usize;
            let (data, woff) = if c.xform != XF_NONE {
                let mut orig = vec![0u8; c.size as usize];
                let okx = if c.xform == XF_PNG { pngx::inverse(&tmp, &mut orig) } else { jpgx::inverse(&tmp, &mut orig) };
                if !okx {
                    fail(format!("Umwandlung in Block {bi} beschädigt"));
                    continue;
                }
                (orig, 0)
            } else {
                (tmp, p.off)
            };
            crcs[k] = crc32fast::hash(&data);
            for &t in &targets[p.content as usize] {
                let res = path_from_bytes(outdir, &a.entries[t].path)
                    .map_err(|e| io::Error::other(e.0))
                    .and_then(|f| fs::OpenOptions::new().write(true).open(f))
                    .and_then(|f| write_at(&f, &data, woff));
                if let Err(e) = res {
                    fail(format!("Schreibfehler: {e}"));
                }
            }
        }
        crcs
    });

    // CRC je Datei aus den Stück-CRCs kombinieren und prüfen
    let mut crc = vec![0u32; a.contents.len()];
    for (b, pc) in a.blocks.iter().zip(&piece_crc) {
        for (p, &pcrc) in b.pieces.iter().zip(pc) {
            let c = &a.contents[p.content as usize];
            let len = if c.xform != XF_NONE { c.size } else { p.len as u64 };
            crc[p.content as usize] = crc32_combine(crc[p.content as usize], pcrc, len);
        }
    }
    let bad = a.contents.iter().zip(&crc).filter(|(c, &k)| c.crc != k).count();
    // Symlinks, Rechte, Zeitstempel
    for e in &a.entries {
        let f = path_from_bytes(outdir, &e.path)?;
        if e.typ == E_LINK {
            if e.link.first() == Some(&b'/') {
                eprintln!("gpk: absoluter Symlink übersprungen: {}", String::from_utf8_lossy(&e.path));
                continue;
            }
            let _ = fs::remove_file(&f);
            if make_symlink(&e.link, &f).is_err() {
                eprintln!("gpk: Symlink nicht angelegt (auf diesem System nicht unterstützt?): {}", String::from_utf8_lossy(&e.path));
            }
            continue;
        }
        set_mode(&f, e.mode);
        if e.typ == E_FILE {
            let _ = filetime::set_file_mtime(&f, filetime::FileTime::from_unix_time(e.mtime, 0));
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    let total: u64 = a.entries.iter().filter(|e| e.typ == E_FILE).map(|e| e.size).sum();
    if bad > 0 || failed.load(Ordering::Relaxed) {
        return err(format!("FEHLER – {bad} Dateien mit falscher Prüfsumme"));
    }
    eprintln!(
        "Entpackt: {} Einträge, {:.1} MB in {:.2} s ({:.1} MB/s), alle CRCs OK",
        a.entries.len(),
        total as f64 / 1e6,
        dt,
        total as f64 / dt / 1e6
    );
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &[u8], link: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), link)
}
#[cfg(not(unix))]
fn make_symlink(_: &[u8], _: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}
#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) {}

/// Inhalt auflisten (Ausgabe wie die C-Version).
pub fn list(archive: &Path) -> Result<()> {
    let a = load_archive(archive)?;
    let mut so = io::stdout().lock();
    for e in &a.entries {
        let flt = if e.typ == E_FILE && e.size > 0 { FILTER_NAMES[a.contents[e.content as usize].f.id as usize] } else { "" };
        let tc = b"fdl"[e.typ as usize] as char;
        let link = if e.typ == E_LINK { format!(" -> {}", String::from_utf8_lossy(&e.link)) } else { String::new() };
        let _ = writeln!(so, "{} {:04o} {:12}  {:<14} {}{}", tc, e.mode, e.size, flt, String::from_utf8_lossy(&e.path), link);
    }
    let csum: u64 = a.blocks.iter().map(|b| b.csize).sum();
    let _ = writeln!(
        so,
        "{} Einträge, {} eindeutige Inhalte, {} Blöcke, Blockdaten {:.2} MB",
        a.entries.len(),
        a.contents.len(),
        a.blocks.len(),
        csum as f64 / 1e6
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sichere_pfade() {
        assert!(safe_path(b"a/b/c.txt"));
        for bad in [&b""[..], b"/etc/passwd", b"a/../b", b"..", b"a//b", b"./a", b"a/.", b"a\0b"] {
            assert!(!safe_path(bad), "{:?}", bad);
        }
    }
    #[test]
    fn crc_kombination() {
        let (a, b) = (b"Hallo ".as_slice(), b"Welt".as_slice());
        let whole = crc32fast::hash(b"Hallo Welt");
        assert_eq!(crc32_combine(crc32fast::hash(a), crc32fast::hash(b), b.len() as u64), whole);
        assert_eq!(crc32_combine(0, crc32fast::hash(a), a.len() as u64), crc32fast::hash(a));
    }
}
