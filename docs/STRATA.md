# STRATA – design of an own compression core (draft, not implemented)

Status: **design for review**. Nothing in this document is implemented yet; all speed and size
figures for STRATA are targets or estimates, all other figures are measurements.

STRATA = **str**eam-separated, **a**symmetric, **t**able-driven LZ with **a**ccurate parsing.
The data of a block is split into several "layers" (streams), each coded with static entropy
tables that the encoder optimizes. The decoder never adapts a model: it only looks up tables.

## 1. Why a new core?

Measurements on the extended test build (327.6 MB, 4-vCPU VM, release build of `d809ce1`):

| Archive | Size | Unpack, 1 thread |
|---|---:|---:|
| GPK (LZRC + TCM for text) | 125.54 MB | 29.7 s |
| GPK `--no-cm` (LZRC only) | 126.28 MB (+0.59 %) | 8.1 s |

- The text core TCM saves 0.74 MB but costs ~22 s of decoding CPU (1–1.5 MB/s per thread).
- LZRC (LZMA principle: adaptive binary range coder, one bit at a time) decodes 60–70 MB/s.
  It codes 225.8 MB of binary data (x86 code, BCn textures, meshes, audio) into 67.3 MB and
  takes ~4.5 s of the remaining 8.1 s; PNG/JPEG inverse transformations take ~3.5 s.
- Packing time matters much less than unpacking time (unpacking runs on every player's
  machine, packing once per release).

Bit-by-bit adaptive coding is the reason for both speed limits: every bit depends on the
previous one (serial dependency), and every bit updates a probability. An asymmetric design can
move all modelling work into the encoder.

## 2. Goals

Measured against LZRC on the same blocks (after the existing GPK filters):

| | Target | Stretch goal |
|---|---|---|
| Size, binary blocks | ≤ +2 % vs. LZRC | ≤ LZRC |
| Size, text blocks | ≤ LZRC (TCM stays available) | halfway between LZRC and TCM |
| Decoding, single thread | ≥ 4× LZRC (≥ 250 MB/s) | ≥ 8× |
| Encoding | ≤ 2× LZRC time | |
| Decoder memory | block + < 1 MB tables | |
| Safety | safe Rust only, every corrupted input → error, never a panic or overread | |
| Determinism | integers only, identical output on all platforms and thread counts | |

Non-goals: streaming with small windows, compression levels for speed, beating TCM on text.

## 3. Overview

```
            encoder (expensive)                                  decoder (cheap)
 block ─► match finder (BT, reused from LZRC) ─┐
          ▲                                     ▼
          │   optimal parse with EXACT static code lengths ◄─ tables of the previous pass
          │                                     │
          └──── 2–3 passes (parse → statistics → tables → parse) ──┘
                                                │
          block-level choices: literal context function, distance codebook, align bits
                                                │
          chunking: split points + table reuse decided by cost (DP)
                                                │
          streams: CMD | LIT | MLEN | OSLOT | ALIGN | DICT | RAW bits  ─► rANS ─►  block
```

A block (up to 64 MiB, as today) is one LZ window. It is coded as a sequence of
**commands** `(literal run, match)`; the fields of the commands go into separate streams, each
stream uses its own static rANS tables, and tables can change at chunk boundaries.

## 4. Command model

Each command = `litlen` literals followed by one match (or the end of the block).

**CMD symbol** (one per command, ≤ 112 symbols): `kind × 16 + ll_code`

- `kind` ∈ {`R0`, `R1`, `R2`, `R3`, `D`, `N`, `END`}: repeat offsets 0–3 (LRU as in LZMA),
  distance from the block's codebook (`D`, section 6), new offset (`N`), end of block.
- `ll_code` 0–15: literal run length, codes 0–7 direct, 8–15 with a base + extra bits
  (extra bits in RAW).

Coding kind and literal run jointly captures their correlation (e.g. long literal runs are
usually followed by new offsets, `R0` directly after a match is impossible and gets frequency 0).

**MLEN stream**: match length code 0–39 (base + extra bits, minimum match 2 for `R0..R3`,
3 for `D`/`N`), with **two tables**: after repeat/codebook matches and after new offsets.

