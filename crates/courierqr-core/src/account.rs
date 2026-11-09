//! Account numbers and account-derived device keys (optional).
//!
//! By default each device makes its own independent random identity
//! ([`crate::generate_device_keys`]): if a device is lost, its key is gone and
//! you re-pair a new one. That is the most compartmentalized setup, and it
//! stays available.
//!
//! An **account number** is the alternative some people want: one anonymous
//! master secret per person (like a crypto-wallet seed, or a Mullvad number),
//! from which each of their devices derives its *own distinct* key. Benefits:
//!
//! * **Recovery** — lose a device, install CourierQR on a new one, enter the
//!   same account number and the same device id, and the identical keys come
//!   back, so everyone you paired with still recognizes you.
//! * **Per-device keys** — each device mixes in a memorable label (e.g.
//!   "desktop", "iphone"), so its key differs from your other devices and can
//!   be revoked on its own. Restoring a lost device needs only the account
//!   number and that label — nothing else to have backed up.
//! * **Anonymous** — the number is random, tied to no name, email or phone.
//!
//! Derivation (all local, no network):
//! ```text
//! device seed = HKDF-SHA256(
//!     ikm  = account seed (20 bytes),
//!     salt = "courierqr-account-v1",
//!     info = "device" ‖ 0x00 ‖ label,
//! ) → 64 bytes
//!   classic X25519 identity ← device seed[0..32]
//!   post-quantum X-Wing seed ← device seed[32..64]
//! ```
//! The account seed is the only long-term secret; keep it off the devices.

use age::secrecy::SecretString;
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::pq::{HybridIdentity, bech32};
use crate::{DeviceKeys, Error, Result};

const SEED_LEN: usize = 20; // 160 bits
const CHARS: usize = 32; // 20 bytes → 32 Crockford base32 symbols (exact)
const GROUP: usize = 4;
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"; // Crockford: no I L O U

/// An anonymous master account secret. Display it with [`Account::number`];
/// never store it on a device unless the user opts to.
pub struct Account {
    seed: Zeroizing<[u8; SEED_LEN]>,
}

impl Account {
    /// A fresh random account.
    pub fn generate() -> Self {
        let mut seed = Zeroizing::new([0u8; SEED_LEN]);
        getrandom::getrandom(seed.as_mut()).expect("system randomness unavailable");
        Account { seed }
    }

    /// The shareable account number, grouped `XXXX-XXXX-…` (32 chars, 160
    /// bits). This is the secret the user writes down.
    pub fn number(&self) -> SecretString {
        let bits = to_base32(self.seed.as_ref());
        bits.as_bytes()
            .chunks(GROUP)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
            .into()
    }

    /// Parse a typed account number (case- and separator-insensitive, with
    /// the same `O`→`0`, `I`/`L`→`1` forgiveness as the recovery code).
    pub fn parse(number: &str) -> Result<Account> {
        let norm: String = number
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| match c.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                other => other,
            })
            .collect();
        if norm.len() != CHARS {
            return Err(Error::BadKey(format!(
                "account number should be {CHARS} characters, got {}",
                norm.len()
            )));
        }
        let seed =
            from_base32(&norm).ok_or_else(|| Error::BadKey("invalid account number".into()))?;
        Ok(Account { seed })
    }

    /// Derive this account's keys for the device called `label` (e.g.
    /// "desktop", "iphone"). Deterministic: same account + label ⇒ same keys,
    /// which is how a lost device is restored. The label is case-folded and
    /// trimmed so "Desktop" and "desktop" match.
    pub fn device_keys(&self, label: &str) -> DeviceKeys {
        let hk = Hkdf::<Sha256>::new(Some(b"courierqr-account-v1"), self.seed.as_ref());
        let mut info = b"device\0".to_vec();
        info.extend_from_slice(label.trim().to_lowercase().as_bytes());
        let mut okm = Zeroizing::new([0u8; 64]);
        hk.expand(&info, okm.as_mut())
            .expect("64 is a valid HKDF length");

        let classic_seed: [u8; 32] = okm[..32].try_into().unwrap();
        let pq_seed: [u8; 32] = okm[32..].try_into().unwrap();
        // A classic age identity is a 32-byte X25519 scalar in Bech32 form.
        let classic_sk = bech32::encode("age-secret-key-", &classic_seed).to_ascii_uppercase();
        let pq = HybridIdentity::from_seed(&pq_seed);

        let classic_pub = crate::public_key_of(&classic_sk).expect("derived classic key is valid");
        let pq_pub = pq.to_public().to_bech32();
        DeviceKeys {
            public: format!(
                "{} {}",
                classic_pub.split_whitespace().next().unwrap(),
                pq_pub
            ),
            secret: format!("{classic_sk}\n{}", pq.to_bech32().expose_secret_str()).into(),
        }
    }
}

/// A short local **account number**: 16 uppercase hex characters (64 bits),
/// shown grouped as `XXXX-XXXX-XXXX-XXXX`. It is a *credential and handle*,
/// not key material — used to unlock a device's keys at rest and (later) to
/// bind a YubiKey. Key recovery is the separate strong [`Account`] seed.
///
/// 64 bits is ample here: unlocking is gated by scrypt in the vault, so
/// guessing is infeasible, and nothing is *derived* from this number.
pub struct AccountNumber([u8; 8]);

impl AccountNumber {
    /// A fresh random account number.
    pub fn generate() -> Self {
        let mut b = [0u8; 8];
        getrandom::getrandom(&mut b).expect("system randomness unavailable");
        AccountNumber(b)
    }

