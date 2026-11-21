# NOMP – design of an own compression core (draft, not implemented)

Status: **design for review**. Nothing in this document is implemented yet. Figures for NOMP
are targets or estimates; all other figures are measurements on the extended test build
(327.6 MB, 4-vCPU VM, release build of `d809ce1`).

Goal in one sentence: **the highest compression ratio that can still be unpacked as fast as
the archive is downloaded.** Unpacking speed is not maximized; it is a constraint.

## 1. Starting point

| Method (test build) | Archive | Unpack, CPU time (1 thread) |
|---|---:|---:|
| GPK today (LZRC + TCM for text) | 125.5 MB | 29.7 s |
| GPK `--no-cm` (LZRC only) | 126.3 MB | 8.1 s |
| Estimate with zpaq -m5 on the LZRC/TCM blocks (measured per block, extrapolated) | ~117 MB | minutes |

Where the time goes today: TCM ~22 s (text, 25 MB → 2.3 MB), LZRC ~4.5 s (binary,
225.8 MB → 67.3 MB), PNG/JPEG inverse ~3.5 s, stored data ~0.

zpaq -m5 (context mixing) versus today, per block: x86 code 13.0 → 10.9 MB (−16 %),
binary block 3 24.6 → 20.7 MB (−16 %), block 4 16.3 → 16.0 MB (−2 %), text 2.3 → 2.0 MB.
That is the ratio range worth chasing; its price is a decoder as slow as the encoder.

## 2. The constraint: decoding must keep up with the download

If the archive is unpacked while it is downloaded, the decoder has to consume compressed data
at least as fast as it arrives. With `T` decoding threads and a download rate `D`, the
**total CPU time** for decoding may be at most

```
budget = T × archive_size / D
```

(plus a short tail: the last block can only start when it has arrived, see section 9).

| Download | Archive (125 MB) arrives in | Budget with 4 threads | 8 threads | 16 threads |
|---|---:|---:|---:|---:|
| 100 Mbit/s | 10.0 s | 40 s | 80 s | 160 s |
| 250 Mbit/s | 4.0 s | 16 s | 32 s | 64 s |
| 500 Mbit/s | 2.0 s | 8 s | 16 s | 32 s |
| 1 Gbit/s | 1.0 s | 4 s | 8 s | 16 s |

Today's 29.7 s fit 100 Mbit/s on 4 threads, but not 500 Mbit/s on 8 threads.
**Proposed design point: 500 Mbit/s on 8 threads → ≤ 16 s CPU for this build**
(≈ 0.13 s per compressed MB). The design point is a parameter, not a constant (section 8).

Important consequence: the budget is per **compressed** byte. Data that compresses very well
(text, 11:1) gets very little time per raw byte; data that barely compresses gets a lot. A
context-mixing coder on text (TCM: ~10 s per compressed MB) is about 75× over budget, a
context-mixing coder on binary literals can fit.

## 3. Core idea: spend the time budget where it buys the most bytes

NOMP codes every block as LZ commands (`literal run + match`), like LZRC. The difference is
what happens to each byte:

1. **Long repeats are copied** – cost ~0 per byte. The parse leaves to the literal coder
   whatever a match codes worse (section 5).
2. **Literals are coded by a sparse context-mixing model** whose model set the **encoder
   chooses per segment** (1–4 MiB): out of a library of ~15 context models the encoder picks
   the 0–6 that pay off; the decoder only runs these. With 0 models, literals use static rANS
   tables (fast path, from the earlier STRATA draft).
3. **The encoder distributes the time budget**: for every segment it knows the coded size and
   the decoding cost of each configuration; it chooses configurations so that the total size
   is minimal while the total decoding time stays within the budget (a Lagrangian / knapsack
   choice, section 8). Segments where modelling buys many bytes per millisecond (x86 code,
   structured binary data) get rich models, others get the fast path.

All modelling decisions are made once by the encoder and stored; the decoder executes them.
That is where the asymmetry lies: the encoder tries dozens of configurations, the decoder
runs exactly one.

## 4. Literal model library

Every model maps a context to a slot of bit probabilities (nibble-wise as in TCM: one
64-byte row per context and nibble, check value + 15 counters), so a byte needs 2 row
lookups per model instead of 8.

