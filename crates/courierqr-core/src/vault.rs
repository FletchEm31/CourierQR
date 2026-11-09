//! At-rest protection for a device's secret identity (and, later, a local
//! message store).
//!
//! Today the secret keys live in a `0600` file in plaintext: good against
//! another user on the same machine, useless if the disk, a backup, or the
//! whole laptop is taken. A **vault** encrypts those bytes so the file alone
//! is worthless without one of:
//!
//! * a **recovery code** — a random ~125-bit code shown once at setup
//!   ([`recovery_code`]); or
//! * a **YubiKey** — handled separately through age's plugin system
//!   (`age-plugin-yubikey`), where the key never leaves the token.
//!
//! This module is the recovery-code path and the on-disk format both share:
//! an ordinary age file, so [`is_locked`] can tell a vault from a legacy
//! plaintext identity file and migrate it.

use crate::Result;

/// scrypt cost for the vault. The code already carries ~125 bits, so this is
/// belt-and-suspenders against a weak *typed* passphrase, not the main
/// defence; 2^17 ≈ 0.5–1 s to unlock, paid once per session.
const WORK_FACTOR: u8 = 17;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"; // Crockford: no I L O U
const GROUPS: usize = 5;
const PER_GROUP: usize = 5;

/// A fresh recovery code: 25 Crockford base32 characters (~125 bits),
/// grouped `XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`. Show it **once**. If the code is
/// the only thing protecting the vault, losing it means losing the keys.
pub fn recovery_code() -> String {
    // 256 = 8 × 32, so a byte mod 32 is a uniform base32 symbol (no bias).
    let mut bytes = [0u8; GROUPS * PER_GROUP];
    getrandom::getrandom(&mut bytes).expect("system randomness unavailable");
    let chars: Vec<char> = bytes
        .iter()
        .map(|b| ALPHABET[(b & 31) as usize] as char)
        .collect();
    chars
        .chunks(PER_GROUP)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// Normalize a typed code so near-misses still unlock: drop spaces/dashes,
/// uppercase, and fold the characters Crockford leaves out of the alphabet
/// onto their look-alikes (`O`→`0`, `I`/`L`→`1`). The generated code never
/// contains those, so this only ever *helps* a human who mistyped.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}

/// Encrypt `secret` under `code`. The result is an age file (binary).
pub fn lock(secret: &[u8], code: &str) -> Result<Vec<u8>> {
    let mut r = age::scrypt::Recipient::new(normalize(code).into());
    r.set_work_factor(WORK_FACTOR);
    Ok(age::encrypt(&r, secret)?)
}

/// Decrypt a vault under `code`. Wrong code → [`Error::Decrypt`].
pub fn unlock(blob: &[u8], code: &str) -> Result<Vec<u8>> {
    let mut id = age::scrypt::Identity::new(normalize(code).into());
    // Cap the work factor a hostile file can demand (matches the rest of CourierQR).
    id.set_max_work_factor(crate::MAX_SCRYPT_WORK_FACTOR);
    Ok(age::decrypt(&id, blob)?)
}

/// Does this look like a vault (an age file) rather than a plaintext
/// identity? Lets callers keep reading legacy files and offer to migrate.
pub fn is_locked(bytes: &[u8]) -> bool {
    bytes.starts_with(b"age-encryption.org/v1\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[test]
    fn code_shape_and_entropy() {
        let c = recovery_code();
        assert_eq!(c.len(), GROUPS * PER_GROUP + (GROUPS - 1)); // 25 + 4 dashes
        assert_eq!(c.chars().filter(|&c| c == '-').count(), 4);
        let body: String = c.chars().filter(|c| *c != '-').collect();
        assert!(body.bytes().all(|b| ALPHABET.contains(&b)));
        assert_ne!(recovery_code(), recovery_code());
    }

    #[test]
    fn round_trip_and_wrong_code() {
        let secret = b"AGE-SECRET-KEY-1EXAMPLE\nAGE-SECRET-KEY-PQ-1EXAMPLE";
        let code = recovery_code();
        let blob = lock(secret, &code).unwrap();
        assert!(is_locked(&blob));
        assert_eq!(unlock(&blob, &code).unwrap(), secret);

        let err = unlock(&blob, &recovery_code()).unwrap_err();
        assert!(matches!(err, Error::Decrypt(_)));
    }

    #[test]
    fn forgiving_about_how_the_code_is_typed() {
        let secret = b"keys";
        let code = recovery_code();
        let blob = lock(secret, &code).unwrap();
        // Lowercase, spaces instead of dashes, and O/I/L for 0/1 all unlock.
        let messy = code.to_lowercase().replace('-', " ");
        assert_eq!(unlock(&blob, &messy).unwrap(), secret);
        let confused = code.replace('0', "O").replace('1', "l");
        assert_eq!(unlock(&blob, &confused).unwrap(), secret);
    }

    #[test]
    fn plaintext_identity_is_not_mistaken_for_a_vault() {
        assert!(!is_locked(b"# CourierQR identity\nAGE-SECRET-KEY-1ABC"));
        assert!(!is_locked(b"AGE-SECRET-KEY-1ABC"));
    }
}
