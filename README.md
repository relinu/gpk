# Kompression für Computerspiele

Zwei Kompressoren in C, jeweils mit Benchmarks gegen gängige Verfahren
(LZ4, zlib, zstd, bzip2, xz):

| Projekt | Zweck | Doku |
|---|---|---|
| **GPK** (`gpk/`) | Packt einen kompletten Spiel-Build für den Download: Deduplizierung, dateitypspezifische Filter (x86-Code, DDS-Texturen, Meshes, Audio), verlustfreie PNG- und JPEG-Rekompression, eigene Kerne LZRC (LZ) und TCM (Context Mixing für Text), paralleles Entpacken mit CRC-Prüfung | [gpk/README.md](gpk/README.md) |
| **GLZ** (Wurzelordner) | Schneller Asset-Kompressor fürs Laden zur Laufzeit (unabhängige 1-MiB-Blöcke, Huffman) | [GLZ.md](GLZ.md) |

## Kernergebnis (Test-Build 327,6 MB inkl. JPEGs)

| Verfahren | Größe nachher | Differenz | Packen | Entpacken (4 Threads) |
|---|---:|---:|---:|---:|
| zip -9 | 187,2 MB | −42,9 % | 40,0 s | 3,1 s |
| tar + zstd -19 --long=27 | 162,8 MB | −50,3 % | 191,8 s | 0,9 s |
| tar + xz -9e | 149,2 MB | −54,5 % | 311,1 s | 7,3 s |
| GPK v3 `--no-cm` | 126,3 MB | −61,5 % | 100,6 s | 4,1 s |
| **GPK v3** | **125,5 MB** | **−61,7 %** | 118,5 s | 9,9 s |

Details, Ablationen und Abwägungen: [gpk/README.md](gpk/README.md).
Der Test-Build ist teilweise synthetisch (Code, Skripte, Schriften sind echte Dateien).

## Schnellstart

```bash
cd gpk && make && make test
python3 make_build.py testbuild/MyGame            # Test-Build erzeugen (~330 MB, JPEGs brauchen Pillow)
./gpk c -t 4 testbuild/MyGame spiel.gpk
./gpk x -t 4 spiel.gpk entpackt
./bench_build.sh testbuild/MyGame arbeit && python3 compare_table.py arbeit/results.csv testbuild/MyGame
```

Benötigt: gcc, zlib, liblzma, bzip2, Python 3 mit NumPy; für den Vergleich
zusätzlich `xz`, `zip`/`unzip` und die zstd-/LZ4-Laufzeitbibliotheken.

## Windows

`gpk.exe` wird unter Linux per Cross-Compiling gebaut (MinGW-w64, statisch gelinkt,
nur Windows-System-DLLs nötig):

```bash
sudo apt-get install gcc-mingw-w64-x86-64 libz-mingw-w64-dev
make -C gpk windows        # erzeugt gpk/gpk.exe
```

Archive sind zwischen Linux und Windows austauschbar. Unterschiede der `.exe`:
Option `--xz` fehlt (ohne liblzma gebaut; Standard-Kern LZRC ist enthalten),
Symlinks werden beim Entpacken übersprungen, Unix-Rechte nicht gesetzt.
Dateinamen sollten ASCII sein (die `.exe` nutzt die ANSI-Dateisystem-API).

## CI

`.github/workflows/build.yml` läuft bei jedem Push auf `main` und bei Pull Requests,
ausschließlich auf Ubuntu-Runnern (keine Windows-VM):

- **Linux (gcc und clang):** Bauen, Filter- und GLZ-Fuzztests mit ASan/UBSan,
  GPK-Rauchtest (packen, entpacken, bitgenau vergleichen, beschädigtes Archiv muss abgelehnt werden)
- **Windows-.exe:** Cross-Build mit MinGW-w64, Tests der `.exe` unter Wine,
  Archiv-Austausch Linux → Windows und Windows → Linux
- Die fertigen Programme (`gpk`, `gpk.exe`) stehen als Artefakte am jeweiligen Lauf.

## Releases

Fertige Programme erscheinen automatisch unter *Releases*, sobald alle Builds und Tests grün sind:

```bash
git tag v0.1.0 && git push origin v0.1.0     # legt den Release an und hängt die Dateien an
```

Alternativ einen Release in der GitHub-Oberfläche veröffentlichen – die Pipeline hängt die
Dateien dann an diesen Release an. Angehängt werden `gpk-<version>-linux-x86_64.tar.gz`,
`gpk-<version>-windows-x86_64.zip` und `SHA256SUMS.txt`.
