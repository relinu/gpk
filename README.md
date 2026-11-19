# Kompression für Computerspiele

Zwei Kompressoren in C, jeweils mit Benchmarks gegen gängige Verfahren
(LZ4, zlib, zstd, bzip2, xz):

| Projekt | Zweck | Doku |
|---|---|---|
| **GPK** (`gpk/`) | Packt einen kompletten Spiel-Build für den Download: Deduplizierung, dateitypspezifische Filter (x86-Code, DDS-Texturen, Meshes, Audio), PNG-Rekompression, eigener Kompressionskern LZRC, paralleles Entpacken mit CRC-Prüfung | [gpk/README.md](gpk/README.md) |
| **GLZ** (Wurzelordner) | Schneller Asset-Kompressor fürs Laden zur Laufzeit (unabhängige 1-MiB-Blöcke, Huffman) | [GLZ.md](GLZ.md) |

## Kernergebnis (Test-Build 320,6 MB)

| Verfahren | Größe nachher | Differenz | Packen | Entpacken |
|---|---:|---:|---:|---:|
| zip -9 | 180,3 MB | −43,7 % | 30,6 s | 2,1 s |
| tar + zstd -19 --long=27 | 155,9 MB | −51,4 % | 141,6 s | 0,7 s |
| tar + xz -9e | 142,2 MB | −55,7 % | 236,3 s | 4,9 s |
| **GPK v2** | **121,7 MB** | **−62,0 %** | 76,0 s | 2,0 s (1 Thread: 5,2 s) |

Vollständige Tabelle inkl. Ablationen: [gpk/vergleich.md](gpk/vergleich.md).
Der Test-Build ist teilweise synthetisch (Code, Skripte, Schriften sind echte Dateien).

## Schnellstart

```bash
cd gpk && make && make test
python3 make_build.py testbuild/MyGame            # Test-Build erzeugen (~320 MB)
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
