# GPK – Download-Packer für Spiel-Builds

GPK nimmt einen fertigen Spiel-Build (Ordner) und erzeugt ein Archiv für den
Download. Ziel: möglichst kleine Downloads, bitgenaue Wiederherstellung
(CRC32 je Datei), paralleles Entpacken.

```bash
cargo build --release                  # baut target/release/gpk
gpk c -t 4 <build-ordner> spiel.gpk    # packen
gpk x -t 4 spiel.gpk <ziel>            # entpacken (prüft alle CRCs)
gpk l spiel.gpk                        # Inhalt und gewählte Filter anzeigen
cargo test --release --features cref   # Tests + Differenztests gegen die C-Referenz
```

## Speicherbedarf

Der RAM-Bedarf hängt **nicht** von der Build-Größe ab:

- Fertige Blöcke werden sofort ins Archiv geschrieben, in fester Reihenfolge
  (Archive sind bei gleicher Eingabe bytegleich). Die Threads arbeiten höchstens
  `Threads + 2` Blöcke voraus.
- Dateien werden nur bereichsweise gelesen, jeweils für den aktuellen Arbeitsschritt.
- Umgewandelte PNG-/JPEG-Daten werden in `<archiv>.auslagerung.tmp` neben dem Archiv
  ausgelagert und am Ende gelöscht.
- Beim Entpacken liest jeder Thread nur den Block, den er gerade entpackt.

| Build | Packen (4 Threads) | Entpacken (4 Threads) |
|---:|---:|---:|
| 2,9 GB | 2,4 GB RAM (vorher 6,3 GB) | – |
| 5,6 GB | 2,7 GB RAM | 0,5 GB RAM (vorher 6,1 GB) |
| 15,4 GB | 2,3 GB RAM | 0,5 GB RAM |

Faustregel beim Packen: etwa 11 × Blockgröße je Thread (LZRC-Binärbaum + Puffer).
Mit `--max-ram GB` wählt GPK Threads und notfalls kleinere Blöcke passend zum Budget,
z. B. `--max-ram 1` → 1 Thread, gemessen 0,7 GB. Weniger Threads oder kleinere Blöcke
machen das Packen langsamer bzw. das Archiv etwas größer.

## Pipeline

1. **Scannen**: Dateien, Ordner, Symlinks, Rechte (z. B. Ausführbar-Bit), Zeitstempel
2. **Deduplizieren**: identische Dateien nur einmal (CRC32 + Größe + Bytevergleich)
3. **Umwandeln** (immer vor der Übernahme vollständig rekonstruiert und verglichen):
   - **PNG**: IDAT-Deflate-Strom entpacken, wenn zlib ihn bitgenau reproduzieren kann
     (Precomp/Preflate-Prinzip). Danach je Bild das Kleinere von zwei Varianten:
     Rohdaten für LZRC oder **Pixel-Coder PXC** (Zeilenfilter entfernen, gewichtete
     Mischung von 6 Prädiktoren, optionale Farbtransformation, Kontext-Arithmetikkodierung)
   - **JPEG** (`src/jpgx.rs`): Baseline/Extended mit Huffman, 1–4 Komponenten, beliebige
     Abtastung, Restart-Marker. Scan zu DCT-Koeffizienten dekodieren, mit Kontextmodell
     (Nachbarblöcke) neu kodieren, beim Entpacken Huffman-Strom exakt neu erzeugen.
     Progressive/arithmetische JPEGs bleiben unverändert.
