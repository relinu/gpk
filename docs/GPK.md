# GPK – download packer for game builds

GPK takes a finished game build (a directory) and creates an archive for download.
Goals: downloads as small as possible, bit-exact restoration (CRC32 per file), parallel
unpacking.

```bash
cargo build --release                     # builds target/release/gpk
gpk c -t 4 <build-dir> game.gpk           # pack
gpk x -t 4 game.gpk <target>              # unpack (checks all CRCs)
gpk l game.gpk                            # show contents and chosen filters
cargo test --release --features cref,xz   # tests + differential tests against the C reference
```

## Memory usage

The RAM requirement does **not** depend on the build size:

- Finished blocks are written to the archive immediately, in a fixed order (the same input
  always gives byte-identical archives, independent of the number of threads). The most
  expensive blocks are started first, as long as the started but unwritten blocks together
  take at most `(threads + 1) × block size`.
- Files are only read range by range, each time for the current processing step.
- Transformed PNG/JPEG data is spilled to `<archive>.spill.tmp` next to the archive and
  deleted at the end.
- When unpacking, each thread only reads the block it is currently unpacking. For text blocks
  each thread keeps the tables of the text core (~125 MB) and only resets them instead of
  allocating them again for every block.

| Build | Pack (4 threads) | Unpack (4 threads) |
|---:|---:|---:|
| 2.9 GB | 2.4 GB RAM (before: 6.3 GB) | – |
| 5.6 GB | 2.7 GB RAM | 0.5 GB RAM (before: 6.1 GB) |
| 15.4 GB | 2.3 GB RAM | 0.5 GB RAM |

(Measured with the C version. When unpacking text blocks, the Rust version needs up to
~0.65 GB because each thread keeps the text core tables.)

Rule of thumb for packing: about 11 × block size per thread (LZRC binary tree + buffers).
With `--max-ram GB`, GPK chooses the number of threads and, if necessary, smaller blocks to fit
the budget, e.g. `--max-ram 1` → 1 thread, measured 0.7 GB. Fewer threads or smaller blocks
make packing slower or the archive slightly larger.

## Pipeline

1. **Scan**: files, directories, symlinks, permissions (e.g. the executable bit), timestamps
2. **Deduplicate**: identical files only once (CRC32 + size + byte comparison)
3. **Transform** (always fully reconstructed and compared before it is accepted):
   - **PNG**: inflate the IDAT Deflate stream if zlib can reproduce it bit-exactly
     (Precomp/Preflate principle). Inflating and recompressing is done by an own
     implementation (`src/inflate.rs`, `src/deflate.rs`) that works bit-exactly like zlib 1.4;
     the search over level, memLevel and strategy compares continuously and stops at the first
     differing block (the zlib header byte also reveals the level class). Then, per image, the
     smaller of two variants: raw data for LZRC or the **pixel coder PXC** (remove the row
     filters, weighted mix of 6 predictors, optional color transform, context arithmetic coding)
   - **JPEG** (`src/jpgx.rs`): baseline/extended with Huffman, 1–4 components, any
     subsampling, restart markers. Decode the scan into DCT coefficients, re-encode them with a
     context model (neighboring blocks), recreate the Huffman stream exactly when unpacking.
     Progressive/arithmetic JPEGs stay unchanged.
4. **Classify & choose filters** per file, by trial compression with the LZRC core:
   | Filter | for | Effect |
   |---|---|---|
   | x86 split | EXE/DLL/SO | operands of CALL/JMP/Jcc in a separate stream, as absolute targets (BCJ2 principle, length-preserving) |
   | x86 BCJ | EXE/DLL/SO | relative CALL/JMP targets → absolute (same targets = same bytes) |
   | field split | DDS BC1–BC5 | endpoints and indices of the 4×4 blocks separated |
   | word prediction | vertex buffers, PCM, height maps | predict 16/32-bit values from the previous record (order 1/2), residuals in byte planes |
   | delta / shuffle | other structured data | byte delta or byte planes with a detected stride (1–64 bytes) |
   | – | Ogg, video, non-reproducible PNG | detected as "already compressed" and only stored |
5. **Sort** by class, extension and name (similar files end up in the same window)
6. **Compress** in independent 64 MiB blocks, in parallel, with **LZRC**
   (LZ77 + binary-tree match finder + optimal parsing + adaptive range coder, LZMA principle,
   own implementation) – optionally `--xz` as the core (only with `--features xz`).
   **Text blocks** (2 MiB, pure text) are coded with the context-mixing core **TCM**
   (orders 0–6, word, column and match models, two mixers, two APM stages); LZRC is only also
   tried there if a fast estimate gives it a chance (almost never on text). Can be disabled
   with `--no-cm`. TCM unpacks only ~1–1.5 MB/s per thread, so the largest text block bounds
   the unpacking time on many cores: 4 instead of 8 MiB cost +0.45 % text size, 2 instead of
   4 MiB another +0.04 % of the archive (test build: +54 KB) and halves the longest block.
   Stored blocks hold at most 2 MiB of PNG/JPEG transformations, so that their inverse
   (pixel and coefficient decoding) is also spread over the threads.

