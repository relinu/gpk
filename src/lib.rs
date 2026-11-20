//! GPK – lossless download packer for game builds.
//!
//! Modules (each a port of the C reference in `legacy-c/`, same format):
//! - [`filters`]: reversible pre-filters (delta, shuffle, x86, field split, word prediction)
//! - [`glz`]: fast LZ+Huffman compressor (estimator for "already compressed")
//! - [`lzrc`]: main core, LZ77 + optimal parsing + range coder (LZMA principle)
//! - [`tcm`]: context-mixing core for text
//! - [`pxc`]: lossless pixel coder
//! - [`pngx`]: bit-exact PNG recompression
//! - [`deflate`] / [`inflate`]: own Deflate implementation, bit-exact with zlib 1.4
//! - [`jpgx`]: bit-exact JPEG recompression
//! - [`archive`]: archive format, packing, unpacking
//!
//! Without the test feature `cref` the library contains no `unsafe` at all (enforced by the
//! compiler) and no C code.
#![cfg_attr(not(feature = "cref"), forbid(unsafe_code))]
#![forbid(unsafe_op_in_unsafe_fn)]
// Style: index loops mirror the structure of the reference (differential tests against C);
// `x % n == 0` instead of `is_multiple_of` keeps older Rust versions compatible.
#![allow(clippy::needless_range_loop, clippy::manual_is_multiple_of, clippy::should_implement_trait)]

pub mod archive;
pub mod deflate;
pub mod filters;
pub mod glz;
pub mod inflate;
pub mod jpgx;
pub mod lzrc;
pub mod pngx;
pub mod pxc;
pub mod tcm;
/// Real zlib (C) as a reference, only for differential tests.
#[cfg(feature = "cref")]
pub mod zlib;

#[cfg(feature = "cref")]
pub mod cref;

#[doc(hidden)]
pub mod testutil;
