# Kompression für Computerspiele

**GPK** packt einen kompletten Spiel-Build für den Download: Deduplizierung,
dateitypspezifische Filter (x86-Code, DDS-Texturen, Meshes, Audio), verlustfreie PNG- und
JPEG-Rekompression, eigene Kerne LZRC (LZ) und TCM (Context Mixing für Text), paralleles
Entpacken mit CRC-Prüfung. Details: [docs/GPK.md](docs/GPK.md).

Das Programm ist in **Rust** geschrieben (`src/`). Die frühere C-Version liegt als Referenz
in `legacy-c/` (inkl. GLZ, dem Laufzeit-Asset-Kompressor, siehe [docs/GLZ.md](docs/GLZ.md)).
Rust- und C-Version verwenden dasselbe Archivformat und entpacken die Archive der jeweils
anderen; die Kerne (Filter, LZRC, TCM, PNG, JPEG) sind bytegleich, das prüft die CI bei jedem Push.

## Kernergebnis (Test-Build 327,6 MB inkl. JPEGs)

| Verfahren | Größe nachher | Differenz | Packen | Entpacken (4 Threads) |
|---|---:|---:|---:|---:|
| zip -9 | 187,2 MB | −42,9 % | 40,0 s | 3,1 s |
| tar + zstd -19 --long=27 | 162,8 MB | −50,3 % | 191,8 s | 0,9 s |
| tar + xz -9e | 149,2 MB | −54,5 % | 311,1 s | 7,3 s |
| GPK v3 `--no-cm` | 126,3 MB | −61,5 % | 100,6 s | 4,1 s |
| **GPK v3** | **125,5 MB** | **−61,7 %** | 118,5 s | 9,9 s |

Details, Ablationen und Abwägungen: [docs/GPK.md](docs/GPK.md) und [docs/vergleich.md](docs/vergleich.md).
Der Test-Build ist teilweise synthetisch (Code, Skripte, Schriften sind echte Dateien).

## Rust-Version: Port und Optimierung (Test-Build 308 MB, 4 Threads)

| | Rust-Port (Archive bytegleich zu C) | Rust optimiert |
|---|---:|---:|
| Archiv | 120,27 MB | 120,28 MB (+0,01 %) |
| Packen | 92,8 s, 1,46 GB RAM | **64,3 s**, 1,67 GB RAM |
| Entpacken | 8,3 s, 0,44 GB RAM | **6,8 s**, 0,61 GB RAM |

Beide Spalten direkt hintereinander gemessen. Der reine Port war so schnell wie die C-Version
(in einer früheren Messung Packen 104,7 s gegenüber 104,3 s).
Was optimiert wurde und was nicht geholfen hat: [docs/GPK.md](docs/GPK.md#geschwindigkeit-rust-version).

## Schnellstart

```bash
cargo build --release
python3 tools/make_build.py testbuild/MyGame        # Test-Build erzeugen (~330 MB, JPEGs brauchen Pillow)
target/release/gpk c -t 4 testbuild/MyGame spiel.gpk
target/release/gpk x -t 4 spiel.gpk entpackt
target/release/gpk l spiel.gpk
```

Benötigt: Rust (stable) und einen C-Compiler (für die statisch eingebundenen zlib und liblzma).
Optionen: `gpk` ohne Argumente zeigt die Hilfe; `--max-ram GB` begrenzt den Speicherbedarf.

## Sicherheit

- Der gesamte Code ist sicheres Rust; die einzige `unsafe`-Stelle ist die Anbindung an zlib
  (`src/zlib.rs`), die für die bitgenaue PNG-Rekompression exakt dieselbe zlib wie beim Packen braucht.
- Archive werden vor dem Entpacken vollständig geprüft: Pfade (keine absoluten Pfade, kein `..`,
  unter Windows kein `\` oder `:`), Größen und Grenzen aller Blöcke und Stücke. Beschädigte Daten
  führen zu einer Fehlermeldung, nie zu einem Absturz oder Schreiben außerhalb des Zielordners;
  jede Datei wird per CRC32 geprüft.
- Absolute Symlinks werden beim Entpacken übersprungen.

## Tests

```bash
cargo test --release                       # Unit-Tests (Roundtrips, beschädigte Daten)
cargo test --release --features cref       # zusätzlich Differenztests: Rust == C, Byte für Byte
python3 ci/smoke_test.py target/release/gpk
```

Die Differenztests übersetzen die C-Module aus `legacy-c/` mit und vergleichen jede Stufe
(Filter, GLZ, LZRC, TCM, Pixel-Coder, PNG, JPEG) mit der Rust-Version.
Mit `GPK_PNG_DIR`/`GPK_JPEG_DIR` lassen sich weitere Testbilder einbinden
(`tools/gen_testimages.py` erzeugt welche, braucht Pillow).

## Windows

`gpk.exe` wird unter Linux per Cross-Compiling gebaut (keine Windows-VM nötig):

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt-get install gcc-mingw-w64-x86-64
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release --target x86_64-pc-windows-gnu
```

Gegenüber der C-Version hat die Rust-`.exe` denselben Funktionsumfang wie unter Linux
(auch `--xz`) und unterstützt Unicode-Dateinamen. Archive sind zwischen Linux und Windows
austauschbar; unter Windows werden Symlinks übersprungen und Unix-Rechte nicht gesetzt.

## CI

`.github/workflows/build.yml` läuft bei jedem Push auf `main` und `rust-port` und bei Pull Requests,
ausschließlich auf Ubuntu-Runnern (keine Windows-VM):

- **Linux:** `cargo fmt`/`clippy`, Tests und Differenztests gegen die C-Referenz, Tests im
  Debug-Modus (Überlaufprüfungen), Rauchtest (packen, entpacken, bitgenau vergleichen,
  beschädigtes Archiv muss abgelehnt werden), gleiche Archive bei 1 und 4 Threads,
  Kreuz-Entpacken C ↔ Rust
- **Windows-.exe:** Cross-Build, Unit- und Rauchtests der `.exe` unter Wine,
  Archiv-Austausch Linux → Windows und Windows → Linux
- Die fertigen Programme (`gpk`, `gpk.exe`) stehen als Artefakte am jeweiligen Lauf.

## Releases

Fertige Programme erscheinen automatisch unter *Releases*, sobald alle Builds und Tests grün sind:

```bash
git tag v0.4.0 && git push origin v0.4.0     # legt den Release an und hängt die Dateien an
```

Alternativ einen Release in der GitHub-Oberfläche veröffentlichen – die Pipeline hängt die
Dateien dann an diesen Release an. Angehängt werden `gpk-<version>-linux-x86_64.tar.gz`,
`gpk-<version>-windows-x86_64.zip` und `SHA256SUMS.txt`.
