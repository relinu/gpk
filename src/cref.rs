//! FFI zur C-Referenz (nur mit Feature `cref`, für Differenztests).
#![allow(dead_code)]
use std::os::raw::c_int;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CFspec {
    pub id: u8,
    pub stride: u8,
    pub nfields: u8,
    pub fields: [u8; 6],
    pub skip: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CLzrcParams {
    pub lc: c_int,
    pub lp: c_int,
    pub pb: c_int,
    pub depth: c_int,
    pub nice: c_int,
}

extern "C" {
    pub fn filter_encode(f: *const CFspec, inp: *const u8, out: *mut u8, n: usize, off: u64);
    pub fn filter_decode(f: *const CFspec, inp: *mut u8, out: *mut u8, n: usize, off: u64);
    pub fn glz_bound(n: usize) -> usize;
    pub fn glz_compress(src: *const u8, n: usize, dst: *mut u8, cap: usize, level: c_int, flags: u32) -> usize;
    pub fn glz_decompress(src: *const u8, n: usize, dst: *mut u8, cap: usize) -> usize;
    pub fn lzrc_default_params(p: *mut CLzrcParams, level: c_int);
    pub fn lzrc_bound(n: usize) -> usize;
    pub fn lzrc_compress(src: *const u8, n: usize, dst: *mut u8, cap: usize, p: *const CLzrcParams) -> usize;
    pub fn lzrc_decompress(src: *const u8, n: usize, dst: *mut u8, rawsize: usize) -> c_int;
    pub fn tcm_bound(n: usize) -> usize;
    pub fn tcm_compress(src: *const u8, n: usize, dst: *mut u8, cap: usize) -> usize;
    pub fn tcm_decompress(src: *const u8, n: usize, dst: *mut u8, rawsize: usize) -> c_int;
    pub fn pxc_encode(px: *const u8, w: u32, h: u32, ch: c_int, outn: *mut usize) -> *mut u8;
    pub fn pxc_decode(src: *const u8, n: usize, px: *mut u8, w: u32, h: u32, ch: c_int) -> c_int;
    pub fn pngx_forward_mode(png: *const u8, n: usize, mode: c_int, tsize: *mut usize) -> *mut u8;
    pub fn pngx_inverse(t: *const u8, tn: usize, out: *mut u8, origsize: usize) -> c_int;
    pub fn pngx_channels(png: *const u8, n: usize) -> c_int;
    pub fn jpgx_forward(jpg: *const u8, n: usize, tsize: *mut usize) -> *mut u8;
    pub fn jpgx_inverse(t: *const u8, tn: usize, out: *mut u8, origsize: usize) -> c_int;
}

/// Von C mit malloc erzeugten Puffer als Vec übernehmen (kopiert, gibt frei).
pub fn take(p: *mut u8, n: usize) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: p zeigt auf n gültige Bytes aus malloc der C-Referenz.
    let v = unsafe { std::slice::from_raw_parts(p, n).to_vec() };
    unsafe { libc::free(p as *mut libc::c_void) };
    Some(v)
}
