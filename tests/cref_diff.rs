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
        let off = if r.next().is_multiple_of(3) { 0 } else { r.next() as u64 * 13 };
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

fn mixed_data(r: &mut Rng, n: usize) -> Vec<u8> {
    let words: [&[u8]; 6] =
        [b"Drache ", b"Schwert ", b"{\"id\": 42}, ", b"\x00\x00\x80\x3f", b"\xE8\x10\x20\x00\x00", b"func() {\n    return 1;\n}\n"];
    let mut v = Vec::with_capacity(n);
    let noise = r.next() % 3;
    while v.len() < n {
        if r.next() % 4 < noise {
            v.push(r.next() as u8);
        } else {
            v.extend_from_slice(words[(r.next() % 6) as usize]);
        }
    }
    v.truncate(n);
    v
}

#[test]
fn glz_identisch_zu_c() {
    let mut r = Rng::new(7);
    for case in 0..40 {
        let n = [0usize, 3, 100, 5000, 70_000, 300_000, 1_200_000][case % 7];
        let d = if case % 5 == 4 { test_data(&mut r, n) } else { mixed_data(&mut r, n) };
        let level = 1 + (case % 9) as i32;
        let nf = case % 3 != 0;
        let rust = gpk::glz::compress(&d, level, nf);
        let cap = unsafe { glz_bound(n) };
        let mut c = vec![0u8; cap];
        let cn = unsafe { glz_compress(d.as_ptr(), n, c.as_mut_ptr(), cap, level, if nf { 1 } else { 0 }) };
        c.truncate(cn);
        assert_eq!(rust, c, "GLZ weicht ab: Fall {case}, n={n}, Stufe {level}, ohne Filter {nf}");
        // Kreuz-Dekodierung
        assert_eq!(gpk::glz::decompress(&c, usize::MAX).unwrap(), d);
        let mut back = vec![0u8; n + 1];
        let bn = unsafe { glz_decompress(rust.as_ptr(), rust.len(), back.as_mut_ptr(), n + 1) };
        assert_eq!(bn, n);
        assert_eq!(&back[..n], &d[..]);
    }
}

#[test]
fn lzrc_identisch_zu_c() {
    let mut r = Rng::new(11);
    for case in 0..24 {
        let n = [0usize, 1, 5, 300, 9000, 100_000, 700_000, 2_000_000][case % 8];
        let d = if case % 4 == 3 { test_data(&mut r, n) } else { mixed_data(&mut r, n) };
        let level = [2, 5, 7, 9][case % 4];
        let mut p = gpk::lzrc::Params::level(level);
        if case % 3 == 1 {
            p.lc = 4;
            p.pb = 0;
        }
        if case % 5 == 2 {
            p.lc = 0;
            p.lp = 2;
            p.pb = 2;
        }
        let rust = gpk::lzrc::compress(&d, &p);
        let cp = CLzrcParams { lc: p.lc as i32, lp: p.lp as i32, pb: p.pb as i32, depth: p.depth as i32, nice: p.nice as i32 };
        let cap = unsafe { lzrc_bound(n) };
        let mut c = vec![0u8; cap];
        let cn = unsafe { lzrc_compress(d.as_ptr(), n, c.as_mut_ptr(), cap, &cp) };
        c.truncate(cn);
        assert_eq!(rust.len(), c.len(), "LZRC-Größe weicht ab: Fall {case}, n={n}, {p:?}");
        assert!(rust == c, "LZRC-Bytes weichen ab: Fall {case}, n={n}, {p:?}");
        let mut back = vec![0u8; n];
        gpk::lzrc::decompress(&c, &mut back).expect("Rust dekodiert C-Daten");
        assert_eq!(back, d);
        let mut back2 = vec![0u8; n];
        assert_eq!(unsafe { lzrc_decompress(rust.as_ptr(), rust.len(), back2.as_mut_ptr(), n) }, 1);
        assert_eq!(back2, d);
    }
}

