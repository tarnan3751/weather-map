//! The WXT1 tile decoder (`wx_tile::decode`) behind a minimal C ABI, so the
//! browser can run it as WebAssembly with `WebAssembly.instantiate` and a few
//! lines of TypeScript, no generated glue.
//!
//! Calling sequence, one tile at a time (the module is single-threaded):
//!
//! ```text
//! ptr = wx_alloc(len)          // copy the tile's bytes to memory[ptr..ptr+len]
//! status = wx_decode(ptr, len) // 0, or a negative error code
//! wx_free(ptr, len)
//! wx_width(), wx_height(), wx_bands()
//! wx_samples()                 // u16 samples, band after band, rows north to south
//! wx_release()                 // drop the decoded tile
//! ```
//!
//! Malformed input must never trap: decoding errors become status codes, and
//! headers that would need an absurd allocation are refused before any is
//! attempted (a failed allocation would abort, which in WebAssembly is a trap).

use std::alloc::{Layout, alloc, dealloc};
use std::cell::RefCell;

/// Status codes returned by [`wx_decode`].
pub const OK: i32 = 0;
pub const ERR_TRUNCATED: i32 = -1;
pub const ERR_BAD_MAGIC: i32 = -2;
pub const ERR_UNSUPPORTED: i32 = -3;
pub const ERR_BAD_HEADER: i32 = -4;
pub const ERR_CORRUPT: i32 = -5;
pub const ERR_TOO_LARGE: i32 = -6;
pub const ERR_NULL: i32 = -7;

/// Largest decoded tile accepted, in bytes of samples. Real tiles are
/// 257 x 257 x up to 8 bands, about 1 MiB; this allows twice the resolution.
/// WebAssembly memory never shrinks, so a hostile tile near the limit leaves
/// each worker a few times this much bigger, not gigabytes.
pub const MAX_DECODED_BYTES: u64 = 8 * 1024 * 1024;

struct Decoded {
    width: u32,
    height: u32,
    bands: u32,
    samples: Vec<u16>,
}

thread_local! {
    // WebAssembly here has one thread; the decoded tile lives until the next
    // decode or `wx_release`.
    static LAST: RefCell<Option<Decoded>> = const { RefCell::new(None) };
}

/// Refuse headers whose decoded size is out of bounds, using 64-bit
/// arithmetic (on wasm32 `usize` products can overflow).
fn check_size(bytes: &[u8]) -> Result<(), i32> {
    if bytes.len() < wx_tile::HEADER_LEN {
        return Err(ERR_TRUNCATED);
    }
    let width = u16::from_le_bytes([bytes[4], bytes[5]]) as u64;
    let height = u16::from_le_bytes([bytes[6], bytes[7]]) as u64;
    let bands = bytes[8] as u64;
    if width * height * bands * 2 > MAX_DECODED_BYTES {
        return Err(ERR_TOO_LARGE);
    }
    Ok(())
}

fn status_of(e: &wx_tile::TileError) -> i32 {
    match e {
        wx_tile::TileError::Truncated => ERR_TRUNCATED,
        wx_tile::TileError::BadMagic => ERR_BAD_MAGIC,
        wx_tile::TileError::Unsupported(..) => ERR_UNSUPPORTED,
        wx_tile::TileError::BadHeader(_) => ERR_BAD_HEADER,
        wx_tile::TileError::Corrupt => ERR_CORRUPT,
    }
}

/// Decode `bytes` and keep the result for the accessors. Safe Rust; the
/// exported function only turns a pointer into this slice.
pub fn decode_into_last(bytes: &[u8]) -> i32 {
    LAST.with(|l| *l.borrow_mut() = None);
    // Check the magic first so garbage reports as such, not as "too large".
    if bytes.len() >= 4 && bytes[0..4] != wx_tile::MAGIC {
        return ERR_BAD_MAGIC;
    }
    if let Err(code) = check_size(bytes) {
        return code;
    }
    match wx_tile::decode(bytes) {
        Ok(tile) => {
            let samples: Vec<u16> = tile.bands.concat();
            let decoded = Decoded {
                width: tile.width as u32,
                height: tile.height as u32,
                bands: tile.bands.len() as u32,
                samples,
            };
            LAST.with(|l| *l.borrow_mut() = Some(decoded));
            OK
        }
        Err(e) => status_of(&e),
    }
}

/// Allocate `len` bytes for the caller to fill with a tile. Returns null when
/// `len` is zero or the allocation fails (never aborts).
#[unsafe(no_mangle)]
pub extern "C" fn wx_alloc(len: usize) -> *mut u8 {
    let Ok(layout) = Layout::from_size_align(len, 1) else {
        return std::ptr::null_mut();
    };
    if layout.size() == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the layout has a non-zero size, which is all `alloc` requires.
    // A null result is passed back to the caller, who checks it.
    unsafe { alloc(layout) }
}

/// Free a buffer from [`wx_alloc`].
///
/// # Safety
/// `ptr` must come from `wx_alloc(len)` with the same `len`, and not have been
/// freed already. A null `ptr` is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wx_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len, 1) else {
        return;
    };
    // SAFETY: by this function's contract `ptr` was allocated by `wx_alloc`
    // with exactly this layout and is freed once.
    unsafe { dealloc(ptr, layout) }
}

