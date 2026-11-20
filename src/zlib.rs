//! Thin wrapper around the real zlib (C, statically built via `libz-sys`) – only as a reference
//! for tests.
//!
//! The program itself uses its own bit-identical implementation (`deflate`, `inflate`);
//! the differential tests compare both. Only compiled with feature `cref` (FFI, `unsafe`).

use libz_sys as z;
use std::ptr::null_mut;

unsafe extern "C" fn zalloc(_opaque: z::voidpf, items: z::uInt, size: z::uInt) -> z::voidpf {
    // SAFETY: standard allocator; zlib returns the memory via zfree.
    unsafe { libc::calloc(items as usize, size as usize) }
}
unsafe extern "C" fn zfree(_opaque: z::voidpf, p: z::voidpf) {
    // SAFETY: p comes from zalloc (calloc).
    unsafe { libc::free(p) }
}

fn new_stream() -> z::z_stream {
    z::z_stream {
        next_in: null_mut(),
        avail_in: 0,
        total_in: 0,
        next_out: null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: null_mut(),
        state: null_mut(),
        zalloc,
        zfree,
        opaque: null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

const STREAM_SIZE: i32 = std::mem::size_of::<z::z_stream>() as i32;

/// Deflate with exactly these zlib parameters (window bits 15, zlib header).
pub fn deflate_with(raw: &[u8], level: i32, mem_level: i32, strategy: i32) -> Option<Vec<u8>> {
    if raw.len() > u32::MAX as usize / 2 {
        return None;
    }
    let mut s = new_stream();
    // SAFETY: s is fully initialized; zlib manages its state via `state`.
    unsafe {
        if z::deflateInit2_(&mut s, level, z::Z_DEFLATED, 15, mem_level, strategy, z::zlibVersion(), STREAM_SIZE) != z::Z_OK {
            return None;
        }
        let cap = z::deflateBound(&mut s, raw.len() as z::uLong) as usize + 64;
        let mut out = vec![0u8; cap];
        s.next_in = raw.as_ptr() as *mut u8;
        s.avail_in = raw.len() as z::uInt;
        s.next_out = out.as_mut_ptr();
        s.avail_out = cap as z::uInt;
        let r = z::deflate(&mut s, z::Z_FINISH);
        let n = s.total_out as usize;
        z::deflateEnd(&mut s);
        if r != z::Z_STREAM_END || n > cap {
            return None;
        }
        out.truncate(n);
        Some(out)
    }
}

/// Inflate completely; `None` if the stream is invalid, not all input bytes are consumed,
/// or the output exceeds `max_out`.
pub fn inflate_all(zs: &[u8], max_out: usize) -> Option<Vec<u8>> {
    if zs.len() > u32::MAX as usize / 2 {
        return None;
    }
    let mut s = new_stream();
    let mut cap = zs.len().saturating_mul(4).saturating_add(65536).min(max_out.max(1));
    let mut out = vec![0u8; cap];
    // SAFETY: pointers point into live buffers; lengths match avail_in/avail_out.
    unsafe {
        if z::inflateInit_(&mut s, z::zlibVersion(), STREAM_SIZE) != z::Z_OK {
            return None;
        }
        s.next_in = zs.as_ptr() as *mut u8;
        s.avail_in = zs.len() as z::uInt;
        let mut r;
        loop {
            let done = s.total_out as usize;
            if done == cap {
                if cap >= max_out {
                    z::inflateEnd(&mut s);
                    return None;
                }
                cap = (cap * 2).min(max_out);
                out.resize(cap, 0);
            }
            let done = s.total_out as usize;
            s.next_out = out.as_mut_ptr().add(done);
            s.avail_out = (cap - done).min(u32::MAX as usize) as z::uInt;
            r = z::inflate(&mut s, z::Z_NO_FLUSH);
            if r != z::Z_OK {
                break;
            }
        }
        let n = s.total_out as usize;
        let consumed_all = s.total_in as usize == zs.len();
        z::inflateEnd(&mut s);
        if r != z::Z_STREAM_END || !consumed_all {
            return None;
        }
        out.truncate(n);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deflate_inflate_roundtrip() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8 ^ (i / 1000) as u8).collect();
        for &lvl in &[1, 6, 9] {
            let c = deflate_with(&data, lvl, 8, 0).unwrap();
            assert_eq!(inflate_all(&c, 1 << 20).unwrap(), data);
        }
        assert!(inflate_all(&[1, 2, 3, 4], 1 << 20).is_none());
    }
}
