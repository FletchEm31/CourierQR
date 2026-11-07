use thiserror::Error;

/// Everything that can go wrong preparing or opening a transfer.
#[derive(Debug, Error)]
pub enum Error {
    #[error("file name is too long ({0} bytes, max {max})", max = crate::MAX_NAME_BYTES)]
    NameTooLong(usize),

    #[error("transfer is too large ({0} bytes, max {max})", max = crate::MAX_TRANSFER_BYTES)]
    TooLarge(usize),

    #[error("frame budget of {0} bytes is too small (min {min})", min = crate::frame::MIN_FRAME_BYTES)]
    FrameBudgetTooSmall(usize),

    #[error("invalid recipient or identity key: {0}")]
    BadKey(String),

    #[error("encryption failed: {0}")]
    Encrypt(#[from] age::EncryptError),

    /// Wrong passphrase/key, or the ciphertext was tampered with.
    #[error("decryption failed (wrong key, or data was altered): {0}")]
    Decrypt(#[from] age::DecryptError),

    #[error("decrypted payload is malformed: {0}")]
    BadPayload(&'static str),

    #[error("decompression failed or exceeded the size limit")]
    Decompress,
}

pub type Result<T> = std::result::Result<T, Error>;
