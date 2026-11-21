# Compression for computer games

**GPK** packs a complete game build for download: deduplication, file-type-specific filters
(x86 code, DDS textures, meshes, audio), lossless PNG and JPEG recompression, its own cores
LZRC (LZ) and TCM (context mixing for text), and parallel unpacking with CRC checks.
Details: [docs/GPK.md](docs/GPK.md).

The program is written in **Rust** (`src/`). The former C version is kept as a reference in
`legacy-c/` (including GLZ, the runtime asset compressor, see [docs/GLZ.md](docs/GLZ.md)).
The Rust and C versions use the same archive format and unpack each other's archives; the
kernels (filters, LZRC, TCM, PNG, JPEG) are byte-identical, which CI checks on every push.

## Key result (test build 327.6 MB incl. JPEGs)

| Method | Size after | Difference | Pack | Unpack (4 threads) |
|---|---:|---:|---:|---:|
| zip -9 | 187.2 MB | −42.9 % | 40.0 s | 3.1 s |
| tar + zstd -19 --long=27 | 162.8 MB | −50.3 % | 191.8 s | 0.9 s |
| tar + xz -9e | 149.2 MB | −54.5 % | 311.1 s | 7.3 s |
| GPK v3 `--no-cm` | 126.3 MB | −61.5 % | 100.6 s | 4.1 s |
| **GPK v3** | **125.5 MB** | **−61.7 %** | 118.5 s | 9.9 s |

Details, ablations and trade-offs: [docs/GPK.md](docs/GPK.md) and
[docs/comparison.md](docs/comparison.md). The test build is partly synthetic (code, scripts and
fonts are real files).

## Rust version: port and optimization (test build 308 MB, 4 threads)

| | Rust port (archives byte-identical to C) | Rust optimized |
|---|---:|---:|
| Archive | 120.27 MB | 120.28 MB (+0.01 %) |
| Pack | 92.8 s, 1.46 GB RAM | **64.3 s**, 1.67 GB RAM |
| Unpack | 8.3 s, 0.44 GB RAM | **6.8 s**, 0.61 GB RAM |