All cores use **integers only**: Linux and Windows create and read the same archives
bit-identically (checked in CI: Linux ↔ Windows .exe under Wine).

## Speed (Rust version)

`GPK_TIMINGS=1 gpk …` prints phase timings when packing and per-block timings when unpacking.

- **Filter choice**: structured data has up to ~13 filter candidates. A fast estimator
  (`lzrc::estimate`: same model and same range coder, but greedy parsing instead of the
  optimal DP) preselects the two most promising ones; only these and "no filter" are really
  trial-compressed. Measured: classification 21.3 → 12.5 s, archive +2.5 KB.
- **Order**: when packing, classifying and unpacking, the most expensive tasks run first
  (text CM blocks are ~40× more expensive than LZRC) so that no large chunk runs alone at the end.
- Tried and discarded (no measurable gain): other parse windows, price update intervals and
  search depths in the LZRC encoder (all within ±0.1 %).
- Limit when packing: a single 64 MiB LZRC block alone takes ~45 s (the binary tree grows to
  0.5 GB and the search gets slower); with large blocks it determines the minimum duration.

| Test build 308 MB, 4 threads | before | after |
|---|---:|---:|
| Archive | 120,266,104 B | 120,278,468 B (+0.01 %) |
| Pack | 92.8 s, 1.46 GB RAM | 64.3 s, 1.67 GB RAM |
| Unpack | 8.3 s, 0.44 GB RAM | 6.8 s, 0.61 GB RAM |

### Unpacking

