# NOMP – requirements specification

This file is self-contained: it holds everything needed to develop NOMP in a separate
environment (own repository or session). It can be used as that project's `CLAUDE.md` or
`REQUIREMENTS.md`. The background and the reasoning behind each decision are in the design
draft `docs/NOMP.md` of the GPK repository (https://github.com/relinu/gpk, branch `rust-port`).

Keywords: **MUST** = hard requirement (a violation blocks the release), **SHOULD** = target
(deviation allowed with a measured justification), **MAY** = optional.

## 1. Goal

NOMP is a lossless compression core for game builds that achieves **the highest possible
compression ratio while unpacking at least as fast as the archive is downloaded**.
Decoding speed is a constraint (a time budget), not a quantity to maximize. Packing time has
the lowest priority.

## 2. Decisions already taken

| # | Decision |
|---|---|
| D1 | Name: **NOMP**. |
| D2 | **Design point "typical fiber"**: 400 Mbit/s download on a 6-core/12-thread CPU. |
| D3 | **Unpacking during the download** is required (streaming), see section 5. |
| D4 | **No GPU decoder.** The fast path keeps the number of interleaved rANS states per stream as a block parameter, so a GPU variant would need no format change. |
| D5 | The encoder distributes the decoding budget over segments (Lagrangian choice, section 6.5). |
| D6 | The code is developed in **Rust**; GPK's existing kernels (match finder, optimal parse, TCM model layout) may be reused or adapted. |

## 3. Decoding budget (MUST)

```
budget [CPU seconds] = effective_threads × compressed_size / download_rate
effective_threads    = cores × 1.25 (with SMT) − 1   (one thread for download, disk, system)
```

Design point (D2): 400 Mbit/s = 50 MB/s, 6 cores with SMT → 6.5 effective threads →
**≤ 0.13 s of decoding CPU time per compressed MB**, measured single-threaded on the reference
machine (section 9). For the GPK test build (~125 MB archive) that is **≤ 16 s**.

The budget is a parameter of the encoder (CLI: `--download <Mbit/s>`,
`--unpack-threads <n>`; plus `--fast` = fast path everywhere and `--max-ratio` = no budget).
The default MUST be the design point.

Other profiles the encoder MUST be able to target (for information; not the default):

| Profile | Download | Device | Budget per compressed MB |
|---|---:|---|---:|
| 5G | 200 Mbit/s | 4 cores/8 threads (handheld, laptop) | 0.16 s |
| Gigabit fiber | 1 Gbit/s | 8 cores/16 threads | 0.072 s |
| DSL/cable Germany (median) | 100 Mbit/s | 6 cores/12 threads | 0.52 s |

Researched basis (November 2026): fiber medians of leading providers ~360–370 Mbit/s (Ookla
2025); fixed broadband Germany ~102 Mbit/s median (Ookla 2025); 5G Germany ~110–190 Mbit/s
per operator (Opensignal, Nov. 2026), 5G SA Europe ~205–221 Mbit/s (Ookla); Steam CPUs:
8 cores 28.7 %, 6 cores 27.3 %, 4 cores ~13 % (Oct. 2026); PS5/Xbox Series 8C/16T, Steam Deck
4C/8T.

## 4. Functional requirements

### 4.1 Compression (MUST)
- Lossless: `decode(encode(x)) == x` bit for bit for every input, including empty input.
- Independent blocks of up to 64 MiB (one LZ window per block); blocks can be decoded in
  parallel.
- Within a block: segments of 1–4 MiB; per segment either the **fast path** (static rANS
  streams) or the **modelled path** (sparse context mixing for literals), chosen by the encoder.
- Deterministic encoder: the same input and parameters give the same output, independent of
  the thread count and the platform.

### 4.2 Decoding (MUST)
- Block decoder API that works on a complete block and fills a caller-provided output buffer.
- Decoding time per block ≤ **0.5 s** on the reference machine (limits the tail after the
  download ends); the encoder splits blocks accordingly.
- Memory per decoding thread: ≤ 128 MB of model tables + the block's output (SHOULD ≤ 96 MB).

### 4.3 Formats (MUST)
- Integers only in everything that influences the format or the decoded output (no floating
  point in models, mixers, probability updates or table construction).
- Every length, offset, table and copy is validated before use.
- Format version byte at the start of every block; unknown versions are rejected cleanly.

## 5. Streaming container (MUST, decision D3)

The archive must be unpackable while it is being downloaded:

- The **index comes first** (file list, block list with offsets/sizes/checksums), so the
  unpacker can create all files before their data arrives. (GPK's current format `GPK1` has
  the index at the end; the streaming container needs a new magic/version, e.g. `GPK2`.)
- Blocks are stored in decoding order. The unpacker starts a block as soon as it has fully
  arrived, decodes up to `--unpack-threads` blocks in parallel and writes the files directly.
- The unpacker reads from a **pipe or a growing file** (`gpk x - <dir>` reading stdin, and
  `gpk x --follow <file>`), with bounded memory: at most `threads + 2` blocks in RAM.
- Integrity: CRC32 per file (as GPK1) and a checksum per block, so that a corrupted download
  is detected per block.
- Packing may need two passes (blocks first, then index placed in front); the archive must
  still be written without holding all blocks in memory (e.g. reserve space for the index or
  write blocks to a temporary file).

## 6. Algorithm (summary of the design; details in `docs/NOMP.md`)

### 6.1 Structure
LZ commands `(literal run, match)`. Long repeats are copied; literals are coded by a sparse
context-mixing model (modelled path) or by static rANS tables (fast path).

### 6.2 Literal model library (modelled path)
Nibble-wise bit models (one 64-byte row per context and nibble: check value + 15 counters),
one mixer layer with a small selectable weight context, one APM stage. Models:
`o1`, `o2`, `o3`, `o4`, `o6`, `word`, `column`, `rec(s)` (stride-aware), `sparse`, `x86`,
`mismatch` (byte a continued match would produce), `match` (longest recent context match).
The encoder selects **0–6 models per segment**; the decoder only runs these. Models are updated
on coded literals only; contexts are computed from the full output.

### 6.3 Parsing
1. Cost map: run the full model library once over the block; record the literal cost per byte.
2. Optimal parse (binary-tree match finder + DP, as in GPK's LZRC) minimizing literal costs from
   the map + exact token costs. Matches only where cheaper than the model's literals.
3. Model selection per segment, final coding; optionally one more map → parse iteration.

### 6.4 Tokens
Kinds: repeat offsets R0–R3, **distance codebook** (≤ 16 characteristic distances per block),
new offset. Lengths/offsets as slots + extra bits; the lowest 0–4 offset bits with their own
model. Modelled path: adaptive binary range coding. Fast path: separate static rANS streams
(commands, literals with context map, lengths, offset slots, align bits, raw bits),
11–12-bit precision, interleaved states (block parameter, decision D4).

### 6.5 Budget distribution
For every segment `s` and configuration `k`: size `B(s,k)` and decoding time `t(s,k)` from a
calibrated cost model. Minimize `Σ B + λ × Σ t`, bisect λ until `Σ t ≤ budget`.

## 7. Targets (SHOULD)

Measured against GPK's LZRC on the same filtered blocks, and on the whole GPK test build:

| Quantity | Target | Go/no-go threshold (phase 1) |
|---|---|---|
| Size of binary blocks, modelled path | −5 to −9 % vs. LZRC | at least −4 % |
| Decoding, modelled path | 10–20 MB/s of output per thread | at least 10 MB/s |
| Decoding, fast path | ≥ 250 MB/s of output per thread | – |
| Size, fast path | ≤ +2 % vs. LZRC | – |
| Whole test build at the design point | ~121–123 MB, ≤ 16 s decoding CPU | – |
| Packing time | ≤ 4× GPK today | – |

If the go/no-go threshold is missed, NOMP is reduced to the fast path (LZRC ratio, much faster
decoding) and the modelled path is dropped.

## 8. Engineering rules (MUST)

- Rust, stable toolchain (GPK pins 1.97.0), edition 2021 or later.
- Library code: `#![forbid(unsafe_code)]`. No runtime CPU dispatch that needs `unsafe`;
  release builds target baseline x86-64 (no `target-cpu`).
- Corrupted or truncated input → `Err`, never a panic, an overread, an infinite loop or an
  unbounded allocation (all sizes are checked against limits before allocating).
- Platforms: Linux x86-64 and Windows x86-64. Windows is cross-compiled
  (`x86_64-pc-windows-gnu`, MinGW) and tested under Wine in CI. **Never use a Windows VM
  in GitHub Actions.** Archives must be byte-identical between Linux and Windows.
- No new runtime dependencies without need (GPK's default build uses only `crc32fast`).
- Language: everything in the repository is written in **English** (code, comments, strings,
  docs, CI, commit messages); the chat with the user is in **German**.
- Commits are pushed directly; CI must stay green.

## 9. Measurement rules

- **Reference machine**: the measurements in this file come from a 4-vCPU cloud VM
  (Intel Xeon @ 2.1 GHz, AVX-512 available but not used). A new environment MUST first
  re-measure the baseline (section 10) on its own machine; budgets are converted with the
  ratio of the baseline decoding times.
- Run-to-run noise on shared VMs is ±10 %: take the minimum of ≥ 5 runs and compare versions
  **interleaved** (A, B, A, B …), never sequentially.
- Instruction counts (`valgrind --tool=callgrind`) are a useful second metric for the
  decoders; latency-bound code (context mixing) must still be confirmed by time.
- Download simulation for the streaming unpacker: throttled pipe, e.g.
  `pv -q -L 50m archive.gpk | gpk x - out` for 400 Mbit/s (also 25m and 125m). Report the
  time from the first byte to the last file written, and the time after the download ended.

## 10. Baseline (measured, GPK `rust-port` at commit `d809ce1`)

Test build: `python3 tools/make_build.py testbuild/MyGame` from the GPK repository
(~327.6 MB; uses real system libraries of the host, so the exact bytes depend on the system –
re-measure in a new environment).

| Method | Archive | Decoding CPU (1 thread) |
|---|---:|---:|
| GPK today (LZRC + TCM for text) | 125.5 MB | 29.7 s |
| GPK `--no-cm` (LZRC only) | 126.3 MB | 8.1 s |
| tar + xz -9e | 149.2 MB | – |
| tar + zstd -19 --long=27 | 162.8 MB | – |

Where GPK's time goes: TCM ~22 s (text 25 MB → 2.3 MB), LZRC ~4.5 s (binary 225.8 MB →
67.3 MB, 60–70 MB/s), PNG/JPEG inverse ~3.5 s, stored data (56 MB: Ogg, transformed
PNG/JPEG) ~0 s.

Upper bound by context mixing (zpaq -m5 on GPK's filtered blocks):

| Block | GPK | zpaq -m5 |
|---|---:|---:|
| x86 code, 57.9 MB | 13.0 MB | 10.9 MB (−16 %) |
| binary, 66.9 MB | 24.6 MB | 20.7 MB (−16 %) |
| binary, 62.5 MB | 16.3 MB | 16.0 MB (−2 %) |
| text, 25.1 MB | 2.3 MB (TCM) | 2.0 MB |

Extrapolated: ~117 MB for the whole build – the ceiling NOMP works toward, at a fraction of
the decoding time.

## 11. Tests (MUST)

- Round trip: empty, 1 byte, random data, highly repetitive data, every GPK filter output,
  block sizes at the limits, segment boundaries, all model selections, fast and modelled path.
- Corrupted input: random byte flips and truncations of real blocks (thousands of cases), no
  panic; streaming container with truncated downloads.
- Determinism: identical archives with 1 and N threads; Linux ↔ Windows (Wine) cross pack and
  unpack.
- Budget: a test that checks the encoder's predicted decoding time against the measured one
  (tolerance ±25 %) on the test build.
- CI (GitHub Actions, Ubuntu only): build, clippy with `-D warnings`, `cargo fmt --check`,
  tests, smoke test, Windows cross build + Wine tests, timeout per job.

## 12. Phases and deliverables

1. **Measurement prototype** – cost map, parse, literal models with fixed selection; measures
   literal share, size gain and decoding speed. Deliverable: table + go/no-go decision.
2. **Coder** – segment format, model selection, λ distribution, fast path (rANS), block
   decoder API, cost model calibration.
3. **Streaming container** – index first, block checksums, unpacking from a pipe/growing file,
   bounded memory, tail ≤ 0.5 s.
4. **Hardening** – corrupted input, determinism, fuzz loop, Windows under Wine.
5. **Final measurement** – test build at all four profiles, throttled downloads
   (25/50/125 MB/s), comparison with xz/zstd/GPK today; documentation.

## 13. Out of scope

GPU decoding (D4), lossy compression, runtime asset streaming into video memory, delta/patch
updates (may follow as a separate project), compression levels for speed beyond `--fast`.