Both columns were measured back to back. The plain port was as fast as the C version
(in an earlier measurement packing took 104.7 s vs. 104.3 s).
What was optimized and what did not help: [docs/GPK.md](docs/GPK.md#speed-rust-version).

**Unpacking** was optimized further (format unchanged, kernels still byte-identical to C):
text core −13–15 % instructions per bit, LZRC decoder +10–20 %, PNG/JPEG inverse ~5–15 %,
and smaller text/transformation blocks for more parallelism. On the extended test build
(327.6 MB): 4 threads 10.3 → **8.75 s**, longest block 5.6 → 2.4 s (relevant from 8 threads),
archive +0.04 %. Details: [docs/GPK.md](docs/GPK.md#unpacking).

Planned: NOMP, an own compression core with the highest ratio that still unpacks as fast as
the archive downloads; design draft in [docs/NOMP.md](docs/NOMP.md), requirements in
[docs/NOMP_SPEC.md](docs/NOMP_SPEC.md) (not implemented yet).

## Quick start

```bash
cargo build --release
python3 tools/make_build.py testbuild/MyGame        # create a test build (~330 MB, JPEGs need Pillow)
target/release/gpk c -t 4 testbuild/MyGame game.gpk
target/release/gpk x -t 4 game.gpk unpacked
target/release/gpk l game.gpk
```

Requirements: only Rust (stable). The default build contains no C code; its only dependency is
`crc32fast` (pure Rust). Optionally `--features xz` for the comparison core `--xz` (liblzma, needs
a C compiler).
Options: `gpk` without arguments shows the help; `--max-ram GB` limits the memory usage.

## Security

- All code is safe Rust: `unsafe` is forbidden in the library via `#![forbid(unsafe_code)]`
  (the only exception is the test feature `cref`, which links the C reference and the real
  zlib for comparison). PNG recompression uses its own Deflate implementation that produces
  bit-exactly the same streams as zlib 1.4 (see below).
- Archives are fully validated before unpacking: paths (no absolute paths, no `..`, on Windows
  no `\` or `:`), sizes and bounds of all blocks and pieces. Corrupted data leads to an error
  message, never to a crash or to writing outside the target directory; every file is checked
  with CRC32.
- Absolute symlinks are skipped when unpacking.

## Own algorithms instead of libraries

| Library | replaced by | Result |
|---|---|---|
| zlib (C) for PNG recompression | `src/deflate.rs`, `src/inflate.rs`: Deflate bit-exact with zlib 1.4 (levels 1–9, memLevel 1–9, strategies default/filtered), own inflater | as fast as zlib (Deflate 97–103 %, Inflate 90–106 %); the parameter search compares continuously and stops at the first differing block: **PNG search 1.9–3.9× faster**; Windows .exe 18 % smaller |
| liblzma (C) for `--xz` | LZRC (own core, already existed) | only optional now (`--features xz`); LZRC packs smaller and faster |
| crc32fast (Rust) | – | kept: uses the CPU's carry-less multiplication instructions, 12 GB/s vs. 2 GB/s for the best safe own implementation (slicing-by-16) |

Bit-exactness is checked in CI against the real zlib: 702 parameter combinations on text,
random, image, zero and structured data (including window slides, stored blocks and distant
matches), plus 8100 corrupted streams that the inflater must accept or reject exactly like
zlib. The archives are byte-identical to the version that used zlib.

## Tests

```bash
cargo test --release                       # unit tests (round trips, corrupted data)
cargo test --release --features cref,xz    # plus differential tests: Rust == C, byte for byte
python3 ci/smoke_test.py target/release/gpk
```

The differential tests compile the C modules from `legacy-c/` and compare every stage
(filters, GLZ, LZRC, TCM, pixel coder, PNG, JPEG, Deflate/Inflate) with the Rust version.
More test images can be added with `GPK_PNG_DIR`/`GPK_JPEG_DIR`
(`tools/gen_testimages.py` creates some, needs Pillow).
`GPK_TIMINGS=1 gpk …` prints phase timings when packing and per-block timings when unpacking.

## Windows

`gpk.exe` is cross-compiled on Linux (no Windows VM needed):

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt-get install gcc-mingw-w64-x86-64
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release --target x86_64-pc-windows-gnu
```

Compared to the C version, the Rust `.exe` has the same features as on Linux and supports
Unicode file names. Archives can be exchanged between Linux and Windows; on Windows symlinks
are skipped and Unix permissions are not set.

## CI

`.github/workflows/build.yml` runs on every push to `main` and `rust-port` and on pull
requests (documentation-only changes are skipped), exclusively on Ubuntu runners (no Windows
VM). Three jobs run in parallel; a full run takes about 1.5–2 minutes:

- **Linux tests:** unit tests and differential tests against the C reference and the real zlib
  (with a fast-compiling optimized profile `ci`)
- **Linux build:** `cargo fmt`/`clippy`, check "no C code in the default build", optional xz
  core, tests in debug mode (overflow checks), smoke test (pack, unpack, compare bit-exactly, a
  corrupted archive must be rejected), identical archives with 1 and 4 threads, cross-unpacking
  C ↔ Rust
- **Windows .exe:** cross build, unit and smoke tests of the `.exe` under Wine, archive exchange
  Linux → Windows and Windows → Linux
- The finished programs (`gpk`, `gpk.exe`) are available as artifacts of each run.

Speed and robustness: per-commit Rust caches (restored from the latest one), packages are
installed from the runner's package lists (refreshed only on failure, without recommended
packages, 5-minute timeout), and a new push cancels the still running run of the same branch.

## Releases

Finished programs appear automatically under *Releases* once all builds and tests are green:

```bash
git tag v0.4.0 && git push origin v0.4.0     # creates the release and attaches the files
```

Alternatively, publish a release in the GitHub UI – the pipeline then attaches the files to that
release. Attached are `gpk-<version>-linux-x86_64.tar.gz`, `gpk-<version>-windows-x86_64.zip`
and `SHA256SUMS.txt`.