**Offsets** (`N`): slot = position of the highest bit + the next 2 bits (4 slots per octave,
finer than LZMA's 2; up to 26 bits → ≤ 104 slots) in the OSLOT stream. The `a` lowest extra
bits (`a` = 0–4, chosen per block) are coded with the ALIGN table, all other extra bits raw.
Structured data has offsets that are multiples of 4, 8, 16 … – ALIGN learns that.

## 5. Literals

Literals are the expensive part of LZ coding; LZMA wins with contexts (previous byte, match
byte). STRATA gets contexts with static tables via an **encoder-chosen context function** and a
**context map**:

1. **Context function** (per block, the encoder tries the candidates on a sample and keeps the
   cheapest; ≤ 64 raw contexts):
   - `prev(k)`: top `k` bits of the previous byte (k = 0–6),
   - `col(s, k)`: top `k` bits of the byte `s` positions back (s = detected stride of the
     block, e.g. record size of vertex buffers; GPK already detects strides for its filters),
   - `phase(s)`: position modulo `s` (s ∈ {2, 4, 8, 16, 32}),
   - each optionally × "first literal after a match" (1 bit).
2. **Context map**: the raw contexts are clustered into ≤ 8 literal tables per chunk (greedy
   merging of the pair with the smallest increase of coded size; the decoder only needs a
   64-byte map).
3. **Sub-literals** (per chunk, chosen by cost): code `lit − out[pos − rep0]` instead of `lit`.
   After a match the "next byte of the old occurrence" is the best predictor of the current one;
   in structured data (records that differ in a few bytes) the difference is mostly 0. This
   replaces LZMA's matched-literal coding with a single table lookup.

Text gets `prev(6)` with first-literal bit and up to 16 tables – roughly an order-1 model, as
LZRC uses today (`lc`).

## 6. Distance codebook (own idea for game data)

Repeat offsets only remember the last 4 distances. Game data often alternates between more
characteristic distances: vertex fields at multiples of the record size, the row pitch of a
texture (the "pixel above"), the size of a BCn block row, table rows in level data. The encoder
builds a histogram of the match distances of the first pass and stores up to **16 distances**
in the block header (varint, ~30 bytes). Kind `D` + one DICT symbol selects one of them. The
parse decides per match whether `D`, a repeat or a new offset is cheaper (exact costs, see 7).
After a `D` or `N` match the distance enters the repeat list as usual.

## 7. Encoder: parsing with exact costs

Static tables have a property that adaptive coders lack: **the cost of every symbol is known
exactly** before parsing (−log2(freq / 2^12) bits). The parse can therefore optimize for the
real coder:

1. Pass 1: LZRC's binary-tree match finder (reused) and its optimal-parse DP (reused, with the
   node state: position, repeat offsets, last kind) – with cost estimates from generic
   start tables.
2. Collect the statistics of all streams, choose the literal context function, codebook, align
   bits, build tables per chunk.
3. Pass 2 (and 3): repeat the DP with the exact code lengths of these tables. Stops when the
   size gain is < 0.1 %.

Chunking: the command sequence is split into chunks (64 KiB – 4 MiB of output); each stream
can keep the table of the previous chunk (1 flag) or send a new one. The split points and
the reuse decisions come from a DP over candidate boundaries every 64 KiB, with the table
header size included in the cost. This adapts to changing data (code → resources inside one
block) without the overhead of fixed small chunks.

Expected encoder time: 2–3 parses ≈ 2–3× the LZRC parse; the match finder runs once and its
match lists are reused (memory: ~16 bytes per position for a 64 MiB block → spill the lists
of large blocks or rerun the finder; to be decided in phase 1).

## 8. Entropy coder

- **rANS**, 32-bit state, 16-bit renormalization (reads one `u16`), probability precision
  11 or 12 bits (decided by measurement: smaller tables fit better into the L1 cache).
- Decoding table per symbol table: 2^11–2^12 entries of `u32` (symbol, frequency, offset
  packed) → one lookup + one multiply per symbol, no division, no branch except renorm.
- Two interleaved states for the literal stream (consecutive literals alternate), one per
  other stream; the streams are independent, so the CPU overlaps them.
- Table transmission: normalized frequencies, coded relative to the previous table of the
  same stream (most frequencies change little) with an Elias-gamma-like code; validated in the
  decoder (sum = 2^precision, symbols within the alphabet).
- Raw extra bits: a separate forward bit stream (64-bit buffer).

## 9. Block format

```
block   = u8 version | varint raw_size | params | chunk*
params  = u8 lit_ctx_fn | u8 stride | u8 align_bits | u8 n_dict | varint dict[n_dict]
chunk   = varint n_commands | u8 lit_mode (raw / sub) | u8 ctx_map[64] (if changed)
          | tables (per stream: reuse flag or new table) | varint stream_len[7]
          | CMD | LIT | MLEN | OSLOT | ALIGN | DICT | RAW
```