4. **Klassifizieren & Filter wählen** je Datei, per Probekompression mit dem LZRC-Kern:
   | Filter | für | Wirkung |
   |---|---|---|
   | x86-Split | EXE/DLL/SO | Operanden von CALL/JMP/Jcc in eigenen Strom, als absolute Ziele (BCJ2-Prinzip, längenerhaltend) |
   | x86-BCJ | EXE/DLL/SO | relative CALL/JMP-Ziele → absolut (gleiche Ziele = gleiche Bytes) |
   | Feld-Split | DDS BC1–BC5 | Endpunkte und Indizes der 4×4-Blöcke getrennt |
   | Wort-Vorhersage | Vertex-Puffer, PCM, Heightmaps | 16/32-Bit-Werte aus dem vorigen Datensatz vorhersagen (Ordnung 1/2), Rest in Byte-Planes |
   | Delta / Shuffle | sonstige Strukturdaten | Byte-Delta bzw. Byte-Planes mit erkanntem Stride (1–64 Byte) |
   | – | Ogg, Video, nicht reproduzierbare PNG | als „bereits komprimiert“ erkannt und nur gespeichert |
5. **Sortieren** nach Klasse, Endung und Name (ähnliche Dateien landen im selben Fenster)
6. **Komprimieren** in unabhängigen 64-MiB-Blöcken, parallel, mit **LZRC**
   (LZ77 + Binärbaum-Match-Finder + optimales Parsing + adaptiver Range Coder,
   LZMA-Prinzip, eigene Implementierung) – wahlweise `--xz` als Kern.
   **Textblöcke** (8 MiB, reiner Text) werden zusätzlich mit dem Context-Mixing-Kern
   **TCM** kodiert (Ordnungen 0–6, Wort-, Spalten- und Match-Modell, zwei Mischnetze,
   zwei APM-Stufen); das kleinere Ergebnis gewinnt. Abschaltbar mit `--no-cm`.

Alle Kerne rechnen **nur mit Ganzzahlen**: Linux und Windows erzeugen und lesen
dieselben Archive bitgleich (in der CI geprüft: Linux ↔ Windows-.exe unter Wine).

## Ergebnisse v3 (erweiterter Test-Build 327,6 MB, mit 36 JPEGs)

| Verfahren | Größe nachher | Differenz | Packen | Entpacken (4 Threads / 1 Thread) |
|---|---:|---:|---:|---:|
| zip -9 (Deflate je Datei) | 187,2 MB | −42,9 % | 40,0 s | 3,1 s |
| tar + zstd -19 --long=27 | 162,8 MB | −50,3 % | 191,8 s | 0,9 s |
| tar + xz -9e | 149,2 MB | −54,5 % | 311,1 s | 7,3 s |
| GPK v2 | 128,5 MB | −60,8 % | 105,4 s | 2,7 s / 7,5 s |
| GPK v3 `--no-cm` | 126,3 MB | −61,5 % | 100,6 s | 4,1 s / 9,3 s |
| **GPK v3** | **125,5 MB** | **−61,7 %** | 118,5 s | 9,9 s / 33,5 s |

Alle GPK-Läufe bitgenau geprüft. Rohdaten: `results_v3.csv`.

### Was v2 → v3 je Baustein gebracht hat

| Baustein | Gruppe | v2 | v3 | Vergleich |
|---|---|---:|---:|---|
| JPEG-Rekompression | 36 JPEGs, 6,9 MB | ≈6,9 MB | 5,2 MB (−25 %) | 86 Test-JPEGs: 70 bitgenau, 16 abgelehnt (progressiv u. ä.), 0 Fehler |
| Text-Kern TCM | Text 25,1 MB | 2,99 MB | 2,21 MB (−26 %) | PPMd 2,41 MB, ZPAQ -m5 1,96 MB; entpackt ~1 MB/s je Kern |
| Pixel-Coder PXC | PNG 10,8 MB | 6,06 MB | 5,73 MB (−5,4 %) | JPEG XL verlustfrei 5,27 MB |
| x86-Split | Code 66,6 MB | 13,10 MB | 13,03 MB (−0,5 %) | 7-Zip BCJ2 −5 % ggü. BCJ; selektive Variante gemessen nur −1,5 % mehr |

**Abwägung:** Der Text-Kern spart am meisten je Byte, macht das Entpacken aber
langsam (hier 9,9 statt 4,1 s mit 4 Threads). Für schnelle Installationen `--no-cm`
nutzen – dann bleiben JPEG-, PNG- und x86-Verbesserungen bei kaum langsamerem Entpacken.

