#!/usr/bin/env python3
"""GPK-Rauchtest für Linux und Windows.

  smoke_test.py <gpk-programm> [--wine]                      packen, entpacken, bitgenau vergleichen
  smoke_test.py <gpk-programm> [--wine] --cross-pack A.gpk    deterministischen Baum packen
  smoke_test.py <gpk-programm> [--wine] --cross-extract A.gpk fremdes Archiv entpacken und vergleichen
  smoke_test.py --make-tree DIR                               nur den Test-Build anlegen (z. B. für Bytegleichheit C/Rust)

--wine: gpk.exe unter Linux per Wine ausführen (Windows-Verhalten: keine Symlinks, keine Unix-Rechte).

Abgedeckt: x86-Code (BCJ), Text, PNG-Rekompression, Float-Strukturdaten, Duplikate,
leere Dateien und Ordner, Leerzeichen im Namen; unter Linux zusätzlich Symlinks und Rechte.
Außerdem muss ein beschädigtes Archiv mit Fehler abgelehnt werden.
"""
import argparse
import math
import os
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import zlib

WIN = os.name == "nt"          # wird bei --wine auf True gesetzt
RUNNER = []                     # z. B. ["wine"]
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)


def make_tree(root, with_system_files):
    """Deterministischer Test-Build; optional mit echten Systembibliotheken."""
    w = h = 256
    raw = b"".join(b"\x00" + bytes(((x ^ y) & 0xFF) if c < 3 else 255 for x in range(w) for c in range(4)) for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))
    write(os.path.join(root, "data", "ui", "atlas.png"), png)
    verts = b"".join(struct.pack("<8f", i * 0.5, math.sin(i * 0.01) * 10, i * 0.25, 0, 1, 0, (i % 64) / 64, (i // 64) / 64)
                     for i in range(40000))
    write(os.path.join(root, "data", "mesh.bin"), verts)
    # JPEG aus den eingebetteten Testdaten (kein Pillow nötig): größtes Beispiel
    hdr = open(os.path.join(REPO, "legacy-c", "gpk", "tests", "sample_jpegs.h")).read()
    jpgs = [bytes(int(v) for v in m.group(1).split(",")) for m in re.finditer(r"SAMPLE_JPEG\d\[\d+\] = \{([0-9,]+)\}", hdr)]
    write(os.path.join(root, "data", "ui", "loading.jpg"), max(jpgs, key=len))
    write(os.path.join(root, "data", "leer.txt"), b"")
    os.makedirs(os.path.join(root, "leerer ordner"), exist_ok=True)
    for name in sorted(os.listdir(os.path.join(REPO, "legacy-c", "gpk"))):
        if name.endswith((".c", ".h")):
            with open(os.path.join(REPO, "legacy-c", "gpk", name), "rb") as f:
                src = f.read().replace(b"\r\n", b"\n")      # gleich auf allen Systemen (git autocrlf)
            write(os.path.join(root, "data", "src", name), src)
    write(os.path.join(root, "dlc", "mesh kopie.bin"), verts)   # Duplikat mit Leerzeichen im Namen
    if not with_system_files:
        return
    bindir = os.path.join(root, "bin")
    os.makedirs(bindir, exist_ok=True)
    if WIN and os.name == "nt":
        sysdir = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "System32")
        for dll in ["kernel32.dll", "user32.dll", "msvcrt.dll"]:
            shutil.copyfile(os.path.join(sysdir, dll), os.path.join(bindir, dll))
    else:
        libdir = "/usr/lib/x86_64-linux-gnu"
        for lib in ["libz.so.1", "liblzma.so.5", "libbz2.so.1.0"]:
            real = os.path.realpath(os.path.join(libdir, lib))
            shutil.copyfile(real, os.path.join(bindir, os.path.basename(real)))
            if not WIN:                     # Rechte und Symlinks nur bei nativem Linux-Ziel
                os.chmod(os.path.join(bindir, os.path.basename(real)), 0o755)
                os.symlink(os.path.basename(real), os.path.join(bindir, lib))


def compare_trees(a, b):
    errors = []
    for base, dirs, files in os.walk(a):
        rel = os.path.relpath(base, a)
        other = os.path.join(b, rel)
        if not os.path.isdir(other):
            errors.append(f"Ordner fehlt: {rel}")
            continue
        for name in dirs + files:
            pa, pb = os.path.join(base, name), os.path.join(other, name)
            if os.path.islink(pa):
                if not os.path.islink(pb) or os.readlink(pa) != os.readlink(pb):
                    errors.append(f"Symlink falsch: {os.path.join(rel, name)}")
            elif os.path.isfile(pa):
                if not os.path.isfile(pb):
                    errors.append(f"Datei fehlt: {os.path.join(rel, name)}")
                    continue
                with open(pa, "rb") as fa, open(pb, "rb") as fb:
                    if fa.read() != fb.read():
                        errors.append(f"Inhalt verschieden: {os.path.join(rel, name)}")
                if not WIN and stat.S_IMODE(os.stat(pa).st_mode) != stat.S_IMODE(os.stat(pb).st_mode):
                    errors.append(f"Rechte verschieden: {os.path.join(rel, name)}")
    for base, dirs, files in os.walk(b):
        rel = os.path.relpath(base, b)
        for name in dirs + files:
            if not os.path.lexists(os.path.join(a, rel, name)):
                errors.append(f"Zusätzlich: {os.path.join(rel, name)}")
    return errors


def run(gpk, *args, check=True):
    r = subprocess.run([*RUNNER, gpk, *args], capture_output=True, text=True, encoding="utf-8", errors="replace")
    if check and r.returncode != 0:
        sys.exit(f"FEHLER: gpk {' '.join(args)} -> Exit {r.returncode}\n{r.stdout}{r.stderr}")
    return r


def fail(msg):
    sys.exit(f"FEHLER: {msg}")


def main():
    global WIN, RUNNER
    ap = argparse.ArgumentParser()
    ap.add_argument("gpk", nargs="?")
    ap.add_argument("--make-tree")
    ap.add_argument("--wine", action="store_true")
    ap.add_argument("--cross-pack")
    ap.add_argument("--cross-extract")
    a = ap.parse_args()
    if a.make_tree:
        make_tree(os.path.abspath(a.make_tree), True)
        return
    if not a.gpk:
        ap.error("gpk-Programm fehlt")
    if a.wine:
        WIN, RUNNER = True, ["wine"]
    gpk = os.path.abspath(a.gpk)
    with tempfile.TemporaryDirectory() as w:
        build = os.path.join(w, "build")
        if a.cross_pack:
            make_tree(build, False)
            log = run(gpk, "c", "-t", "2", build, os.path.abspath(a.cross_pack)).stderr
            if "(Text-CM)" not in log or "JPEG-Rekompression: 1" not in log or "PNG-Rekompression: 1" not in log:
                fail("Systemwechsel-Archiv deckt Text-Kern, JPEG und PNG nicht ab:\n" + log)
            print(f"Archiv für den Systemwechsel-Test erzeugt: {a.cross_pack}")
            return
        if a.cross_extract:
            make_tree(build, False)
            out = os.path.join(w, "out")
            run(gpk, "x", os.path.abspath(a.cross_extract), out)
            errs = compare_trees(build, out)
            if errs:
                fail("Fremdes Archiv nicht bitgenau entpackt:\n  " + "\n  ".join(errs))
            print("Systemwechsel-Test OK: fremdes Archiv bitgenau entpackt")
            return

        make_tree(build, True)
        arch, out = os.path.join(w, "test.gpk"), os.path.join(w, "out")
        log = run(gpk, "c", "-t", "2", build, arch).stderr
        print(log)
        run(gpk, "l", arch)
        print(run(gpk, "x", "-t", "2", arch, out).stderr)
        errs = compare_trees(build, out)
        if errs:
            fail("Entpacktes weicht ab:\n  " + "\n  ".join(errs))
        if "PNG-Rekompression: 1" not in log:
            fail("PNG-Rekompression war nicht aktiv")
        if "(Text-CM)" not in log:
            fail("Text-Kern (Context Mixing) wurde nicht benutzt")
        if "JPEG-Rekompression: 1" not in log:
            fail("JPEG-Rekompression war nicht aktiv")
        if "x86-bcj" not in log and "x86-split" not in log:
            fail("x86-Filter war nicht aktiv")

        data = bytearray(open(arch, "rb").read())
        data[len(data) // 2] ^= 0xFF
        bad = os.path.join(w, "kaputt.gpk")
        open(bad, "wb").write(data)
        if run(gpk, "x", bad, os.path.join(w, "out2"), check=False).returncode == 0:
            fail("Beschädigtes Archiv wurde nicht erkannt")
        print(f"Rauchtest OK: {os.path.getsize(arch)} Bytes Archiv")


if __name__ == "__main__":
    main()