The decoder checks every length, every table and every copy against the output size and the
window before using it.

## 10. Decoder loop

```
for each chunk:
    load/keep tables, set stream readers
    repeat n_commands times:
        cmd       = CMD.decode()
        kind, ll  = cmd >> 4, ll_base[cmd & 15] + RAW.bits(ll_extra[cmd & 15])
        for i in 0..ll:                                   // literals
            t     = ctx_map[ctx_fn(out, pos, after_match)]
            b     = LIT[i & 1].decode(table[t])
            out[pos] = if sub { b + out[pos - rep0] } else { b }; pos += 1
        if kind == END: break
        dist = match kind { R0..R3 => reps[kind], D => dict[DICT.decode()],
                            N => offset from OSLOT + RAW + ALIGN }
        update reps; len = MLEN[kind_class].decode() + extra
        copy(out, pos - dist, len)   // 16-byte steps when dist >= 16, byte loop otherwise
```

Per literal: context computation (2–3 ALU ops), one table lookup for the map, one rANS
decode. Per command: 3–4 rANS decodes, a few raw bits, one copy. Compared with LZRC (~9 serial
adaptive bit decodes per literal) this is the source of the expected 4–8× speed-up.

## 11. Integration into GPK

- New codec id `CODEC_STRATA = 4`; archives that use it need the new unpacker (older versions
  reject the block cleanly). The existing filters (x86-split, split, shuffle, word-pred …)
  stay in front of the core unchanged.
- **Codec choice per block** by cost: `size + λ × decoding time`. With λ = 0 the smallest
  block wins (today's behaviour); with λ > 0 a fast codec wins if it is only slightly larger.
  λ becomes an option (`--unpack-weight`) with a default to be decided (section 14).
- LZRC stays as a fallback and for comparison; TCM stays for text when λ allows it.
- No C reference: tests are Rust-only (round trip, corrupted input, determinism across thread
  counts, Linux ↔ Windows under Wine as for the other codecs).

## 12. What is borrowed, what is new

Borrowed (well known): LZ77 with repeat offsets and optimal parsing (LZMA), separate streams
for sequence fields (zstd, Oodle), rANS (Duda), context maps for literal tables (Brotli),
sub-literals (Oodle Kraken, as far as public descriptions go).

Own contributions in this combination:
- parse **iterated with exact static code lengths** of the final tables, including the
  context-mapped literal costs and the table header costs of the chunk split;
- **distance codebook** per block for the characteristic distances of game data;
- **encoder-chosen literal context function** with stride-aware contexts (`col`, `phase`),
  using the stride detection GPK already has;
- joint `kind × literal-run` command symbol with impossible combinations at frequency 0;
- cost-based codec choice with an explicit exchange rate between bytes and unpacking time.

## 13. Risks and go/no-go

| Risk | Consequence | Check |
|---|---|---|
| Static tables lose against adaptive coding on non-stationary data | larger than LZRC | phase 1: measure on the 4 LZRC blocks of the test build |
| Table headers too expensive | +1–3 % on small chunks | chunk DP with header costs; delta coding of tables |
| Literal contexts too weak for text | text larger than LZRC | λ decides; TCM/LZRC remain |
| Match lists of 64 MiB blocks too large for several passes | encoder RAM | rerun the match finder per pass or limit the stored candidates |

**Go/no-go after phase 1**: if a prototype (pass 1 + exact-cost pass 2, without chunk DP and
codebook) is more than 5 % larger than LZRC on the binary blocks, the static approach is not
pursued; the fallback would be keeping LZRC and only adding the fast decoder ideas that apply
to it.

## 14. Plan

1. **Prototype & measurement** (go/no-go): streams, rANS, one table set per block, pass 1+2,
   `prev(k)` contexts. Measure size and decoding speed on the LZRC blocks and the text blocks.
2. **Full model**: chunking DP, context functions and map, sub-literals, codebook, align bits.
   Measure the effect of each part separately (ablation table in this document).
3. **Hardening**: format validation, corrupted-input tests, determinism, fuzz loop.
4. **Integration**: codec id, cost-based codec choice with λ, CLI option, docs, CI.
5. **Final measurement**: whole test build, 1/4 threads, simulated 8/16 threads, comparison
   with xz/zstd in `docs/comparison.md`.

Open decisions:
- **λ default** – how many bytes is one second of unpacking (single core) worth? Example from
  the table in section 1: TCM costs ~30 s per MB saved (22 s for 0.74 MB).
- Should STRATA eventually replace LZRC completely (simpler code, one archive format), or stay
  an additional codec?
