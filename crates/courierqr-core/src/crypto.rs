//! Encryption via the [age](https://age-encryption.org) format.
//!
//! We deliberately don't invent our own envelope: age gives us chunked
//! ChaCha20-Poly1305 with a reviewed header format, and two key modes that
//! map well onto CourierQR:
//!
//! * **Passphrase** (scrypt): type the same passphrase on both devices.
//! * **Paired keys** (X25519): each receiving device generates an identity
//!   once and shows its *public* key to the sender. Displaying a public key
//!   on screen is harmless, so pairing needs no secret channel.

use std::str::FromStr;

use age::secrecy::{ExposeSecret, SecretString};

use crate::{Error, Result};

/// scrypt cost (log2 N) used for passphrase mode unless overridden.
///
/// 16 => 64 MiB of RAM per attempt. age's own default auto-calibrates to
/// ~1s on the *encrypting* machine (often 18 = 256 MiB), which can be
/// painful in a phone browser. With a strong passphrase 16 is plenty.
pub const DEFAULT_SCRYPT_WORK_FACTOR: u8 = 16;

/// Highest scrypt cost a receiver will attempt. Caps how much CPU/RAM a
/// hostile header can make the receiver burn.
pub const MAX_SCRYPT_WORK_FACTOR: u8 = 20;

/// How the sender locks the file.
#[derive(Clone)]
pub enum SealKey {
    Passphrase {
        passphrase: SecretString,
        work_factor: u8,
    },
    /// An `age1...` X25519 public key from a paired receiver.
    Recipient(String),
}

impl SealKey {
    pub fn passphrase(passphrase: impl Into<String>) -> Self {
        SealKey::Passphrase {
            passphrase: SecretString::from(passphrase.into()),
            work_factor: DEFAULT_SCRYPT_WORK_FACTOR,
        }
    }

    pub fn recipient(public_key: impl Into<String>) -> Self {
        SealKey::Recipient(public_key.into())
    }
}

/// How the receiver unlocks the file.
#[derive(Clone)]
pub enum OpenKey {
    Passphrase(SecretString),
    /// An `AGE-SECRET-KEY-1...` identity kept on the receiving device.
    Identity(SecretString),
}

impl OpenKey {
    pub fn passphrase(passphrase: impl Into<String>) -> Self {
        OpenKey::Passphrase(SecretString::from(passphrase.into()))
    }

    pub fn identity(secret_key: impl Into<String>) -> Self {
        OpenKey::Identity(SecretString::from(secret_key.into()))
    }
}

/// A freshly generated device keypair for paired mode.
pub struct DeviceKeys {
    /// `AGE-SECRET-KEY-1...` — never leaves the receiving device.
    pub secret: SecretString,
    /// `age1...` — safe to display as a QR code for the sender to scan.
    pub public: String,
}

/// Generate an X25519 identity for a receiving device.
pub fn generate_device_keys() -> DeviceKeys {
    let identity = age::x25519::Identity::generate();
    DeviceKeys {
        public: identity.to_public().to_string(),
        secret: identity.to_string(),
    }
}

pub(crate) fn seal(key: &SealKey, plaintext: &[u8]) -> Result<Vec<u8>> {
    match key {
        SealKey::Passphrase {
            passphrase,
            work_factor,
        } => {
            let mut r = age::scrypt::Recipient::new(passphrase.clone());
            r.set_work_factor(*work_factor);
            Ok(age::encrypt(&r, plaintext)?)
        }
        SealKey::Recipient(pk) => {
            let r = age::x25519::Recipient::from_str(pk.trim())
                .map_err(|e| Error::BadKey(e.to_string()))?;
            Ok(age::encrypt(&r, plaintext)?)
        }
    }
}

pub(crate) fn open(key: &OpenKey, ciphertext: &[u8]) -> Result<Vec<u8>> {
    match key {
        OpenKey::Passphrase(passphrase) => {
            let mut id = age::scrypt::Identity::new(passphrase.clone());
            id.set_max_work_factor(MAX_SCRYPT_WORK_FACTOR);
            Ok(age::decrypt(&id, ciphertext)?)
        }
        OpenKey::Identity(sk) => {
            let id = age::x25519::Identity::from_str(sk.expose_secret().trim())
                .map_err(|e| Error::BadKey(e.to_string()))?;
            Ok(age::decrypt(&id, ciphertext)?)
        }
    }
}

/// The `age1...` public key belonging to an `AGE-SECRET-KEY-1...` identity.
pub fn public_key_of(secret_key: &str) -> Result<String> {
    let id = age::x25519::Identity::from_str(secret_key.trim())
        .map_err(|e| Error::BadKey(e.to_string()))?;
    Ok(id.to_public().to_string())
}

/// Check that `public_key` is a valid `age1...` X25519 recipient (bech32
/// checksum included, so most typos are caught) and return it normalized.
pub fn validate_public_key(public_key: &str) -> Result<String> {
    let pk = public_key.trim().to_ascii_lowercase();
    age::x25519::Recipient::from_str(&pk).map_err(|e| Error::BadKey(e.to_string()))?;
    Ok(pk)
}

/// Short, human-comparable fingerprint of a public key, e.g. `K7QM-2XRA-9DPW`.
///
/// Pairing moves the public key over whatever channel is handy (scan, copy,
/// email to yourself). That channel doesn't need to be secret, but it must
/// not be able to *swap* the key — so both devices show this fingerprint and
/// you compare them by eye. 60 bits of SHA-256: far beyond what anyone can
/// grind a look-alike key for.
pub fn fingerprint(public_key: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"; // Crockford: no I L O U
    let pk = validate_public_key(public_key)?;
    let digest = Sha256::new()
        .chain_update(b"courierqr-fingerprint-v1:")
        .chain_update(pk.as_bytes())
        .finalize();
    let bits = u64::from_be_bytes(digest[..8].try_into().unwrap()) >> 4; // top 60 bits
    let chars: Vec<u8> = (0..12)
        .rev()
        .map(|i| ALPHABET[((bits >> (i * 5)) & 31) as usize])
        .collect();
    let s = std::str::from_utf8(&chars).unwrap();
    Ok(format!("{}-{}-{}", &s[0..4], &s[4..8], &s[8..12]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_case_insensitive() {
        let keys = generate_device_keys();
        let a = fingerprint(&keys.public).unwrap();
        assert_eq!(a, fingerprint(&keys.public.to_uppercase()).unwrap());
        assert_eq!(a, fingerprint(&format!("  {}\n", keys.public)).unwrap());
        assert_eq!(a.len(), 14);
        assert!(
            a.chars()
                .all(|c| c == '-' || "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c))
        );
        assert_ne!(a, fingerprint(&generate_device_keys().public).unwrap());
    }

    #[test]
    fn typos_are_caught_by_the_checksum() {
        let pk = generate_device_keys().public;
        let mut typo = pk.clone().into_bytes();
        let i = 20;
        typo[i] = if typo[i] == b'q' { b'p' } else { b'q' };
        assert!(validate_public_key(std::str::from_utf8(&typo).unwrap()).is_err());
        assert!(validate_public_key("age1notakey").is_err());
        assert!(validate_public_key(&pk).is_ok());
    }

    #[test]
    fn known_vector() {
        // Pin the format so phone and desktop can never silently disagree.
        let pk = "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p";
        assert_eq!(fingerprint(pk).unwrap(), "8SP0-RP4E-HSG6");
    }
}
