# GLZ – ein Kompressor für Spiel-Assets

GLZ („Game LZ“) ist ein in C geschriebener verlustfreier Kompressor, der auf die
typischen Anforderungen von Computerspielen zugeschnitten ist, verglichen mit
LZ4, zlib, zstd, bzip2 und xz.

## Designentscheidungen

| Anforderung im Spiel | Umsetzung in GLZ |
|---|---|
| Assets werden **einmal** beim Build gepackt, aber **bei jedem Laden** entpackt | Asymmetrisch: aufwändige Kompression (Filter-Probeläufe, Hash-Ketten bis Tiefe 1024, Lazy Matching), einfacher Decoder |
| Streaming, paralleles Laden, wahlfreier Zugriff in Paketdateien | Unabhängige 1-MiB-Blöcke mit eigener Größe im Header (überspringbar, parallel dekodierbar) |
| Viele Binärdaten: Texturen, Vertex-Puffer, Audio, Heightmaps | **Automatische Vorfilter je Block**: Stride-Erkennung (1–32 Byte per Delta-Entropie), dann Probekompression mit *none*, *delta*, *byte-shuffle*, *shuffle+delta*; der kleinste gewinnt |
| Strukturierte Daten mit festen Abständen | LZ77 mit **2 Repeat-Offsets** (billig kodierte Wiederholung des letzten Abstands) |
| Schnelle Dekodierung | Kanonisches **Huffman, max. 11 Bit** → eine 2048-Einträge-Lookup-Tabelle, 4 Symbole pro Nachfüllen des 64-Bit-Bitpuffers; 8-Byte-Wildcopy für Matches |
| Bereits komprimierte Dateien (Ogg, JPEG …) | Block wird roh gespeichert, wenn Kompression nichts bringt |
| Beschädigte Dateien / Mods | Decoder prüft alle Grenzen; Fuzz-Test mit ASan/UBSan ohne Befund |

### Datenformat (pro Block)

```
filter(u8) stride(u8) mode(u8) rawsize(u32) payloadsize(u32) payload
payload = nseq | HUF(Literale) | HUF(Tokens) | HUF(Offset-Codes) | Längen-Überläufe (LEB128) | Offset-Extrabits
Token   = (min(Literallänge,15) << 4) | min(Matchlänge-4, 15)
Offset-Code: 0 = rep0, 1 = rep1, 2+b = Offset (1<<b) + b Extrabits
```

Warum die Filter so viel bringen: In einem Vertex-Puffer (32-Byte-Vertices aus Floats)
oder 16-Bit-Audio ändern sich benachbarte Werte nur wenig, aber das ist für einen
byteweisen LZ-Matcher unsichtbar. *Byte-Shuffle* sortiert alle 1. Bytes, alle 2. Bytes …
zusammen (die hohen Bytes werden fast konstant), *Delta* macht aus langsam
veränderlichen Werten kleine Zahlen, die Huffman günstig kodiert.

## Testkorpus

Synthetisch, aber realitätsnah und deterministisch erzeugt (`gen_corpus.py`, ~19 MB):
RGBA-Albedo-Textur 1024², 16-Bit-Heightmap, Normal Map, Terrain-Mesh (Vertex- und
Index-Puffer, Kachel-Reihenfolge wie nach Cache-Optimierung), 16-Bit-Stereo-PCM,
Tilemap eines Levels, Dialog-/Lokalisierungs-JSON, Spielstand mit Entity-Structs und
ein bereits komprimierter (zufälliger) Blob.

## Ergebnisse

Gemessen auf 1 Kern (`-O2 -march=native`), Systembibliotheken zlib 1.3, zstd 1.5.5,
LZ4 1.9.4, liblzma 5.4.5, bzip2 1.0.8. Jeder Durchlauf wurde per Roundtrip verifiziert.

### Kompressionsrate je Datei (Original / komprimiert, höher = besser)