Unpacking matters more than packing (it runs on every player's machine). All changes keep the
archive format; the kernels stay byte-identical to the C reference.

- **Text core (TCM)**: ~75 % of the unpacking CPU time. It is not memory-bound (tables of
  64 KB instead of 112 MB were only ~6 % faster) but bound by the instruction count per bit.
  Removed: bounds checks and the double indirection of the hash tables (one table array with
  a power-of-two size and masked row indices, the order-0 rows live in its spare part),
  `squash`/`stretch∘squash` as direct lookup tables instead of interpolation, fixed-size
  mixer and APM tables, balanced dot-product sums. 845 → 690 instructions per bit, ~13–15 %
  faster. A copy of the current rows into a working buffer was slower (store forwarding).
  Compiling for AVX2 (`-C target-cpu=native`) would give another ~10 %, but releases target
  baseline x86-64 and a runtime CPU dispatch would need `unsafe`.
- **LZRC decoder**: literal and tree bits are decoded without data-dependent branches
  (select masks instead of `if`), the matched literal uses the offset trick of the LZMA SDK.
  +10–20 % (now 60–70 MB/s; xz/liblzma: ~50 MB/s on the same data).
- **PNG pixel coder (PXC)**: the per-predictor weights `2^40 / e²` come from a table for
  e < 1024, the remaining 64-bit divisions are done exactly in f64 (proof in the code). ~5 %
  here; much more on CPUs with slow 64-bit `idiv` (up to ~90 cycles before Ice Lake/Zen 2).
- **JPEG**: Huffman bit writer with a 64-bit accumulator instead of bit by bit, non-zero
  counts of neighbour blocks are cached, encoder/decoder are separate instantiations
  (const generic). ~15 %.
- **Scheduling**: 2 MiB text blocks and stored blocks with at most 2 MiB of PNG/JPEG
  transformations (see above); before, one stored block with 18 MB of transformations took
  3.5 s alone on one thread.

| Test build 327.6 MB (with JPEGs) | before | after |
|---|---:|---:|
| Archive | 125,489,637 B | 125,544,160 B (+0.04 %) |
| Unpack, 4 threads | 10.3 s, 0.66 GB RAM | **8.75 s** (−15 %), 0.75 GB RAM |
| Unpack, 1 thread | 33.1 s | **29.7 s** (−10 %) |
| Longest block (bounds unpacking on ≥ 8 threads) | 5.6 s | **2.4 s** |

Simulated from the measured block times (ideal scheduling): 8 threads 5.6 → 4.3 s,
16 threads 5.6 → 2.4 s. Measured back to back on a shared 4-vCPU VM (±10 % noise).

## Results v3 (extended test build 327.6 MB, with 36 JPEGs)

| Method | Size after | Difference | Pack | Unpack (4 threads / 1 thread) |
|---|---:|---:|---:|---:|
| zip -9 (Deflate per file) | 187.2 MB | −42.9 % | 40.0 s | 3.1 s |
| tar + zstd -19 --long=27 | 162.8 MB | −50.3 % | 191.8 s | 0.9 s |
| tar + xz -9e | 149.2 MB | −54.5 % | 311.1 s | 7.3 s |
| GPK v2 | 128.5 MB | −60.8 % | 105.4 s | 2.7 s / 7.5 s |
| GPK v3 `--no-cm` | 126.3 MB | −61.5 % | 100.6 s | 4.1 s / 9.3 s |
| **GPK v3** | **125.5 MB** | **−61.7 %** | 118.5 s | 9.9 s / 33.5 s |

All GPK runs verified bit-exactly. Raw data: `results_v3.csv`.

### What v2 → v3 gained per component

| Component | Group | v2 | v3 | Comparison |
|---|---|---:|---:|---|
| JPEG recompression | 36 JPEGs, 6.9 MB | ≈6.9 MB | 5.2 MB (−25 %) | 86 test JPEGs: 70 bit-exact, 16 rejected (progressive etc.), 0 errors |
| Text core TCM | text 25.1 MB | 2.99 MB | 2.21 MB (−26 %) | PPMd 2.41 MB, ZPAQ -m5 1.96 MB; unpacks ~1 MB/s per core |
| Pixel coder PXC | PNG 10.8 MB | 6.06 MB | 5.73 MB (−5.4 %) | lossless JPEG XL 5.27 MB |
| x86 split | code 66.6 MB | 13.10 MB | 13.03 MB (−0.5 %) | 7-Zip BCJ2 −5 % vs. BCJ; a selective variant measured only −1.5 % more |

**Trade-off:** the text core saves the most per byte but makes unpacking slow (here 9.9 instead
of 4.1 s with 4 threads). For fast installations use `--no-cm` – then the JPEG, PNG and x86
improvements remain with hardly slower unpacking.

## Results v2 (test build 320.6 MB, 954 entries, without JPEGs)

| Method | Size after | Difference | Pack | Unpack |
|---|---:|---:|---:|---:|
| zip -9 (Deflate per file) | 180.3 MB | −43.7 % | 30.6 s | 2.1 s |
| tar + zstd -19 --long=27 | 155.9 MB | −51.4 % | 141.6 s | 0.7 s |
| tar + xz -9e | 142.2 MB | −55.7 % | 236.3 s | 4.9 s |
| GPK v1 | 133.4 MB | −58.4 % | 57.3 s | 1.5 s |
| **GPK v2 (optimized)** | **121.7 MB** | **−62.0 %** | 76.0 s | 2.0 s (1 thread: 5.2 s) |

Complete table including ablations: `comparison.md`, raw data: `results_build.csv`.

### What the optimization v1 → v2 gained (measured per asset group)

| Group | v1 | v2 | Cause |
|---|---:|---:|---|
| Meshes | 25.6 MB | 18.6 MB | stride detection up to 64 bytes (vertex = 48 bytes), word prediction, filter choice with LZRC instead of the GLZ estimator |
| PNG (UI) | 10.8 MB | 6.1 MB | PNG recompression |
| WAV | 8.5 MB | 8.3 MB | word prediction |
| Textures, code, text | unchanged | | tested: other split layouts and lc/lp/pb variants gain ≤ 0.5 % |

The gains come from **preprocessing**. The LZRC core is at xz level (with `--xz` the archive is
0.3 MB smaller).

## Findings from research

- **Steam (SteamPipe)** splits files into ~1 MB chunks, compresses each with LZMA and
  encrypts it; updates only download changed chunks. Valve advises *not* to compress pack files
  yourself. According to analyses, Epic also works with ~1 MiB chunks.
  → GPK's solid 64 MiB blocks are ideal for the **initial download**, but bad for **updates**:
  a small change invalidates a whole block.
- **Oodle Texture** (RAD/Epic): RDO encoding shrinks BCn textures after compression by up to
  ~50 %, but it is lossy (a texture encoder, not a packer). The lossless **BC7Prep** gains
  5–15 % on BC7 according to RAD.
- **Precomp / Preflate**: inflate Deflate streams (PNG, ZIP, PDF) and reconstruct them
  bit-exactly; Preflate manages this even for non-zlib encoders with small correction data.
  GPK uses the simple variant (only zlib-reproducible streams, ~54 % of the real PNGs tested).

## Limits / next steps

- The test build is partly synthetic (code, scripts and fonts are real).
- **Make it update-friendly**: content-defined chunking (e.g. ~1 MiB) and chunk dedup against
  the previous version so that patches stay small.
- Preflate-like correction data for PNGs from other encoders; BC7 transform (BC7Prep idea).
- Progressive JPEGs (stored unchanged today); make the text core faster (e.g. a PPMd-like
  model) or use it only for large amounts of text.
- Packing is slow on purpose (trial compressions); unpacking is parallel and faster than
  typical download rates.
