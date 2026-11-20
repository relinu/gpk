//! GPK – verlustfreier Download-Packer für Spiel-Builds.
//!
//! Module (jeweils Port der C-Referenz in `legacy-c/`, formatgleich):
//! - [`filters`]: umkehrbare Vorfilter (Delta, Shuffle, x86, Feld-Split, Wort-Prädiktion)
//! - [`glz`]: schneller LZ+Huffman-Kompressor (Schätzer für "bereits komprimiert")
//! - [`lzrc`]: Hauptkern, LZ77 + optimales Parsing + Range Coder (LZMA-Prinzip)
//! - [`tcm`]: Context-Mixing-Kern für Text
//! - [`pxc`]: verlustfreier Pixel-Coder
//! - [`pngx`]: bitgenaue PNG-Rekompression
//! - [`jpgx`]: bitgenaue JPEG-Rekompression
//! - [`archive`]: Archivformat, Packen, Entpacken
#![forbid(unsafe_op_in_unsafe_fn)]
// Stilfragen: Indexschleifen spiegeln die Struktur der Referenz (Differenztests gegen C);
// `x % n == 0` statt `is_multiple_of` hält ältere Rust-Versionen kompatibel.
#![allow(clippy::needless_range_loop, clippy::manual_is_multiple_of, clippy::should_implement_trait)]

pub mod archive;
pub mod filters;
pub mod glz;
pub mod jpgx;
pub mod lzrc;
pub mod pngx;
pub mod pxc;
pub mod tcm;
pub mod zlib;

#[cfg(feature = "cref")]
pub mod cref;

#[doc(hidden)]
pub mod testutil;
