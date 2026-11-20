# Project conventions

## Language

- **Chat with the user in German.**
- **Everything in the repository is written in English**: source code (identifiers, comments,
  string literals, log and error messages, CLI output), documentation, CI configuration,
  scripts, test names, and commit messages. This also applies to the C reference in `legacy-c/`.

## Project overview

GPK is a lossless download packer for game builds (Rust, `src/`). The former C implementation in
`legacy-c/` is kept as a reference: its kernels (filters, GLZ, LZRC, TCM, PXC, PNGX, JPGX) are
compared byte-for-byte with the Rust ports in `tests/cref_diff.rs` (feature `cref`).

- Build: `cargo build --release` (pure Rust, no C code in the default build)
- Tests: `cargo test --release --features cref,xz` (needs a C compiler for the reference)
- Smoke test: `python3 ci/smoke_test.py target/release/gpk`
- Windows: cross-compiled with `--target x86_64-pc-windows-gnu` and tested under Wine in CI;
  never use a Windows VM in GitHub Actions.
- Library code must not use `unsafe` (`#![forbid(unsafe_code)]` outside feature `cref`).
- `src/deflate.rs` must stay bit-exact with zlib 1.4 (PNG recompression depends on it).
