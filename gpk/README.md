# GPK – Download-Packer für Spiel-Builds

GPK nimmt einen fertigen Spiel-Build (Ordner) und erzeugt ein Archiv für den
Download. Ziel: möglichst kleine Downloads, bitgenaue Wiederherstellung
(CRC32 je Datei), paralleles Entpacken.

```bash
make                                   # baut gpk und zstdtool
./gpk c -t 4 <build-ordner> spiel.gpk  # packen
./gpk x -t 4 spiel.gpk <ziel>          # entpacken (prüft alle CRCs)
./gpk l spiel.gpk                      # Inhalt und gewählte Filter anzeigen
make test                              # Filter-Roundtrip-Fuzztest (ASan/UBSan)
```

## Pipeline

1. **Scannen**: Dateien, Ordner, Symlinks, Rechte (z. B. Ausführbar-Bit), Zeitstempel
2. **Deduplizieren**: identische Dateien nur einmal (CRC32 + Größe + Bytevergleich)
3. **Umwandeln**: PNG-Rekompression – IDAT-Deflate-Strom entpacken, wenn zlib ihn
   bitgenau reproduzieren kann (Precomp/Preflate-Prinzip, vor dem Speichern verifiziert)
4. **Klassifizieren & Filter wählen** je Datei, per Probekompression mit dem LZRC-Kern:
   | Filter | für | Wirkung |
   |---|---|---|
   | x86-BCJ | EXE/DLL/SO | relative CALL/JMP-Ziele → absolut (gleiche Ziele = gleiche Bytes) |
   | Feld-Split | DDS BC1–BC5 | Endpunkte und Indizes der 4×4-Blöcke getrennt |
   | Wort-Vorhersage | Vertex-Puffer, PCM, Heightmaps | 16/32-Bit-Werte aus dem vorigen Datensatz vorhersagen (Ordnung 1/2), Rest in Byte-Planes |
   | Delta / Shuffle | sonstige Strukturdaten | Byte-Delta bzw. Byte-Planes mit erkanntem Stride (1–64 Byte) |
   | – | Ogg, Video, nicht reproduzierbare PNG | als „bereits komprimiert“ erkannt und nur gespeichert |
5. **Sortieren** nach Klasse, Endung und Name (ähnliche Dateien landen im selben Fenster)
6. **Komprimieren** in unabhängigen 64-MiB-Blöcken, parallel, mit **LZRC**
   (LZ77 + Binärbaum-Match-Finder + optimales Parsing + adaptiver Range Coder,
   LZMA-Prinzip, eigene Implementierung) – wahlweise `--xz` als Kern

## Ergebnisse (Test-Build 320,6 MB, 954 Einträge)

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
- Packen ist bewusst langsam (Probekompressionen); Entpacken ist parallel und
  schneller als typische Downloadraten.