/// Decode the `len` bytes at `ptr`. Returns [`OK`] or a negative status.
///
/// # Safety
/// `ptr` must point to `len` readable bytes (normally a buffer from
/// [`wx_alloc`]) that stay valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wx_decode(ptr: *const u8, len: usize) -> i32 {
    if ptr.is_null() {
        // No bytes at all is a (truncated) tile; a length without bytes is not.
        return if len == 0 {
            decode_into_last(&[])
        } else {
            ERR_NULL
        };
    }
    // SAFETY: the caller guarantees `len` readable bytes at `ptr` for this
    // call; the slice does not outlive it.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    decode_into_last(bytes)
}

fn with_last<T>(f: impl FnOnce(&Decoded) -> T, default: T) -> T {
    LAST.with(|l| l.borrow().as_ref().map(f).unwrap_or(default))
}

/// Samples per row of the last decoded tile (0 if none).
#[unsafe(no_mangle)]
pub extern "C" fn wx_width() -> u32 {
    with_last(|d| d.width, 0)
}

/// Rows of the last decoded tile (0 if none).
#[unsafe(no_mangle)]
pub extern "C" fn wx_height() -> u32 {
    with_last(|d| d.height, 0)
}

/// Bands of the last decoded tile (0 if none).
#[unsafe(no_mangle)]
pub extern "C" fn wx_bands() -> u32 {
    with_last(|d| d.bands, 0)
}

/// Pointer to the last decoded tile's samples, `bands * width * height`
/// `u16`s, band after band. Null if none. Valid until the next call to
/// `wx_decode` or `wx_release`.
#[unsafe(no_mangle)]
pub extern "C" fn wx_samples() -> *const u16 {
    with_last(|d| d.samples.as_ptr(), std::ptr::null())
}

/// Drop the last decoded tile.
#[unsafe(no_mangle)]
pub extern "C" fn wx_release() {
    LAST.with(|l| *l.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(bands: usize, w: u16, h: u16) -> Vec<u8> {
        let n = w as usize * h as usize;
        let data = (0..bands)
            .map(|b| (0..n).map(|i| ((i * 7 + b * 100) % 65536) as u16).collect())
            .collect();
        wx_tile::encode(&wx_tile::Tile::new(w, h, data))
    }

    #[test]
    fn decodes_through_the_c_abi() {
        let bytes = tile(3, 257, 257);
        let ptr = wx_alloc(bytes.len());
        assert!(!ptr.is_null());
        // SAFETY (test): `ptr` has `bytes.len()` writable bytes from wx_alloc.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
        // SAFETY (test): the buffer holds `bytes.len()` initialised bytes.
        assert_eq!(unsafe { wx_decode(ptr, bytes.len()) }, OK);
        // SAFETY (test): allocated above with this length.
        unsafe { wx_free(ptr, bytes.len()) };
        assert_eq!((wx_width(), wx_height(), wx_bands()), (257, 257, 3));
        let n = 257 * 257;
        // SAFETY (test): a successful decode holds bands * n samples.
        let s = unsafe { std::slice::from_raw_parts(wx_samples(), 3 * n) };
        assert_eq!(s[n + 10], 10 * 7 + 100);
        wx_release();
        assert!(wx_samples().is_null());
    }

    #[test]
    fn reports_errors_instead_of_trapping() {
        assert_eq!(decode_into_last(b"WXT"), ERR_TRUNCATED);
        assert_eq!(decode_into_last(&[0u8; 32]), ERR_BAD_MAGIC);
        let mut cut = tile(1, 8, 8);
        cut.truncate(cut.len() - 3);
        assert_eq!(decode_into_last(&cut), ERR_CORRUPT);
        let mut bad = tile(1, 8, 8);
        bad[12] = 3;
        assert_eq!(decode_into_last(&bad), ERR_BAD_HEADER);
        // A header claiming 65535 x 65535 x 8 is refused before allocating.
        let mut huge = tile(1, 8, 8);
        huge[4..8].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        huge[8] = 8;
        assert_eq!(decode_into_last(&huge), ERR_TOO_LARGE);
        // SAFETY (test): a null pointer is checked before use.
        assert_eq!(unsafe { wx_decode(std::ptr::null(), 10) }, ERR_NULL);
        // SAFETY (test): a null pointer with no length reads nothing.
        assert_eq!(unsafe { wx_decode(std::ptr::null(), 0) }, ERR_TRUNCATED);
        assert!(wx_alloc(0).is_null());
        assert_eq!(wx_width(), 0);
    }

    #[test]
    fn survives_garbage() {
        let mut seed = 0x2545_f491_u32;
        for len in 0..600 {
            let mut v = vec![0u8; len];
            for b in v.iter_mut() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *b = seed as u8;
            }
            if len >= 4 && len % 3 == 0 {
                v[..4].copy_from_slice(b"WXT1");
            }
            let _ = decode_into_last(&v);
        }
    }
}
