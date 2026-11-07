//! CourierQR receiver for the browser.
//!
//! Deliberately **no wasm-bindgen**: the module exposes a handful of plain
//! `extern "C"` functions over integers and pointers, so building it needs
//! only `cargo` and the JS glue (`web/courierqr.js`) is short enough to
//! read in one sitting. One module instance = one receiver.
//!
//! ```text
//! JS: camera frame ─► grayscale ─► cq_scan(ptr, w, h)
//!       Rust: find QR codes ─► raw bytes ─► CRC ─► RaptorQ ─► (complete?)
//! JS: cq_open_passphrase(...) ─► Rust: age decrypt ─► unpack ─► name + bytes
//! ```
//!
//! Strings and buffers cross the boundary as (pointer, length) pairs in
//! wasm linear memory, allocated with `cq_alloc` / released with `cq_free`.

use std::cell::RefCell;

use courierqr_core::{Accepted, Error, FrameError, OpenKey, ReceivedFile, Receiver};

pub mod scan;

// ---------------------------------------------------------------- state

#[derive(Default)]
struct State {
    rx: Receiver,
    file: Option<ReceivedFile>,
    last_error: String,
    /// Codes found / newly accepted during the last `cq_scan`.
    last_found: u32,
    last_new: u32,
    /// Running totals for the UI.
    total_codes: u32,
    total_rejected: u32,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

// ---------------------------------------------------------------- memory

/// Allocate `len` bytes for JS to write into. Pair with [`cq_free`].
#[unsafe(no_mangle)]
pub extern "C" fn cq_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Release a buffer from [`cq_alloc`].
///
/// # Safety
/// `ptr`/`len` must come from a single `cq_alloc(len)` call and not be
/// freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
    }
}

unsafe fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

// ---------------------------------------------------------------- receiving

/// Status codes returned by `cq_push` / `cq_scan`. Negative = frame dropped.
pub mod status {
    pub const PROGRESS: i32 = 0;
    pub const DUPLICATE: i32 = 1;
    pub const OTHER_TRANSFER: i32 = 2;
    pub const COMPLETE: i32 = 3;
    pub const ALREADY_COMPLETE: i32 = 4;
    /// `cq_scan` only: no CourierQR code in this camera frame.
    pub const NOTHING: i32 = 5;
    pub const NOT_COURIERQR: i32 = -1;
    pub const UNSUPPORTED_VERSION: i32 = -2;
    pub const TRUNCATED: i32 = -3;
    pub const CHECKSUM: i32 = -4;
    pub const BAD_PARAMETERS: i32 = -5;
}

fn code(r: Result<Accepted, FrameError>) -> i32 {
    use status::*;
    match r {
        Ok(Accepted::Progress) => PROGRESS,
        Ok(Accepted::Duplicate) => DUPLICATE,
        Ok(Accepted::OtherTransfer) => OTHER_TRANSFER,
        Ok(Accepted::Complete) => COMPLETE,
        Ok(Accepted::AlreadyComplete) => ALREADY_COMPLETE,
        Err(FrameError::NotCourierQr) => NOT_COURIERQR,
        Err(FrameError::UnsupportedVersion(_)) => UNSUPPORTED_VERSION,
        Err(FrameError::Truncated) => TRUNCATED,
        Err(FrameError::Checksum) => CHECKSUM,
        Err(FrameError::BadParameters) => BAD_PARAMETERS,
    }
}

/// Feed one already-decoded QR payload (for decoders living outside wasm).
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_push(ptr: *const u8, len: usize) -> i32 {
    let bytes = unsafe { slice(ptr, len) };
    with(|s| {
        let c = code(s.rx.push(bytes));
        s.total_codes += 1;
        if c < 0 {
            s.total_rejected += 1;
        }
        c
    })
}