#[test]
fn tcm_identisch_zu_c() {
    let mut r = Rng::new(13);
    let text = std::fs::read_to_string("legacy-c/gpk/gpk.c").unwrap();
    for case in 0..8 {
        let d: Vec<u8> = match case {
            0 => Vec::new(),
            1 => b"x".to_vec(),
            2 => text.as_bytes().to_vec(),
            3 => r.bytes(20_000),
            4 => test_data(&mut r, 50_000),
            _ => mixed_data(&mut r, 120_000 * case),
        };
        let n = d.len();
        let rust = gpk::tcm::compress(&d);
        let cap = unsafe { tcm_bound(n) };
        let mut c = vec![0u8; cap];
        let cn = unsafe { tcm_compress(d.as_ptr(), n, c.as_mut_ptr(), cap) };
        c.truncate(cn);
        assert!(rust == c, "TCM weicht ab: Fall {case}, n={n} ({} vs {} Bytes)", rust.len(), c.len());
        let mut back = vec![0u8; n];
        gpk::tcm::decompress(&c, &mut back);
        assert_eq!(back, d);
    }
}

#[test]
fn pxc_identisch_zu_c() {
    let mut r = Rng::new(17);
    for it in 0..40 {
        let (w, h, ch) = (1 + r.next() % 120, 1 + r.next() % 80, 1 + (it % 4) as usize);
        let n = (w * h) as usize * ch;
        let px: Vec<u8> = (0..n)
            .map(|i| match it % 4 {
                0 => r.next() as u8,
                1 => ((i / ch) as u32 % w * 3 + r.next() % 7) as u8,
                2 => {
                    if (i / 41) & 1 == 1 {
                        250
                    } else {
                        3
                    }
                }
                _ => ((i % 7) * 30 + (r.next() % 3) as usize) as u8,
            })
            .collect();
        let rust = gpk::pxc::encode(&px, w, h, ch).unwrap();
        let mut cn = 0usize;
        let c = unsafe { take(pxc_encode(px.as_ptr(), w, h, ch as i32, &mut cn), cn) }.unwrap();
        assert!(rust == c, "PXC weicht ab: {it} ({w}x{h}x{ch})");
        let mut out = vec![0u8; n];
        assert!(gpk::pxc::decode(&c, &mut out, w, h, ch));
        assert_eq!(out, px);
    }
}

/// Echte PNGs: aus GPK_PNG_DIR (CI erzeugt sie mit Pillow) und aus dem System (falls vorhanden)
fn real_pngs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    let extra = std::env::var("GPK_PNG_DIR").unwrap_or_default();
    for dir in [extra.as_str(), "/usr/share/icons/hicolor/48x48/apps", "/usr/share/icons/hicolor/256x256/apps", "/usr/share/pixmaps"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut f: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "png")).collect();
            f.sort();
            v.extend(f.into_iter().take(if dir == extra { usize::MAX } else { 15 }));
        }
    }
    v
}

#[test]
fn pngx_identisch_zu_c() {
    let files = real_pngs();
    let (mut tested, mut converted) = (0, 0);
    for f in &files {
        let png = std::fs::read(f).unwrap();
        for mode in [1u32, 2] {
            let rust = gpk::pngx::forward(&png, mode);
            let mut tn = 0usize;
            let c = unsafe { take(pngx_forward_mode(png.as_ptr(), png.len(), mode as i32, &mut tn), tn) };
            assert_eq!(rust.is_some(), c.is_some(), "Akzeptanz weicht ab: {} Modus {mode}", f.display());
            if let (Some(rv), Some(cv)) = (&rust, &c) {
                assert!(rv == cv, "PNGX weicht ab: {} Modus {mode}", f.display());
                let mut out = vec![0u8; png.len()];
                assert!(gpk::pngx::inverse(cv, &mut out));
                assert_eq!(out, png);
                converted += 1;
            }
            tested += 1;
        }
        assert_eq!(gpk::pngx::channels(&png), unsafe { pngx_channels(png.as_ptr(), png.len()) } as usize);
    }
    eprintln!("PNG: {tested} Prüfungen, {converted} umgewandelt");
    assert!(converted > 0 || files.is_empty());
}