## Ergebnisse v2 (Test-Build 320,6 MB, 954 Einträge, ohne JPEGs)

| Verfahren | Größe nachher | Differenz | Packen | Entpacken |
|---|---:|---:|---:|---:|
| zip -9 (Deflate je Datei) | 180,3 MB | −43,7 % | 30,6 s | 2,1 s |
| tar + zstd -19 --long=27 | 155,9 MB | −51,4 % | 141,6 s | 0,7 s |
| tar + xz -9e | 142,2 MB | −55,7 % | 236,3 s | 4,9 s |
| GPK v1 | 133,4 MB | −58,4 % | 57,3 s | 1,5 s |
| **GPK v2 (optimiert)** | **121,7 MB** | **−62,0 %** | 76,0 s | 2,0 s (1 Thread: 5,2 s) |

Vollständige Tabelle inkl. Ablationen: `vergleich.md`, Rohdaten: `results_build.csv`.

### Was die Optimierung v1 → v2 gebracht hat (gemessen je Asset-Gruppe)

| Gruppe | v1 | v2 | Ursache |
|---|---:|---:|---|
| Meshes | 25,6 MB | 18,6 MB | Stride-Erkennung bis 64 Byte (Vertex = 48 Byte), Wort-Vorhersage, Filterwahl mit LZRC statt GLZ-Schätzer |
| PNG (UI) | 10,8 MB | 6,1 MB | PNG-Rekompression |
| WAV | 8,5 MB | 8,3 MB | Wort-Vorhersage |
| Texturen, Code, Text | unverändert | | getestet: andere Split-Layouts und lc/lp/pb-Varianten bringen ≤ 0,5 % |

Die Gewinne kommen aus der **Vorverarbeitung**. Der LZRC-Kern liegt auf xz-Niveau
(mit `--xz` ist das Archiv 0,3 MB kleiner).

## Erkenntnisse aus der Recherche

- **Steam (SteamPipe)** teilt Dateien in ~1-MB-Chunks, komprimiert jeden mit LZMA
  und verschlüsselt ihn; Updates laden nur geänderte Chunks. Valve rät, Pack-Dateien
  *nicht* selbst zu komprimieren. Epic arbeitet laut Analysen ebenfalls mit ~1-MiB-Chunks.
  → GPKs solide 64-MiB-Blöcke sind ideal für den **Erst-Download**, aber schlecht für
  **Updates**: Eine kleine Änderung invalidiert einen ganzen Block.
- **Oodle Texture** (RAD/Epic): RDO-Encoding verkleinert BCn-Texturen nach Kompression um
  bis zu ~50 %, ist aber verlustbehaftet (Textur-Encoder, nicht Packer). Das verlustfreie
  **BC7Prep** bringt laut RAD 5–15 % bei BC7.
- **Precomp / Preflate**: Deflate-Ströme (PNG, ZIP, PDF) entpacken und bitgenau
  rekonstruieren; Preflate schafft das auch für Nicht-zlib-Encoder mit kleinen
  Korrekturdaten. GPK nutzt die einfache Variante (nur zlib-reproduzierbare Ströme,
  ~54 % der getesteten echten PNGs).

## Grenzen / nächste Schritte

- Test-Build ist teilweise synthetisch (Code, Skripte, Schriften echt).
- **Update-freundlich machen**: inhaltsdefiniertes Chunking (z. B. ~1 MiB) und
  Chunk-Dedup gegen die Vorversion, damit Patches klein bleiben.
- Preflate-artige Korrekturdaten für PNGs fremder Encoder; BC7-Transform (BC7Prep-Idee).
- Progressive JPEGs (heute unverändert gespeichert); Text-Kern schneller machen
  (z. B. PPMd-artiges Modell) oder nur für große Textanteile nutzen.
- Packen ist bewusst langsam (Probekompressionen); Entpacken ist parallel und
  schneller als typische Downloadraten.