| Datei | GLZ -1 | GLZ -5 | GLZ -9 | GLZ -9 (ohne Filter) | LZ4 | LZ4HC -12 | zlib -6 | zlib -9 | zstd -3 | zstd -19 | bzip2 -9 | xz -6 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| audio_pcm16_stereo.raw | 1.53 | 1.54 | **1.55** | 1.09 | 1.00 | 1.00 | 1.11 | 1.11 | 1.10 | 1.10 | 1.15 | 1.22 |
| dialog_de.json | 5.46 | 6.47 | 6.84 | 6.84 | 3.48 | 5.61 | 6.22 | 6.57 | 6.03 | 8.02 | **11.49** | 8.33 |
| heightmap_u16.raw | 1.57 | 1.57 | **1.59** | 1.03 | 1.00 | 1.00 | 1.05 | 1.05 | 1.04 | 1.06 | 1.33 | 1.18 |
| level_tilemap_u8.bin | 22.01 | 29.92 | 30.09 | 30.09 | 6.99 | 21.62 | 24.86 | 29.28 | 17.82 | **41.39** | 25.50 | 37.53 |
| mesh_indices_u32.bin | 5.01 | 6.20 | **6.69** | 2.80 | 1.59 | 1.69 | 2.71 | 2.71 | 2.26 | 2.89 | 2.70 | 3.98 |
| mesh_vertices.bin | 2.56 | 2.56 | **2.57** | 1.69 | 1.22 | 1.32 | 1.55 | 1.54 | 1.62 | 1.94 | 1.57 | 2.21 |
| normalmap_rgb8.raw | 1.46 | 1.47 | 1.47 | 1.30 | 1.00 | 1.07 | 1.24 | 1.24 | 1.21 | 1.34 | **1.71** | 1.38 |
| precompressed_blob.bin | **1.00** | **1.00** | **1.00** | **1.00** | 1.00 | 1.00 | 1.00 | 1.00 | **1.00** | **1.00** | 1.00 | 1.00 |
| savegame_entities.bin | 1.86 | 1.94 | 1.95 | 1.95 | 1.43 | 1.74 | 1.82 | 1.85 | 1.83 | 1.88 | 2.01 | **2.17** |
| texture_rgba8.raw | 2.37 | 2.37 | 2.37 | 1.77 | 1.05 | 1.43 | 1.70 | 1.70 | 1.63 | 1.93 | **2.39** | 2.27 |

### Gesamtergebnis über alle Dateien

| Codec | Gesamtgröße (MB) | Gesamtrate | Kompression MB/s | Dekompression MB/s |
|---|---:|---:|---:|---:|
| *Original* | 19.15 | 1.00 | – | – |
| GLZ -1 | 8.43 | 2.27 | 22.0 | 584 |
| GLZ -5 | 8.25 | 2.32 | 8.5 | 535 |
| GLZ -9 | 8.19 | 2.34 | 3.0 | 565 |
| GLZ -9 (ohne Filter) | 11.22 | 1.71 | 4.6 | 576 |
| LZ4 | 15.17 | 1.26 | 914.4 | 4746 |
| LZ4HC -12 | 13.36 | 1.43 | 14.6 | 4244 |
| zlib -6 | 11.55 | 1.66 | 29.4 | 269 |
| zlib -9 | 11.53 | 1.66 | 13.3 | 272 |
| zstd -3 | 11.83 | 1.62 | 288.7 | 1122 |
| zstd -19 | 10.63 | 1.80 | 3.5 | 776 |
| bzip2 -9 | 10.00 | 1.92 | 15.9 | 22 |
| xz -6 | 9.43 | 2.03 | 2.7 | 42 |

### Von GLZ -9 gewählte Filter (je 1-MiB-Block)

| Datei | Filter |
|---|---|
| audio_pcm16_stereo.raw | none=0 delta=0 shuf=0 shufdelta=2 stored=0 |
| dialog_de.json | none=2 delta=0 shuf=0 shufdelta=0 stored=0 |
| heightmap_u16.raw | none=0 delta=0 shuf=0 shufdelta=2 stored=0 |
| level_tilemap_u8.bin | none=1 delta=0 shuf=0 shufdelta=0 stored=0 |
| mesh_indices_u32.bin | none=0 delta=3 shuf=0 shufdelta=0 stored=0 |
| mesh_vertices.bin | none=0 delta=0 shuf=0 shufdelta=4 stored=0 |
| normalmap_rgb8.raw | none=0 delta=0 shuf=0 shufdelta=1 stored=0 |
| precompressed_blob.bin | none=0 delta=0 shuf=0 shufdelta=0 stored=1 |
| savegame_entities.bin | none=1 delta=0 shuf=0 shufdelta=0 stored=0 |
| texture_rgba8.raw | none=0 delta=0 shuf=0 shufdelta=4 stored=0 |