#[test]
fn jpgx_identisch_zu_c() {
    // Eingebettete Beispiele + optional weitere Testbilder aus GPK_JPEG_DIR (CI erzeugt sie mit Pillow)
    let mut files: Vec<(String, Vec<u8>)> =
        gpk::testutil::jpeg_samples().into_iter().enumerate().map(|(i, d)| (format!("Beispiel {i}"), d)).collect();
    if let Ok(dir) = std::env::var("GPK_JPEG_DIR") {
        let mut paths: Vec<_> = std::fs::read_dir(&dir).expect("GPK_JPEG_DIR nicht lesbar").flatten().map(|e| e.path()).collect();
        paths.retain(|p| p.extension().is_some_and(|e| e == "jpg" || e == "jpeg"));
        paths.sort();
        files.extend(paths.into_iter().map(|p| (p.display().to_string(), std::fs::read(&p).unwrap())));
    }
    let (mut ok, mut rejected) = (0, 0);
    for (f, jpg) in &files {
        let rust = gpk::jpgx::forward(jpg);
        let mut tn = 0usize;
        let c = unsafe { take(jpgx_forward(jpg.as_ptr(), jpg.len(), &mut tn), tn) };
        assert_eq!(rust.is_some(), c.is_some(), "Akzeptanz weicht ab: {f}");
        match (&rust, &c) {
            (Some(rv), Some(cv)) => {
                assert!(rv == cv, "JPGX weicht ab: {f}");
                let mut out = vec![0u8; jpg.len()];
                assert!(gpk::jpgx::inverse(cv, &mut out) && &out == jpg);
                ok += 1;
            }
            _ => rejected += 1,
        }
    }
    eprintln!("JPEG: {ok} bytegleich umgewandelt, {rejected} von beiden abgelehnt");
}

/// Testdaten für Deflate: Text, Zufall, Nullen, Bildzeilen, weite Wiederholungen (> 32 KiB),
/// kurze Matches in großer Entfernung (TOO_FAR), Mischungen über mehrere Fensterverschiebungen.
#[allow(clippy::vec_init_then_push)]
fn deflate_corpus() -> Vec<(String, Vec<u8>)> {
    let mut r = Rng::new(77);
    let mut v: Vec<(String, Vec<u8>)> = Vec::new();
    v.push(("leer".into(), Vec::new()));
    v.push(("1 Byte".into(), b"x".to_vec()));
    v.push(("text".into(), test_data(&mut r, 150_000)));
    v.push(("zufall".into(), r.bytes(100_000)));
    v.push(("nullen".into(), vec![0u8; 300_000]));
    v.push(("gemischt".into(), mixed_data(&mut r, 400_000)));
    // Bildartig: Zeilen mit Filterbyte, Verläufen und Rauschen
    let mut img = Vec::new();
    for y in 0..300u32 {
        img.push((y % 5) as u8);
        for x in 0..800u32 {
            img.push(((x * 3 + y * 7) & 0xFF) as u8 ^ (r.next() % 4) as u8);
        }
    }
    v.push(("bild".into(), img));
    // Wiederholung eines Blocks in > 32 KiB Abstand und kurze Matches weit weg
    let block = r.bytes(40_000);
    let mut far = block.clone();
    far.extend(r.bytes(5_000));
    far.extend_from_slice(&block);
    for i in 0..20_000u32 {
        far.extend_from_slice(&[(i % 7) as u8, b'a' + (i % 3) as u8, b'z']);
        far.extend(r.bytes(2));
    }
    v.push(("weit".into(), far));
    // Viele kurze Wiederholungen mit Abständen um 4096 (TOO_FAR-Grenze)
    let mut tf = r.bytes(5000);
    for i in 0..60_000usize {
        let c = tf[i + 905];
        tf.push(if i % 3 == 0 { c } else { r.next() as u8 });
    }
    v.push(("toofar".into(), tf));
    v
}

