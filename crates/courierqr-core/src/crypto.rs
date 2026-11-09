//! Encryption via the [age](https://age-encryption.org) format.
//!
//! We deliberately don't invent our own envelope: age gives us chunked
//! ChaCha20-Poly1305 with a reviewed header format, and two key modes that
//! map well onto CourierQR:
//!
//! * **Passphrase** (scrypt): type the same passphrase on both devices.
//! * **Paired keys**: each device generates its identities once and shows
//!   its *public* keys to the sender. Displaying a public key on screen is
//!   harmless, so pairing needs no secret channel. A device has two:
//!   - post-quantum hybrid ML-KEM-768 + X25519 (`age1pq1…`, see [`crate::pq`])
//!   - classic X25519 (`age1…`), smaller and compatible with older pairings.
//!
//! A device's *key set* is its public keys separated by a space
//! (`age1… age1pq1…`); that string is what pairing moves and what the
//! fingerprint covers. The sender picks a [`Method`] per message.

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
    /// One public key from a paired receiver: `age1…` (X25519) or
    /// `age1pq1…` (post-quantum hybrid). See [`SealKey::for_device`].
    Recipient(String),
}

/// How a message to a paired device is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// ML-KEM-768 + X25519 hybrid: safe even against a future quantum
    /// computer recording the codes today. ~1.4 KB more per message.
    PostQuantum,
    /// X25519 only: smallest, but a large enough quantum computer could
    /// one day decrypt recordings.
    Classic,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::PostQuantum => "post-quantum (ML-KEM-768 + X25519)",
            Method::Classic => "classic (X25519)",
        }
    }
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

    /// Pick the key for `method` out of a device's key set.
    pub fn for_device(key_set: &str, method: Method) -> Result<Self> {
        let (classic, pq) = split_key_set(key_set)?;
        match method {
            Method::PostQuantum => pq.map(SealKey::Recipient).ok_or_else(|| {
                Error::BadKey(
                    "this device has no post-quantum key yet: pair it again from an updated app"
                        .into(),
                )
            }),
            Method::Classic => classic
                .map(SealKey::Recipient)
                .ok_or_else(|| Error::BadKey("this device has no classic key".into())),
        }
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

/// Freshly generated device keys for paired mode.
pub struct DeviceKeys {
    /// `AGE-SECRET-KEY-1…` and `AGE-SECRET-KEY-PQ-1…`, one per line. Never
    /// leaves the device.
    pub secret: SecretString,
    /// Key set `age1… age1pq1…`: safe to display for the sender to scan.
    pub public: String,
}

/// Generate a device's identities (classic and post-quantum), locally,
/// from the operating system's CSPRNG.
pub fn generate_device_keys() -> DeviceKeys {
    let classic = age::x25519::Identity::generate();
    let pq = crate::pq::HybridIdentity::generate();
    DeviceKeys {
        public: format!("{} {}", classic.to_public(), pq.to_public().to_bech32()),
        secret: format!(
            "{}\n{}",
            classic.to_string().expose_secret(),
            pq.to_bech32().expose_secret()
        )
        .into(),
    }
}

/// A post-quantum identity to add to a device that only has a classic one.
pub fn generate_pq_identity() -> SecretString {
    crate::pq::HybridIdentity::generate().to_bech32()
}

fn identities(secret: &str) -> Result<Vec<Box<dyn age::Identity>>> {
    let mut ids: Vec<Box<dyn age::Identity>> = Vec::new();
    for line in secret
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        if crate::pq::is_hybrid_identity(line) {
            ids.push(Box::new(
                crate::pq::HybridIdentity::parse(line).map_err(Error::BadKey)?,
            ));
        } else {
            ids.push(Box::new(
                age::x25519::Identity::from_str(line).map_err(|e| Error::BadKey(e.to_string()))?,
            ));
        }
    }
    if ids.is_empty() {
        return Err(Error::BadKey("no identity found".into()));
    }
    Ok(ids)
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
        // A whole key set: use its strongest key.
        SealKey::Recipient(set) if set.trim().contains(char::is_whitespace) => {
            let (classic, pq) = split_key_set(set)?;
            seal(
                &SealKey::Recipient(pq.or(classic).expect("non-empty set")),
                plaintext,
            )
        }
        SealKey::Recipient(pk) if crate::pq::is_hybrid_recipient(pk) => {
            let r = crate::pq::HybridRecipient::parse(&pk.trim().to_ascii_lowercase())
                .map_err(Error::BadKey)?;
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
            // Every identity the device holds (classic and post-quantum).
            let ids = identities(sk.expose_secret())?;
            let d = age::Decryptor::new(ciphertext)?;
            let mut r = d.decrypt(ids.iter().map(|i| i.as_ref()))?;
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut r, &mut out)
                .map_err(|e| Error::BadKey(format!("decrypting: {e}")))?;
            Ok(out)
        }
    }
}

/// The key set belonging to a device's secret identities (one per line).
pub fn public_key_of(secret_key: &str) -> Result<String> {
    let mut classic = None;
    let mut pq = None;
    for line in secret_key
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("AGE-SECRET-KEY-"))
    {
        if crate::pq::is_hybrid_identity(line) {
            pq = Some(
                crate::pq::HybridIdentity::parse(line)
                    .map_err(Error::BadKey)?
                    .to_public()
                    .to_bech32(),
            );
        } else {
            let id =
                age::x25519::Identity::from_str(line).map_err(|e| Error::BadKey(e.to_string()))?;
            classic = Some(id.to_public().to_string());
        }
    }
    let set: Vec<String> = classic.into_iter().chain(pq).collect();
    if set.is_empty() {
        return Err(Error::BadKey("no identity found".into()));
    }
    Ok(set.join(" "))
}

/// Split a key set into its (classic, post-quantum) keys, validating each.
pub fn split_key_set(key_set: &str) -> Result<(Option<String>, Option<String>)> {
    let (mut classic, mut pq) = (None, None);
    for k in key_set.split_whitespace().map(str::to_ascii_lowercase) {
        if crate::pq::is_hybrid_recipient(&k) {
            crate::pq::HybridRecipient::parse(&k).map_err(Error::BadKey)?;
            if pq.replace(k).is_some() {
                return Err(Error::BadKey("two post-quantum keys in one key set".into()));
            }
        } else {
            age::x25519::Recipient::from_str(&k).map_err(|e| Error::BadKey(e.to_string()))?;
            if classic.replace(k).is_some() {
                return Err(Error::BadKey("two classic keys in one key set".into()));
            }
        }
    }
    if classic.is_none() && pq.is_none() {
        return Err(Error::BadKey("no public key found".into()));
    }
    Ok((classic, pq))
}

/// Check a public key or key set (`age1…`, `age1pq1…`, or both separated by
/// a space; bech32 checksums catch most typos) and return it normalized:
/// lowercase, classic first. A single classic key normalizes to itself, so
/// existing pairings and fingerprints are unchanged.
pub fn validate_public_key(public_key: &str) -> Result<String> {
    let (classic, pq) = split_key_set(public_key)?;
    Ok(classic.into_iter().chain(pq).collect::<Vec<_>>().join(" "))
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
