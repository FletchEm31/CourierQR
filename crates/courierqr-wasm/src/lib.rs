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

use courierqr_core::secrecy::ExposeSecret;
use courierqr_core::{
    Accepted, Error, FrameConfig, FrameError, FrameStream, OpenKey, ReceivedFile, Receiver,
    SealKey, Transmission,
};

pub use courierqr_core::scan;

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
    /// Freshly generated device keypair (secret, public), until forgotten.
    keys: Option<(String, String)>,
    /// Output of `cq_fingerprint`.
    text: String,
    /// Output of `cq_qr_encode` / `cq_tx_next_qr`: width*width modules, 1 = dark.
    qr: Vec<u8>,
    /// Output of `cq_scan_first`.
    raw: Vec<u8>,
    /// Active outgoing transfer and the QR version its frames are sized for.
    tx: Option<(FrameStream<Transmission>, i16)>,
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

// ---------------------------------------------------------------- pairing

/// Generate an X25519 device keypair (randomness from `host_random`).
/// Read it with `cq_key_*`, store the secret, then call `cq_forget_keys`.
#[unsafe(no_mangle)]
pub extern "C" fn cq_generate_keys() {
    let k = courierqr_core::generate_device_keys();
    with(|s| s.keys = Some((k.secret.expose_secret().to_owned(), k.public)));
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_key_secret_ptr() -> *const u8 {
    with(|s| s.keys.as_ref().map_or(std::ptr::null(), |k| k.0.as_ptr()))
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_key_secret_len() -> usize {
    with(|s| s.keys.as_ref().map_or(0, |k| k.0.len()))
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_key_public_ptr() -> *const u8 {
    with(|s| s.keys.as_ref().map_or(std::ptr::null(), |k| k.1.as_ptr()))
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_key_public_len() -> usize {
    with(|s| s.keys.as_ref().map_or(0, |k| k.1.len()))
}

/// Wipe the generated keypair from wasm memory.
#[unsafe(no_mangle)]
pub extern "C" fn cq_forget_keys() {
    with(|s| {
        if let Some((secret, _)) = s.keys.take() {
            let mut b = secret.into_bytes();
            b.fill(0);
        }
    });
}

/// Fingerprint of an `age1...` public key into the text buffer
/// (`cq_text_ptr/len`). Returns 0, or `open_status::BAD_KEY` if invalid.
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_fingerprint(ptr: *const u8, len: usize) -> i32 {
    let pk = String::from_utf8_lossy(unsafe { slice(ptr, len) }).into_owned();
    with(|s| match courierqr_core::fingerprint(&pk) {
        Ok(fp) => {
            s.text = fp;
            0
        }
        Err(e) => {
            s.last_error = e.to_string();
            open_status::BAD_KEY
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_text_ptr() -> *const u8 {
    with(|s| s.text.as_ptr())
}
#[unsafe(no_mangle)]
pub extern "C" fn cq_text_len() -> usize {
    with(|s| s.text.len())
}

/// Encode bytes as a QR code (ECC M) in **pure byte mode** at the smallest
/// version that fits. Byte mode matters: "optimal" encoders switch to
/// numeric mode for digit runs, and byte-only readers (like ours) would
/// silently drop them — an `age1…2872…` key came back without its `2872`.
/// Returns the width in modules (matrix is width² bytes at `cq_qr_ptr`,
/// 1 = dark), or -1 if it doesn't fit any version.
///
/// # Safety
/// `ptr` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_qr_encode(ptr: *const u8, len: usize) -> i32 {
    use qrcode::{EcLevel, QrCode, Version, bits::Bits};
    let data = unsafe { slice(ptr, len) };
    let code = (1..=40).find_map(|v| {
        let mut bits = Bits::new(Version::Normal(v));
        bits.push_byte_data(data).ok()?;
        bits.push_terminator(EcLevel::M).ok()?;
        QrCode::with_bits(bits, EcLevel::M).ok()
    });
    match code {
        Some(code) => with(|s| {
            s.qr = code
                .to_colors()
                .iter()
                .map(|c| (*c == qrcode::Color::Dark) as u8)
                .collect();
            code.width() as i32
        }),
        None => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_qr_ptr() -> *const u8 {
    with(|s| s.qr.as_ptr())
}

// ---------------------------------------------------------------- sending

/// Byte capacity of a byte-mode QR code of `version` at ECC L (0 if invalid).
#[unsafe(no_mangle)]
pub extern "C" fn cq_qr_capacity(version: i32) -> usize {
    qr_capacity(version as i16)
}

fn qr_capacity(version: i16) -> usize {
    use qrcode::{EcLevel, Version, bits::Bits};
    if !(1..=40).contains(&version) {
        return 0;
    }
    let fits = |n: usize| {
        let mut bits = Bits::new(Version::Normal(version));
        bits.push_byte_data(&vec![0u8; n]).is_ok() && bits.push_terminator(EcLevel::L).is_ok()
    };
    let (mut lo, mut hi) = (0usize, 3000usize);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) { lo = mid } else { hi = mid - 1 }
    }
    lo
}

/// Start sending a file. `mode` 0 = passphrase (`key` is the passphrase),
/// 1 = paired device (`key` is its `age1...` public key). Frames are sized
/// for byte-mode QR codes of `version` at ECC L. Returns 0, or a negative
/// code with the reason in `cq_error_*`.
///
/// # Safety
/// Each (ptr, len) pair must point to readable memory of that length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_tx_start(
    name_ptr: *const u8,
    name_len: usize,
    data_ptr: *const u8,
    data_len: usize,
    key_ptr: *const u8,
    key_len: usize,
    mode: u32,
    version: i32,
) -> i32 {
    let name = String::from_utf8_lossy(unsafe { slice(name_ptr, name_len) }).into_owned();
    let data = unsafe { slice(data_ptr, data_len) };
    let key = String::from_utf8_lossy(unsafe { slice(key_ptr, key_len) }).into_owned();
    let cap = qr_capacity(version as i16);
    let seal = match mode {
        0 => SealKey::passphrase(key),
        _ => SealKey::recipient(key),
    };
    match Transmission::new(
        &name,
        data,
        &seal,
        FrameConfig {
            max_frame_bytes: cap,
        },
    ) {
        Ok(tx) => with(|s| {
            s.tx = Some((tx.into_frames(), version as i16));
            0
        }),
        Err(e) => with(|s| {
            s.last_error = e.to_string();
            -1
        }),
    }
}

/// One-line summary of the metadata removed before encryption, into the
/// text buffer (`cq_text_ptr/len`). Returns its length (0 if not sending).
#[unsafe(no_mangle)]
pub extern "C" fn cq_tx_report() -> usize {
    with(|s| {
        s.text =
            s.tx.as_ref()
                .map(|(f, _)| f.transmission().scrub_report().summary())
                .unwrap_or_default();
        s.text.len()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_tx_source_frames() -> u32 {
    with(|s| {
        s.tx.as_ref()
            .map_or(0, |(f, _)| f.transmission().source_frames() as u32)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_tx_ciphertext_len() -> u32 {
    with(|s| {
        s.tx.as_ref()
            .map_or(0, |(f, _)| f.transmission().ciphertext_len() as u32)
    })
}

/// Encode the next frame as a QR code (fixed version, ECC L, byte mode).
/// Returns its width (matrix at `cq_qr_ptr`), or -1 if no transfer is active.
#[unsafe(no_mangle)]
pub extern "C" fn cq_tx_next_qr() -> i32 {
    use qrcode::{EcLevel, QrCode, Version, bits::Bits};
    with(|s| {
        let Some((stream, version)) = s.tx.as_mut() else {
            return -1;
        };
        let frame = stream.next().expect("frame stream never ends");
        let mut bits = Bits::new(Version::Normal(*version));
        let code = bits
            .push_byte_data(&frame)
            .and_then(|_| bits.push_terminator(EcLevel::L))
            .and_then(|_| QrCode::with_bits(bits, EcLevel::L));
        match code {
            Ok(code) => {
                s.qr = code
                    .to_colors()
                    .iter()
                    .map(|c| (*c == qrcode::Color::Dark) as u8)
                    .collect();
                code.width() as i32
            }
            Err(_) => -1,
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_tx_stop() {
    with(|s| s.tx = None);
}

/// Decode the first QR code in a grayscale frame, whatever it holds (used
/// to scan a computer's public key). Returns its length (bytes at
/// `cq_raw_ptr`), or 0 if none was found.
///
/// # Safety
/// `ptr` must point to `w*h` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cq_scan_first(ptr: *const u8, w: u32, h: u32) -> usize {
    let pixels = unsafe { slice(ptr, (w as usize) * (h as usize)) };
    let first = scan::decode_all(pixels, w, h)
        .into_iter()
        .next()
        .unwrap_or_default();
    with(|s| {
        s.raw = first;
        s.raw.len()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cq_raw_ptr() -> *const u8 {
    with(|s| s.raw.as_ptr())
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