#[test]
fn deflate_bitgenau_wie_zlib() {
    let corpus = deflate_corpus();
    let mut n = 0;
    for (name, d) in &corpus {
        // kleine/mittlere Daten: alle Parameter; große: Auswahl
        let mems: &[i32] = if d.len() > 200_000 { &[1, 8, 9] } else { &[1, 2, 5, 8, 9] };
        for &strat in &[0, 1] {
            for &mem in mems {
                for lvl in 1..=9 {
                    let z = gpk::zlib::deflate_with(d, lvl, mem, strat).unwrap();
                    let o = gpk::deflate::deflate_with(d, lvl, mem, strat).unwrap();
                    assert!(o == z, "{name}: Stufe {lvl}, memLevel {mem}, Strategie {strat}: {} vs {} B", o.len(), z.len());
                    assert!(gpk::deflate::matches(d, lvl, mem, strat, &z));
                    n += 1;
                }
            }
        }
    }
    eprintln!("Deflate: {n} Kombinationen bytegleich zu zlib");
}

#[test]
fn deflate_grosse_daten_wie_zlib() {
    // mehrere MB: viele Fensterverschiebungen und Blöcke
    let mut r = Rng::new(78);
    let d = mixed_data(&mut r, 3_000_000);
    for (lvl, mem, strat) in [(1, 8, 0), (3, 9, 0), (5, 8, 1), (6, 8, 0), (9, 9, 0), (9, 8, 1)] {
        let z = gpk::zlib::deflate_with(&d, lvl, mem, strat).unwrap();
        assert!(gpk::deflate::deflate_with(&d, lvl, mem, strat).unwrap() == z, "Stufe {lvl} memLevel {mem} Strategie {strat}");
    }
}

#[test]
fn matches_bricht_bei_abweichung_ab() {
    let mut r = Rng::new(79);
    let d = test_data(&mut r, 200_000);
    let z = gpk::zlib::deflate_with(&d, 6, 8, 0).unwrap();
    assert!(gpk::deflate::matches(&d, 6, 8, 0, &z));
    for (lvl, mem, strat) in [(5, 8, 0), (6, 9, 0), (6, 8, 1), (9, 8, 0)] {
        let other = gpk::zlib::deflate_with(&d, lvl, mem, strat).unwrap();
        assert_eq!(gpk::deflate::matches(&d, lvl, mem, strat, &z), other == z);
    }
}

#[test]
fn inflate_wie_zlib() {
    let mut r = Rng::new(80);
    let mut streams = Vec::new();
    for (_, d) in deflate_corpus() {
        for (lvl, mem, strat) in [(1, 8, 0), (6, 8, 0), (9, 9, 1)] {
            streams.push(gpk::zlib::deflate_with(&d, lvl, mem, strat).unwrap());
        }
    }
    let mut agree = 0;
    for z in &streams {
        assert_eq!(gpk::inflate::inflate_zlib(z, 1 << 24), gpk::zlib::inflate_all(z, 1 << 24));
        // Beschädigungen: gleiche Annahme/Ablehnung und gleiches Ergebnis wie zlib
        for _ in 0..300 {
            let mut m = z.clone();
            match r.next() % 4 {
                0 => {
                    let i = r.next() as usize % m.len();
                    m[i] ^= 1 << (r.next() % 8);
                }
                1 => {
                    let i = r.next() as usize % m.len();
                    m[i] = r.next() as u8;
                }
                2 => m.truncate(r.next() as usize % m.len()),
                _ => {
                    let i = r.next() as usize % m.len();
                    m.insert(i, r.next() as u8);
                }
            }
            let mine = gpk::inflate::inflate_zlib(&m, 1 << 24);
            let theirs = gpk::zlib::inflate_all(&m, 1 << 24);
            assert!(
                mine == theirs,
                "Abweichung bei beschädigtem Strom: eigen {:?} B, zlib {:?} B",
                mine.map(|v| v.len()),
                theirs.map(|v| v.len())
            );
            agree += 1;
        }
    }
    eprintln!("Inflate: {agree} beschädigte Ströme gleich behandelt wie zlib");
}