## Einordnung

**Wo GLZ gewinnt:** Bei binären Spiel-Assets – Audio, Heightmaps, Meshes, Index-Puffer –
ist GLZ klar vorn, oft auch vor xz und bzip2. Insgesamt ist das Archiv **8,2 MB statt
10,6 MB (zstd -19) bzw. 9,4 MB (xz)**, also ~23 % kleiner als zstd -19. Die Ablation
„ohne Filter“ (11,2 MB) zeigt: Der Vorteil kommt fast vollständig von den
automatischen Vorfiltern, das LZ+Huffman-Backend allein liegt etwa auf zlib-Niveau.

**Wo GLZ verliert:**
- *Text/JSON und Tilemaps*: zstd -19, xz und bzip2 sind besser. Sie haben optimales
  Parsing (statt Lazy Matching), größere Fenster (GLZ: 1-MiB-Blöcke) bzw. bei bzip2
  die BWT, die bei Text stark ist.
- *Dekompressionstempo*: ~550 MB/s – doppelt so schnell wie zlib und >10× schneller als
  xz/bzip2, aber langsamer als zstd (~800–1100 MB/s) und weit hinter LZ4 (>4 GB/s).
  Ein Teil der Zeit geht in das Rückgängigmachen der Filter.
- *Kompressionstempo*: 3–22 MB/s; durch die Filter-Probeläufe bewusst langsam.
  Für einen Asset-Build-Schritt meist akzeptabel, für Laufzeit-Kompression (z. B.
  Netzwerk, Savegames) wären LZ4/zstd -3 die bessere Wahl.

**Empfehlung nach Einsatzzweck:**

| Einsatz | Beste Wahl |
|---|---|
| Ladezeiten auf schnellen SSDs, CPU-Budget knapp | LZ4 / LZ4HC |
| Allround, Text-lastige Inhalte | zstd (-19 beim Build) |
| Download-Größe von Binär-Assets minimieren, Dekodierung trotzdem schnell | **GLZ** (bzw. die Idee „Vorfilter + schneller Entropie-Coder“) |
| Maximale Rate, Tempo egal (Archiv/Patch) | xz |

Eine praktische Erkenntnis: Die GLZ-Filter lassen sich auch **vor zstd** schalten –
in echten Pipelines ist das oft die einfachste Variante (so machen es z. B. Blosc
oder Textur-Kompressoren mit Vorverarbeitung).

## Grenzen dieses Vergleichs

- Synthetische Daten; echte Assets (insbesondere bereits GPU-komprimierte Texturen
  wie BC1–BC7 / ASTC) verhalten sich anders – dort bringen Byte-Shuffle-Filter weniger,
  spezialisierte Ansätze (Umsortierung der BCn-Blöcke) mehr.
- Ein Kern, ein Rechner, keine Wiederholungen über mehrere Läufe hinweg.
- GLZ hat keine Prüfsumme: Beschädigte Daten werden sicher (ohne Absturz) verarbeitet,
  aber nicht immer als fehlerhaft erkannt. Eine CRC/xxHash pro Block wäre der nächste Schritt.
- Mögliche Verbesserungen: optimales Parsing, tANS statt Huffman, Kontext über
  Blockgrenzen (Wörterbuch), SIMD-Filter.

## Nutzung

```bash
make                 # baut ./bench
make corpus          # erzeugt corpus/
./bench corpus/* > results.csv
python3 summarize.py results.csv
```

API (`glz.h`):

```c
size_t glz_bound(size_t n);
size_t glz_compress(const void *src, size_t n, void *dst, size_t cap, int level /*1..9*/, unsigned flags);
size_t glz_decompress(const void *src, size_t n, void *dst, size_t cap);   /* (size_t)-1 bei Fehler */
size_t glz_decompressed_size(const void *src, size_t n);
```
