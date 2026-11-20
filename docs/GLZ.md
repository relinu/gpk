# GLZ – a compressor for game assets

GLZ ("Game LZ") is a lossless compressor written in C (`legacy-c/glz.c`, ported to Rust in
`src/glz.rs`) that is tailored to the typical requirements of computer games, compared with
LZ4, zlib, zstd, bzip2 and xz.

## Design decisions

| Requirement in a game | Implementation in GLZ |
|---|---|
| Assets are packed **once** at build time but unpacked **on every load** | Asymmetric: expensive compression (filter trial runs, hash chains up to depth 1024, lazy matching), simple decoder |
| Streaming, parallel loading, random access in package files | Independent 1 MiB blocks with their own size in the header (skippable, decodable in parallel) |
| Lots of binary data: textures, vertex buffers, audio, height maps | **Automatic pre-filters per block**: stride detection (1–32 bytes via delta entropy), then trial compression with *none*, *delta*, *byte shuffle*, *shuffle+delta*; the smallest wins |
| Structured data with fixed distances | LZ77 with **2 repeat offsets** (cheaply coded repetition of the last distance) |
| Fast decoding | Canonical **Huffman, max. 11 bits** → one 2048-entry lookup table, 4 symbols per refill of the 64-bit bit buffer; 8-byte wild copy for matches |
| Already compressed files (Ogg, JPEG …) | The block is stored raw if compression does not help |
| Corrupted files / mods | The decoder checks all bounds; fuzz test with ASan/UBSan without findings |

### Data format (per block)

```
filter(u8) stride(u8) mode(u8) rawsize(u32) payloadsize(u32) payload
payload = nseq | HUF(literals) | HUF(tokens) | HUF(offset codes) | length overflows (LEB128) | offset extra bits
token   = (min(literal length,15) << 4) | min(match length-4, 15)
offset code: 0 = rep0, 1 = rep1, 2+b = offset (1<<b) + b extra bits
```

Why the filters help so much: in a vertex buffer (32-byte vertices of floats) or 16-bit audio,
neighboring values change only a little, but that is invisible to a byte-wise LZ matcher.
*Byte shuffle* groups all 1st bytes, all 2nd bytes … together (the high bytes become almost
constant), *delta* turns slowly changing values into small numbers that Huffman codes cheaply.

## Test corpus

Synthetic, but realistic and generated deterministically (`tools/gen_corpus.py`, ~19 MB):
RGBA albedo texture 1024², 16-bit height map, normal map, terrain mesh (vertex and index
buffers, tile order as after a cache optimization), 16-bit stereo PCM, the tile map of a level,
dialog/localization JSON, a save game with entity structs and an already compressed (random)
blob.

## Results

Measured on 1 core (`-O2 -march=native`), system libraries zlib 1.3, zstd 1.5.5, LZ4 1.9.4,
liblzma 5.4.5, bzip2 1.0.8. Every run was verified by a round trip.

### Compression ratio per file (original / compressed, higher = better)

| File | GLZ -1 | GLZ -5 | GLZ -9 | GLZ -9 (no filter) | LZ4 | LZ4HC -12 | zlib -6 | zlib -9 | zstd -3 | zstd -19 | bzip2 -9 | xz -6 |
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

### Overall result across all files

| Codec | Total size (MB) | Overall ratio | Compression MB/s | Decompression MB/s |
|---|---:|---:|---:|---:|
| *Original* | 19.15 | 1.00 | – | – |
| GLZ -1 | 8.43 | 2.27 | 22.0 | 584 |
| GLZ -5 | 8.25 | 2.32 | 8.5 | 535 |
| GLZ -9 | 8.19 | 2.34 | 3.0 | 565 |
| GLZ -9 (no filter) | 11.22 | 1.71 | 4.6 | 576 |
| LZ4 | 15.17 | 1.26 | 914.4 | 4746 |
| LZ4HC -12 | 13.36 | 1.43 | 14.6 | 4244 |
| zlib -6 | 11.55 | 1.66 | 29.4 | 269 |
| zlib -9 | 11.53 | 1.66 | 13.3 | 272 |
| zstd -3 | 11.83 | 1.62 | 288.7 | 1122 |
| zstd -19 | 10.63 | 1.80 | 3.5 | 776 |
| bzip2 -9 | 10.00 | 1.92 | 15.9 | 22 |
| xz -6 | 9.43 | 2.03 | 2.7 | 42 |

### Filters chosen by GLZ -9 (per 1 MiB block)

| File | Filter |
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

## Assessment

**Where GLZ wins:** on binary game assets – audio, height maps, meshes, index buffers – GLZ is
clearly ahead, often even ahead of xz and bzip2. Overall the archive is **8.2 MB instead of
10.6 MB (zstd -19) or 9.4 MB (xz)**, i.e. ~23 % smaller than zstd -19. The ablation "no
filter" (11.2 MB) shows that the advantage comes almost entirely from the automatic
pre-filters; the LZ+Huffman back end alone is roughly at zlib level.

**Where GLZ loses:**
- *Text/JSON and tile maps*: zstd -19, xz and bzip2 are better. They have optimal parsing
  (instead of lazy matching), larger windows (GLZ: 1 MiB blocks), or, for bzip2, the BWT,
  which is strong on text.
- *Decompression speed*: ~550 MB/s – twice as fast as zlib and >10× faster than xz/bzip2, but
  slower than zstd (~800–1100 MB/s) and far behind LZ4 (>4 GB/s). Part of the time goes into
  undoing the filters.
- *Compression speed*: 3–22 MB/s; deliberately slow because of the filter trial runs.
  Usually acceptable for an asset build step; for runtime compression (e.g. network, save
  games) LZ4/zstd -3 would be the better choice.

**Recommendation by use case:**

| Use case | Best choice |
|---|---|
| Load times on fast SSDs, tight CPU budget | LZ4 / LZ4HC |
| All-round, text-heavy content | zstd (-19 at build time) |
| Minimize the download size of binary assets while keeping decoding fast | **GLZ** (or the idea "pre-filters + fast entropy coder") |
| Maximum ratio, speed irrelevant (archive/patch) | xz |

A practical finding: the GLZ filters can also be placed **in front of zstd** – in real
pipelines this is often the simplest variant (this is what e.g. Blosc or texture compressors
with preprocessing do).

## Limits of this comparison

- Synthetic data; real assets (especially textures that are already GPU-compressed such as
  BC1–BC7 / ASTC) behave differently – there byte-shuffle filters help less and specialized
  approaches (reordering the BCn blocks) more.
- One core, one machine, no repetitions across several runs.
- GLZ has no checksum: corrupted data is handled safely (without crashing), but not always
  detected as faulty. A CRC/xxHash per block would be the next step.
- Possible improvements: optimal parsing, tANS instead of Huffman, context across block
  boundaries (dictionary), SIMD filters.

## Usage

```bash
make -C legacy-c                       # builds legacy-c/bench
python3 tools/gen_corpus.py corpus     # creates corpus/
legacy-c/bench corpus/* > results.csv
python3 tools/summarize.py results.csv
```

API (`legacy-c/glz.h`):

```c
size_t glz_bound(size_t n);
size_t glz_compress(const void *src, size_t n, void *dst, size_t cap, int level /*1..9*/, unsigned flags);
size_t glz_decompress(const void *src, size_t n, void *dst, size_t cap);   /* (size_t)-1 on error */
size_t glz_decompressed_size(const void *src, size_t n);
```