/// Find every QR code in an 8-bit grayscale image (`w*h` bytes, row-major)
/// and feed CourierQR frames to the receiver. Returns the most significant
/// status: COMPLETE > PROGRESS > DUPLICATE/OTHER > NOTHING.
///
/// # Safety
/// `ptr` must point to `w*h` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_scan(ptr: *const u8, w: u32, h: u32) -> i32 {
    let pixels = unsafe { slice(ptr, (w as usize) * (h as usize)) };
    let payloads = scan::decode_all(pixels, w, h);
    with(|s| {
        s.last_found = payloads.len() as u32;
        s.last_new = 0;
        let mut best = status::NOTHING;
        for p in &payloads {
            let c = code(s.rx.push(p));
            s.total_codes += 1;
            match c {
                status::COMPLETE => best = status::COMPLETE,
                status::PROGRESS => {
                    s.last_new += 1;
                    if best != status::COMPLETE {
                        best = status::PROGRESS;
                    }
                }
                c if c < 0 => s.total_rejected += 1,
                c => {
                    if best == status::NOTHING {
                        best = c;
                    }
                }
            }
        }
        best
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_received() -> u32 {
    with(|s| s.rx.progress().received as u32)
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_needed() -> u32 {
    with(|s| s.rx.progress().needed as u32)
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_is_complete() -> u32 {
    with(|s| s.rx.is_complete() as u32)
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_last_found() -> u32 {
    with(|s| s.last_found)
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_last_new() -> u32 {
    with(|s| s.last_new)
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_total_codes() -> u32 {
    with(|s| s.total_codes)
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_total_rejected() -> u32 {
    with(|s| s.total_rejected)
}

/// Forget everything (including any decrypted file) and start over.
#[unsafe(no_mangle)]
pub extern "C" fn cq_reset() {
    with(|s| *s = State::default());
}

// ---------------------------------------------------------------- opening

/// Result codes for `cq_open_*`.
pub mod open_status {
    pub const OK: i32 = 0;
    pub const NOT_COMPLETE: i32 = -1;
    /// Wrong passphrase/key, or tampered data.
    pub const DECRYPT: i32 = -2;
    pub const BAD_KEY: i32 = -3;
    pub const MALFORMED: i32 = -4;
}

fn open_with(key: OpenKey) -> i32 {
    with(|s| {
        let Some(res) = s.rx.open(&key) else {
            s.last_error = "transfer is not complete yet".into();
            return open_status::NOT_COMPLETE;
        };
        match res {
            Ok(file) => {
                s.file = Some(file);
                s.last_error.clear();
                open_status::OK
            }
            Err(e) => {
                s.last_error = e.to_string();
                match e {
                    Error::Decrypt(_) => open_status::DECRYPT,
                    Error::BadKey(_) => open_status::BAD_KEY,
                    _ => open_status::MALFORMED,
                }
            }
        }
    })
}

/// Decrypt the completed transfer with a UTF-8 passphrase.
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_open_passphrase(ptr: *const u8, len: usize) -> i32 {
    match std::str::from_utf8(unsafe { slice(ptr, len) }) {
        Ok(p) => open_with(OpenKey::passphrase(p)),
        Err(_) => {
            with(|s| s.last_error = "passphrase is not valid UTF-8".into());
            open_status::BAD_KEY
        }
    }
}

/// Decrypt with an `AGE-SECRET-KEY-1...` identity (comment lines ignored).
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_open_identity(ptr: *const u8, len: usize) -> i32 {
    let text = String::from_utf8_lossy(unsafe { slice(ptr, len) }).into_owned();
    match text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
    {
        Some(k) => open_with(OpenKey::identity(k)),
        None => {
            with(|s| s.last_error = "no AGE-SECRET-KEY line found".into());
            open_status::BAD_KEY
        }
    }
}

/// Pointer/length of the opened file's name, data, or the last error.
/// Valid until the next call that changes state.
#[unsafe(no_mangle)]
pub extern "C" fn cq_file_name_ptr() -> *const u8 {
    with(|s| {
        s.file
            .as_ref()
            .map_or(std::ptr::null(), |f| f.name.as_ptr())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_file_name_len() -> usize {
    with(|s| s.file.as_ref().map_or(0, |f| f.name.len()))
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_file_data_ptr() -> *const u8 {
    with(|s| {
        s.file
            .as_ref()
            .map_or(std::ptr::null(), |f| f.data.as_ptr())
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_file_data_len() -> usize {
    with(|s| s.file.as_ref().map_or(0, |f| f.data.len()))
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_error_ptr() -> *const u8 {
    with(|s| s.last_error.as_ptr())
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_error_len() -> usize {
    with(|s| s.last_error.len())
}

/// Wipe the decrypted file from wasm memory (call after saving it).
#[unsafe(no_mangle)]
pub extern "C" fn cq_forget_file() {
    with(|s| {
        if let Some(mut f) = s.file.take() {
            f.data.fill(0);
        }
    });
}

// ---------------------------------------------------------------- randomness

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod rng {
    #[link(wasm_import_module = "env")]
    unsafe extern "C" {
        /// Provided by the page: fills `len` bytes via crypto.getRandomValues.
        fn host_random(ptr: *mut u8, len: usize);
    }

    fn fill(buf: &mut [u8]) -> Result<(), getrandom::Error> {
        unsafe { host_random(buf.as_mut_ptr(), buf.len()) };
        Ok(())
    }

    getrandom::register_custom_getrandom!(fill);
}