    /// Grouped for display/writing down: `A3F9-C2E1-8B7D-4056`.
    pub fn formatted(&self) -> String {
        let hex: String = self.0.iter().map(|b| format!("{b:02X}")).collect();
        hex.as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// The canonical passphrase form fed to the vault: 16 hex chars, no
    /// separators, uppercase. Stable regardless of how the user typed it.
    pub fn passphrase(&self) -> String {
        self.0.iter().map(|b| format!("{b:02X}")).collect()
    }

    /// Parse a typed account number: separators and case are ignored.
    pub fn parse(s: &str) -> Result<AccountNumber> {
        let hex: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if hex.len() != 16 {
            return Err(Error::BadKey(format!(
                "account number is 16 hex characters, got {}",
                hex.len()
            )));
        }
        let mut b = [0u8; 8];
        for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
            let s = std::str::from_utf8(pair).unwrap();
            b[i] = u8::from_str_radix(s, 16)
                .map_err(|_| Error::BadKey("account number must be hex (0-9, A-F)".into()))?;
        }
        Ok(AccountNumber(b))
    }
}

// 20 bytes ⇄ 32 Crockford base32 chars (exact, no padding).
fn to_base32(bytes: &[u8]) -> String {
    let (mut acc, mut bits, mut out) = (0u32, 0u32, String::with_capacity(CHARS));
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    out
}

fn from_base32(s: &str) -> Option<Zeroizing<[u8; SEED_LEN]>> {
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut out = Zeroizing::new([0u8; SEED_LEN]);
    let mut i = 0;
    for c in s.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out[i] = ((acc >> bits) & 0xff) as u8;
            i += 1;
        }
    }
    (i == SEED_LEN).then_some(out)
}

// Small helper so we don't pull ExposeSecret into every caller.
trait ExposeStr {
    fn expose_secret_str(&self) -> &str;
}
impl ExposeStr for SecretString {
    fn expose_secret_str(&self) -> &str {
        use age::secrecy::ExposeSecret;
        self.expose_secret()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrecy::ExposeSecret;
    use crate::{FrameConfig, OpenKey, SealKey, Transmission};

    #[test]
    fn number_round_trips_and_is_160_bits() {
        let a = Account::generate();
        let n = a.number();
        assert_eq!(
            n.expose_secret().chars().filter(|c| *c != '-').count(),
            CHARS
        );
        let b = Account::parse(n.expose_secret()).unwrap();
        // Same number ⇒ same derived device key.
        assert_eq!(
            a.device_keys("desktop").public,
            b.device_keys("desktop").public,
            "recovery reproduces the same keys"
        );
        assert_ne!(
            Account::generate().number().expose_secret(),
            n.expose_secret()
        );
    }

    #[test]
    fn parse_is_forgiving_about_formatting() {
        let a = Account::generate();
        let n = a.number();
        let messy = n.expose_secret().to_lowercase().replace('-', " ");
        assert_eq!(
            Account::parse(&messy).unwrap().device_keys("iphone").public,
            a.device_keys("iphone").public
        );
    }

    #[test]
    fn each_device_gets_a_distinct_key_that_still_decrypts() {
        let acct = Account::generate();
        let d1 = acct.device_keys("desktop");
        let d2 = acct.device_keys("iphone");
        assert_ne!(d1.public, d2.public, "per-device keys differ");

        // A message encrypted to device 1 opens with device 1's derived
        // secret, both classic and post-quantum.
        for method in [crate::Method::PostQuantum, crate::Method::Classic] {
            let tx = Transmission::new(
                "m.txt",
                b"hello from the account",
                &SealKey::for_device(&d1.public, method).unwrap(),
                FrameConfig::default(),
            )
            .unwrap();
            let mut rx = crate::Receiver::new();
            for f in tx.frames() {
                if rx.push(&f).unwrap() == crate::Accepted::Complete {
                    break;
                }
            }
            let got = rx
                .open(&OpenKey::identity(d1.secret.expose_secret().to_string()))
                .unwrap()
                .unwrap();
            assert_eq!(got.data, b"hello from the account", "{method:?}");
            // Device 2's key can't open device 1's message.
            assert!(
                rx.open(&OpenKey::identity(d2.secret.expose_secret().to_string()))
                    .unwrap()
                    .is_err()
            );
        }
    }

    #[test]
    fn account_number_is_16_hex_grouped_by_four() {
        let n = AccountNumber::generate();
        let f = n.formatted();
        assert_eq!(f.len(), 19); // 16 hex + 3 dashes
        assert_eq!(f.chars().filter(|c| *c == '-').count(), 3);
        assert!(
            f.chars()
                .all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
        );
        // Round-trips, and is forgiving about separators/case.
        assert_eq!(
            AccountNumber::parse(&f).unwrap().passphrase(),
            n.passphrase()
        );
        assert_eq!(
            AccountNumber::parse(&f.to_lowercase())
                .unwrap()
                .passphrase(),
            n.passphrase()
        );
        assert_eq!(
            AccountNumber::parse(&f.replace('-', " "))
                .unwrap()
                .passphrase(),
            n.passphrase()
        );
        assert_ne!(AccountNumber::generate().passphrase(), n.passphrase());
    }

    #[test]
    fn rejects_bad_account_numbers() {
        assert!(AccountNumber::parse("A3F9").is_err()); // too short
        assert!(AccountNumber::parse("GGGG-GGGG-GGGG-GGGG").is_err()); // G not hex
        assert!(AccountNumber::parse(&"A".repeat(17)).is_err());
    }

    #[test]
    fn rejects_malformed_numbers() {
        assert!(Account::parse("too-short").is_err());
        assert!(Account::parse(&"8".repeat(31)).is_err());
        assert!(Account::parse(&"U".repeat(32)).is_err()); // U not in alphabet
    }
}