| Model | Context | Useful for |
|---|---|---|
| `o1`, `o2`, `o3`, `o4`, `o6` | previous 1–6 bytes (hashed) | everything |
| `word` | hash of the current/previous word | text, scripts |
| `column` | column in the line + byte above | text, tables, CSV |
| `rec(s)` | bytes at −s and −2s, position mod s (s = detected stride) | meshes, BCn, audio, level data |
| `sparse` | bytes at −2, −3 (skipping −1) | 16/32-bit fields |
| `x86` | opcode position, previous opcode, ModRM (after GPK's x86 filter) | executables |
| `mismatch` | the byte a continued match would have produced (LZMA "matched literal") | first literal after a match |
| `match` | predicted byte from the longest recent context match (also short ones the parse did not take) | everything |

Mixing: one mixer layer with the chosen inputs + bias, weight set selected by a small context
(bit position, `mismatch` agreement), then one APM stage. Integers only, as all GPK cores.

Models are updated only on coded literal bytes; their contexts are always computed from the
full output (matched bytes included). This keeps decoding cost proportional to the number of
literals, not to the block size.

## 5. Encoder: parsing with a literal cost map

The parse must know what a literal costs **under the context model**, which is far less than
8 bits for predictable data. NOMP therefore parses in two steps:

1. **Cost map**: run the full model library once over the block (no matches) and record for
   every byte the cost `c[i]` in bits under the best configuration of its segment.
2. **Optimal parse** (LZRC's binary-tree match finder and DP, reused): minimize
   `Σ c[i]` over literal bytes `+` exact token costs over matches. A match is only taken where
   it is cheaper than the model's literal costs – typically long repeats, while short and
   "fuzzy" repeats stay with the model, which predicts them better.
3. Model selection per segment on the resulting literals (section 8), final coding.
   Optionally one more iteration of map → parse with the selected configuration.

The cost map is the encoder's most expensive step (a full CM pass, ~1–2 MB/s per thread);
packing gets roughly 2–4× slower than today. Acceptable by the project's priorities.

## 6. Token layer (matches)

- Kinds: repeat offsets `R0`–`R3` (LRU), **distance codebook** `D` (up to 16 characteristic
  distances per block: record size, texture row pitch, …, chosen from the distance
  histogram), new offset `N`.
- Lengths and offsets with slots + extra bits; the lowest 0–4 offset bits with their own
  model (structured data has aligned offsets).
- Coded with adaptive binary range coding (as LZRC): tokens are few compared to bytes, so the
  slower but stronger adaptive coding is affordable here.
- In the fast path (segments without models) tokens and literals use static rANS tables in
  separate streams (from the STRATA draft): one table lookup per symbol.

## 7. Block and segment format (sketch)

```
block   = u8 version | varint raw_size | params | segment*
params  = stride | x86 flag | distance codebook | offset align bits
segment = varint raw_len | u8 mode (0 = fast, 1 = modelled)
          | mode 0: rANS tables + streams (commands, literals, lengths, offsets, raw bits)
          | mode 1: u16 model mask | u8 mixer context | range-coded token+literal stream
```

Model tables persist across the segments of a block (only the selection changes), so
switching configurations costs no relearning. Every length, table and copy is validated
before use; corrupted input yields an error, never a panic.

## 8. Choosing configurations: the exchange rate λ

For every segment `s` and configuration `k` the encoder knows size `B(s,k)` and decoding
time `t(s,k)` (from a calibrated cost model: per-literal cost per model, per-token cost,
copy cost). It minimizes

```
Σ B(s, k_s) + λ × Σ t(s, k_s)
```

and raises λ until `Σ t ≤ budget` (bisection on λ; each step is a cheap per-segment choice).
λ is the "price" of a second of decoding in bytes; it falls out of the budget automatically.

CLI (proposal): `--download 500` (Mbit/s) and `--unpack-threads 8` define the budget;
`--max-ratio` drops the budget (archival use), `--fast` selects the fast path everywhere.
TCM and LZRC remain available as codecs; the same λ rule decides between them and NOMP.

## 9. GPK container changes needed for unpacking during the download

- The index is currently at the end of the archive (`"GPK1" | … | blocks | index`). For
  streaming, the index must come **first** (written after packing into a reserved area or by
  a second pass), so that files can be created before their blocks arrive.
- Blocks are written in decode order; the unpacker starts a block as soon as it is complete.
- **Tail latency**: the last block only starts after the download ends. Blocks are therefore
  limited by decoding time (target ≤ 0.5 s per block on the reference machine), not only by
  size.

## 10. Estimates for the test build (to be verified)

Assumptions that phase 1 measures: share of literal bytes after the cost-map parse (assumed
20–35 % on binary data, 30–50 % on text), decoding speed of the modelled path
(assumed 3–5 MB/s per literal byte with 3–4 models, i.e. 10–20 MB/s of output),
ratio gain of the models on literals only (assumed 60–80 % of zpaq's gain).

| Design point | Archive | Unpack CPU (1 thread) | Keeps up with |
|---|---:|---:|---|
| Today | 125.5 MB | 29.7 s | 250 Mbit/s with 8 threads |
| NOMP `--fast` | ~126–127 MB | ~5 s | 1 Gbit/s with 8 threads |
| **NOMP, 500 Mbit/s × 8 threads** | **~121–123 MB** | ≤ 16 s | 500 Mbit/s with 8 threads |
| NOMP, 100 Mbit/s × 8 threads | ~119–121 MB | ≤ 80 s | 100 Mbit/s with 8 threads |
| Context-mixing reference (zpaq -m5) | ~117 MB | minutes | – |

The range of the main design point (−2 to −3.5 % archive size *and* half the decoding time
of today) is the claim that phase 1 has to confirm.

## 11. What is borrowed, what is new

Borrowed: LZ77 with repeat offsets and optimal parsing (LZMA), bitwise context mixing with
nibble slots (paq family, TCM), matched-literal context (LZMA), rANS (Duda), separate static
streams (zstd, Oodle), LZ + modelled literals in general (e.g. ROLZ-based packers).

New in this combination:
- the **decoding-time budget derived from the download rate** as an explicit constraint, and
  the Lagrangian distribution of that budget over segments;
- **encoder-selected sparse model sets** per segment, so the decoder only runs models that
  pay off there;
- parsing against a **literal cost map of the context model**, so matches only replace bytes
  the model would code worse;
- the **distance codebook** and stride-aware record models for game data;
- one format that spans the whole range from rANS-only to context mixing.

## 12. Risks and go/no-go

| Risk | Consequence | Check in phase 1 |
|---|---|---|
| Too many literals after the parse | modelled path too slow, budget forces the fast path | literal share per block type |
| Models on literals alone gain much less than full CM | ratio near LZRC | size with 3–4 models on the 4 binary blocks |
| Cost map → parse is inconsistent (costs change once matches are taken) | worse parse | second iteration, compare |
| Time cost model inaccurate on other CPUs | budget missed | calibrate on several machines, safety margin |

**Go/no-go after phase 1**: on the binary blocks, the modelled path must reach at least
−4 % size versus LZRC at ≥ 10 MB/s of output per thread. Otherwise NOMP is reduced to its
fast path (the former STRATA goal: LZRC ratio, much faster decoding).

## 13. Plan

1. **Measurement prototype**: cost map + parse + literal models with fixed selection, no
   final format. Measures the three assumptions of section 10. Go/no-go.
2. **Coder**: segment format, model selection, λ distribution, fast path (rANS).
3. **Container**: index first, decode-time-limited blocks, streaming unpacker
   (`gpk x` from a pipe or a growing file).
4. **Hardening**: corrupted input, determinism (1/4 threads, Linux ↔ Windows under Wine),
   fuzz loop.
5. **Final measurement**: size and decoding time per design point, simulated download
   (throttled pipe) at 100/500/1000 Mbit/s.

## 14. Open decisions

- **Reference point**: 500 Mbit/s on 8 threads as proposed, or another one (e.g. 1 Gbit/s,
  or a weaker machine with 4 threads)?
- **Streaming unpack** (section 9) as part of NOMP, or keep unpacking from a complete file
  and only use the budget as the speed target?
- Replace LZRC/TCM in GPK completely once NOMP is ready, or keep them as additional codecs?
