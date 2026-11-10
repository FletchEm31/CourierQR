//! # courierqr-core
//!
//! The transport-agnostic heart of CourierQR. It knows nothing about
//! screens, cameras or QR rendering — it turns a file into an endless
//! stream of small, self-describing byte frames, and turns any large
//! enough subset of those frames (any order, gaps, duplicates) back into
//! the file.
//!
//! ```text
//! file ─► pack (name + DEFLATE) ─► age encrypt ─► RaptorQ ─► frames ─► [QR / screen]
//!                                                                         │ camera
//! file ◄─ unpack ◄─ age decrypt ◄─ RaptorQ decode ◄─ CRC check ◄─ frames ◄┘
//! ```
//!
//! Compression happens *before* encryption: ciphertext is random-looking
//! and can't be compressed.
//!
//! ```
//! use courierqr_core::{FrameConfig, OpenKey, Receiver, SealKey, Transmission};
//!
//! let mut seal = SealKey::passphrase("correct horse battery staple");
//! # if let SealKey::Passphrase { work_factor, .. } = &mut seal { *work_factor = 10; }
//! let tx = Transmission::new("note.md", b"# hello", &seal, FrameConfig::default()).unwrap();
//!
//! let mut rx = Receiver::new();
//! for frame in tx.frames() {
//!     if rx.push(&frame) == Ok(courierqr_core::Accepted::Complete) {
//!         break;
//!     }
//! }
//! let file = rx.open(&OpenKey::passphrase("correct horse battery staple")).unwrap().unwrap();
//! assert_eq!(file.data, b"# hello");
//! ```

pub mod account;
mod crypto;
mod error;
pub mod feed;
pub mod frame;
mod payload;
pub mod pq;
mod receiver;
#[cfg(feature = "scan")]
pub mod scan;
pub mod scrub;
mod sender;
pub mod transport;
pub mod vault;

pub use account::{Account, AccountNumber};
pub use crypto::{
    DEFAULT_SCRYPT_WORK_FACTOR, DeviceKeys, MAX_SCRYPT_WORK_FACTOR, Method, OpenKey, SealKey,
    fingerprint, generate_device_keys, generate_pq_identity, public_key_of, split_key_set,
    validate_public_key,
};
pub use error::{Error, Result};
pub use frame::FrameError;
pub use payload::ReceivedFile;
pub use receiver::{Accepted, Progress, Receiver, open};
pub use scrub::{ScrubReport, scrub};
pub use sender::{FrameConfig, FrameStream, Transmission, qr_capacity};
pub use transport::Envelope;
pub use vault::recovery_code;

/// Re-exported so callers can build keys without depending on `age` directly.
pub use age::secrecy;

/// Largest file name carried in the encrypted payload.
pub const MAX_NAME_BYTES: usize = 255;

/// Largest transfer (plaintext payload and ciphertext). Optical links run
/// at tens of KB/s, so this is ~20 minutes of screen time — and keeps the
/// receiver's memory bounded on a phone.
pub const MAX_TRANSFER_BYTES: usize = 64 * 1024 * 1024;
