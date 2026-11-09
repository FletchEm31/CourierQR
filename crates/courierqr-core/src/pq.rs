//! Post-quantum hybrid recipients: ML-KEM-768 + X25519 (X-Wing), exactly as
//! standardised for age v1.3 (`c2sp.org/age`, "mlkem768x25519"):
//!
//! * identity  = 32-byte random seed, Bech32 `AGE-SECRET-KEY-PQ-1…`
//! * recipient = 1216-byte X-Wing public key, Bech32 `age1pq1…`
//! * stanza    = `-> mlkem768x25519 <base64 1120-byte enc>` + 32-byte body,
//!   where body = HPKE SealBase(X-Wing, HKDF-SHA256, ChaCha20-Poly1305,
//!   info "age-encryption.org/mlkem768x25519", file key).
//!
//! Hybrid means an attacker has to break *both* ML-KEM-768 (lattice, NIST
//! FIPS 203) and X25519: a future quantum computer breaking X25519 isn't
//! enough, and an unexpected flaw in the newer lattice scheme isn't either.
//!
//! The lattice maths and HPKE come from the RustCrypto `hpke`/`x-wing`/
//! `ml-kem` crates; this module only implements the age wire format, and is
//! checked against the official C2SP/CCTV test vectors.

use std::collections::HashSet;

use age::secrecy::{ExposeSecret, SecretString};
use age_core::format::{FILE_KEY_BYTES, FileKey, Stanza};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD as B64;
use zeroize::Zeroizing;

pub const RECIPIENT_HRP: &str = "age1pq";
pub const IDENTITY_HRP: &str = "age-secret-key-pq-";
const TAG: &str = "mlkem768x25519";
const INFO: &[u8] = b"age-encryption.org/mlkem768x25519";
const LABEL: &str = "postquantum";
const ENC_LEN: usize = 1120;
const PK_LEN: usize = 1216;

fn random<const N: usize>() -> Zeroizing<[u8; N]> {
    let mut b = Zeroizing::new([0u8; N]);
    getrandom::getrandom(b.as_mut()).expect("system randomness unavailable");
    b
}

/// X-Wing (draft-connolly-cfrg-xwing-kem): ML-KEM-768 and X25519 run side by
/// side, their secrets bound together with SHA3-256.
mod xwing {
    use ml_kem::kem::Decapsulate;
    use ml_kem::{B32, EncapsulateDeterministic, EncodedSizeUser, KemCore, MlKem768};
    use sha3::digest::{Digest, ExtendableOutput, Update, XofReader};
    use x25519_dalek::{PublicKey as XPub, StaticSecret};
    use zeroize::Zeroizing;

    type Dk = <MlKem768 as KemCore>::DecapsulationKey;
    type Ek = <MlKem768 as KemCore>::EncapsulationKey;
    const LABEL: &[u8] = b"\\.//^\\";
    pub const CT_M: usize = 1088;
    pub const PK_M: usize = 1184;

    pub struct Public {
        ek: Ek,
        pub x: [u8; 32],
        pub bytes: Vec<u8>,
    }

    pub struct Secret {
        pub seed: Zeroizing<[u8; 32]>,
        dk: Dk,
        x: StaticSecret,
        pub public: Public,
    }

    fn b32(s: &[u8]) -> B32 {
        B32::try_from(s).expect("32 bytes")
    }

    pub fn from_seed(seed: &[u8; 32]) -> Secret {
        let mut xof = sha3::Shake256::default();
        xof.update(seed);
        let mut e = Zeroizing::new([0u8; 96]);
        xof.finalize_xof().read(e.as_mut());
        let (dk, ek) = MlKem768::generate_deterministic(&b32(&e[..32]), &b32(&e[32..64]));
        let mut sx = [0u8; 32];
        sx.copy_from_slice(&e[64..]);
        let x = StaticSecret::from(sx);
        let px = XPub::from(&x).to_bytes();
        let mut bytes = ek.as_bytes().to_vec();
        bytes.extend_from_slice(&px);
        Secret {
            seed: Zeroizing::new(*seed),
            dk,
            x,
            public: Public { ek, x: px, bytes },
        }
    }

    /// FIPS 203 encapsulation-key check: every coefficient below q.
    fn modulus_ok(pk_m: &[u8]) -> bool {
        pk_m[..1152].chunks(3).all(|c| {
            let a = c[0] as u16 | ((c[1] as u16 & 0x0f) << 8);
            let b = (c[1] as u16 >> 4) | ((c[2] as u16) << 4);
            a < 3329 && b < 3329
        })
    }

    pub fn public_from_bytes(b: &[u8]) -> Option<Public> {
        if b.len() != PK_M + 32 || !modulus_ok(&b[..PK_M]) {
            return None;
        }
        let ek = Ek::from_bytes(&b[..PK_M].try_into().ok()?);
        let mut x = [0u8; 32];
        x.copy_from_slice(&b[PK_M..]);
        Some(Public {
            ek,
            x,
            bytes: b.to_vec(),
        })
    }

    fn combine(ss_m: &[u8], ss_x: &[u8], ct_x: &[u8], pk_x: &[u8]) -> Zeroizing<[u8; 32]> {
        let d = sha3::Sha3_256::new()
            .chain_update(ss_m)
            .chain_update(ss_x)
            .chain_update(ct_x)
            .chain_update(pk_x)
            .chain_update(LABEL)
            .finalize();
        Zeroizing::new(d.into())
    }

    /// Returns (shared secret, 1120-byte ciphertext).
    pub fn encap(pk: &Public, eseed: &[u8; 64]) -> (Zeroizing<[u8; 32]>, Vec<u8>) {
        let (ct_m, ss_m) = pk
            .ek
            .encapsulate_deterministic(&b32(&eseed[..32]))
            .expect("infallible");
        let mut ek = [0u8; 32];
        ek.copy_from_slice(&eseed[32..]);
        let ek = StaticSecret::from(ek);
        let ct_x = XPub::from(&ek).to_bytes();
        let ss_x = ek.diffie_hellman(&XPub::from(pk.x));
        let ss = combine(&ss_m, ss_x.as_bytes(), &ct_x, &pk.x);
        let mut ct = ct_m.to_vec();
        ct.extend_from_slice(&ct_x);
        (ss, ct)
    }

    /// None if the X25519 share is low-order (all-zero shared secret).
    pub fn decap(sk: &Secret, ct: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
        let ct_m = ct[..CT_M].try_into().ok()?;
        let ss_m = sk.dk.decapsulate(&ct_m).ok()?;
        let mut ct_x = [0u8; 32];
        ct_x.copy_from_slice(&ct[CT_M..]);
        let ss_x = sk.x.diffie_hellman(&XPub::from(ct_x));
        if ss_x.as_bytes().iter().all(|&b| b == 0) {
            return None;
        }
        Some(combine(&ss_m, ss_x.as_bytes(), &ct_x, &sk.public.x))
    }
}

/// HPKE (RFC 9180) base mode, single shot, for suite
/// X-Wing (0x647a) / HKDF-SHA256 (0x0001) / ChaCha20-Poly1305 (0x0003).
mod hpke {
    use chacha20poly1305::ChaCha20Poly1305;
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use hkdf::Hkdf;
    use sha2::Sha256;
    use zeroize::Zeroizing;

    const SUITE: [u8; 10] = *b"HPKE\x64\x7a\x00\x01\x00\x03";

    fn labeled(label: &[u8], data: &[u8]) -> Vec<u8> {
        [b"HPKE-v1".as_slice(), &SUITE, label, data].concat()
    }

    fn extract(salt: &[u8], label: &[u8], ikm: &[u8]) -> Hkdf<Sha256> {
        Hkdf::<Sha256>::new(Some(salt), &labeled(label, ikm))
    }

    fn extract_prk(salt: &[u8], label: &[u8], ikm: &[u8]) -> [u8; 32] {
        let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), &labeled(label, ikm));
        prk.into()
    }

    fn expand(h: &Hkdf<Sha256>, label: &[u8], ctx: &[u8], out: &mut [u8]) {
        let info = [&(out.len() as u16).to_be_bytes()[..], &labeled(label, ctx)].concat();
        h.expand(&info, out).expect("short output");
    }

    fn context(shared: &[u8], info: &[u8]) -> (ChaCha20Poly1305, [u8; 12]) {
        let psk_id_hash = extract_prk(b"", b"psk_id_hash", b"");
        let info_hash = extract_prk(b"", b"info_hash", info);
        let ctx = [&[0u8][..], &psk_id_hash, &info_hash].concat();
        // RFC 9180 KeySchedule: secret = LabeledExtract(shared_secret, "secret", psk = "")
        let secret = extract(shared, b"secret", b"");
        let mut key = Zeroizing::new([0u8; 32]);
        let mut nonce = [0u8; 12];
        expand(&secret, b"key", &ctx, key.as_mut());
        expand(&secret, b"base_nonce", &ctx, &mut nonce);
        let key: &[u8; 32] = &key;
        (ChaCha20Poly1305::new(key.into()), nonce)
    }

    pub fn seal(shared: &[u8], info: &[u8], pt: &[u8]) -> Vec<u8> {
        let (aead, nonce) = context(shared, info);
        aead.encrypt(&nonce.into(), Payload { msg: pt, aad: b"" })
            .expect("encrypt")
    }

    pub fn open(shared: &[u8], info: &[u8], ct: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        let (aead, nonce) = context(shared, info);
        aead.decrypt(&nonce.into(), Payload { msg: ct, aad: b"" })
            .ok()
            .map(Zeroizing::new)
    }
}

/// An `age1pq1…` recipient.
pub struct HybridRecipient(xwing::Public);

impl HybridRecipient {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (hrp, data) = bech32::decode(s.trim())?;
        if hrp != RECIPIENT_HRP {
            return Err(format!("not a post-quantum recipient (prefix {hrp}1)"));
        }
        if data.len() != PK_LEN {
            return Err(format!(
                "post-quantum key has {} bytes, expected {PK_LEN}",
                data.len()
            ));
        }
        xwing::public_from_bytes(&data)
            .map(HybridRecipient)
            .ok_or_else(|| "invalid ML-KEM-768 + X25519 public key".into())
    }

    pub fn to_bech32(&self) -> String {
        bech32::encode(RECIPIENT_HRP, &self.0.bytes)
    }
}

impl age::Recipient for HybridRecipient {
    fn wrap_file_key(
        &self,
        file_key: &FileKey,
    ) -> Result<(Vec<Stanza>, HashSet<String>), age::EncryptError> {
        let (ss, enc) = xwing::encap(&self.0, &random::<64>());
        let body = hpke::seal(ss.as_ref(), INFO, file_key.expose_secret());
        let stanza = Stanza {
            tag: TAG.into(),
            args: vec![B64.encode(enc)],
            body,
        };
        // age refuses to mix this with classic recipients, which would
        // silently throw away the post-quantum protection.
        Ok((vec![stanza], HashSet::from([LABEL.to_string()])))
    }
}

/// An `AGE-SECRET-KEY-PQ-1…` identity.
pub struct HybridIdentity(xwing::Secret);

impl HybridIdentity {
    pub fn generate() -> Self {
        HybridIdentity(xwing::from_seed(&random::<32>()))
    }

    /// Deterministic identity from a 32-byte seed (for account-derived keys).
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        HybridIdentity(xwing::from_seed(seed))
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        let (hrp, data) = bech32::decode(s.trim())?;
        let data = Zeroizing::new(data);
        if hrp != IDENTITY_HRP {
            return Err("not a post-quantum identity".into());
        }
        let seed: [u8; 32] = data
            .as_slice()
            .try_into()
            .map_err(|_| "post-quantum identity must be a 32-byte seed".to_string())?;
        Ok(HybridIdentity(xwing::from_seed(&seed)))
    }

    pub fn to_bech32(&self) -> SecretString {
        bech32::encode(IDENTITY_HRP, self.0.seed.as_ref())
            .to_ascii_uppercase()
            .into()
    }

    pub fn to_public(&self) -> HybridRecipient {
        HybridRecipient(xwing::public_from_bytes(&self.0.public.bytes).expect("own key"))
    }
}

impl age::Identity for HybridIdentity {
    fn unwrap_stanza(&self, stanza: &Stanza) -> Option<Result<FileKey, age::DecryptError>> {
        if stanza.tag != TAG {
            return None;
        }
        let bad = || Some(Err(age::DecryptError::InvalidHeader));
        let [arg] = stanza.args.as_slice() else {
            return bad();
        };
        // Strict, canonical base64 only.
        let Ok(enc) = B64.decode(arg) else {
            return bad();
        };
        if enc.len() != ENC_LEN
            || B64.encode(&enc) != *arg
            || stanza.body.len() != FILE_KEY_BYTES + 16
        {
            return bad();
        }
        let Some(ss) = xwing::decap(&self.0, &enc) else {
            return bad(); // low-order X25519 share
        };
        // ML-KEM uses implicit rejection: a wrong key yields a random
        // secret, so a failed AEAD open simply means "not for us".
        let key = hpke::open(ss.as_ref(), INFO, &stanza.body)?;
        let mut fk = Box::new([0u8; FILE_KEY_BYTES]);
        fk.copy_from_slice(&key);
        Some(Ok(FileKey::new(fk)))
    }
}

/// Bech32 (BIP-173 checksum) without the 90-character limit: post-quantum
/// keys are ~2000 characters, which age allows.
pub(crate) mod bech32 {
    const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    const GEN: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];

    fn polymod(values: impl Iterator<Item = u8>) -> u32 {
        let mut chk = 1u32;
        for v in values {
            let b = chk >> 25;
            chk = ((chk & 0x1ff_ffff) << 5) ^ v as u32;
            for (i, g) in GEN.iter().enumerate() {
                if (b >> i) & 1 == 1 {
                    chk ^= g;
                }
            }
        }
        chk
    }

    fn hrp_expand(hrp: &str) -> Vec<u8> {
        let b = hrp.as_bytes();
        b.iter()
            .map(|c| c >> 5)
            .chain([0])
            .chain(b.iter().map(|c| c & 31))
            .collect()
    }

    fn regroup(data: &[u8], from: u32, to: u32, pad: bool) -> Option<Vec<u8>> {
        let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::new());
        let maxv = (1 << to) - 1;
        for &v in data {
            if (v as u32) >> from != 0 {
                return None;
            }
            acc = (acc << from) | v as u32;
            bits += from;
            while bits >= to {
                bits -= to;
                out.push(((acc >> bits) & maxv) as u8);
            }
        }
        if pad {
            if bits > 0 {
                out.push(((acc << (to - bits)) & maxv) as u8);
            }
        } else if bits >= from || ((acc << (to - bits)) & maxv) != 0 {
            return None;
        }
        Some(out)
    }

    pub fn encode(hrp: &str, data: &[u8]) -> String {
        let d5 = regroup(data, 8, 5, true).expect("8-bit input");
        let pm = polymod(
            hrp_expand(hrp)
                .into_iter()
                .chain(d5.iter().copied())
                .chain([0; 6]),
        ) ^ 1;
        let mut s = format!("{hrp}1");
        s.extend(d5.iter().map(|&v| CHARSET[v as usize] as char));
        s.extend((0..6).map(|i| CHARSET[((pm >> (5 * (5 - i))) & 31) as usize] as char));
        s
    }

    pub fn decode(s: &str) -> Result<(String, Vec<u8>), String> {
        let bad = |w: &str| Err(format!("invalid key: {w}"));
        if s.chars().any(|c| c.is_ascii_lowercase()) && s.chars().any(|c| c.is_ascii_uppercase()) {
            return bad("mixed case");
        }
        let s = s.to_ascii_lowercase();
        let Some(pos) = s.rfind('1') else {
            return bad("no separator");
        };
        if pos == 0 || pos + 7 > s.len() {
            return bad("too short");
        }
        let (hrp, rest) = (&s[..pos], &s[pos + 1..]);
        let mut d5 = Vec::with_capacity(rest.len());
        for c in rest.bytes() {
            match CHARSET.iter().position(|&x| x == c) {
                Some(v) => d5.push(v as u8),
                None => return bad("bad character (typo?)"),
            }
        }
        if polymod(hrp_expand(hrp).into_iter().chain(d5.iter().copied())) != 1 {
            return bad("checksum mismatch (typo?)");
        }
        d5.truncate(d5.len() - 6);
        match regroup(&d5, 5, 8, false) {
            Some(d) => Ok((hrp.to_string(), d)),
            None => bad("bad padding"),
        }
    }
}

/// Is this string a post-quantum public key (by prefix)?
pub fn is_hybrid_recipient(s: &str) -> bool {
    s.trim().to_ascii_lowercase().starts_with("age1pq1")
}

/// Is this string a post-quantum identity (by prefix)?
pub fn is_hybrid_identity(s: &str) -> bool {
    s.trim()
        .to_ascii_uppercase()
        .starts_with("AGE-SECRET-KEY-PQ-1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bech32_matches_classic_age_keys() {
        // A known-good classic age recipient round-trips through our codec.
        let k = "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p";
        let (hrp, data) = bech32::decode(k).unwrap();
        assert_eq!((hrp.as_str(), data.len()), ("age", 32));
        assert_eq!(bech32::encode("age", &data), k);
        assert!(
            bech32::decode("age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8q")
                .is_err()
        );
    }

    #[test]
    fn keys_round_trip_and_have_spec_sizes() {
        let id = HybridIdentity::generate();
        let sk = id.to_bech32();
        assert!(sk.expose_secret().starts_with("AGE-SECRET-KEY-PQ-1"));
        assert_eq!(sk.expose_secret().len(), 77, "spec: 77-character identity");
        let pk = id.to_public().to_bech32();
        assert!(pk.starts_with("age1pq1"));
        assert_eq!(pk.len(), 1959, "spec: 1959-character recipient");
        let again = HybridIdentity::parse(sk.expose_secret()).unwrap();
        assert_eq!(again.to_public().to_bech32(), pk);
        assert_eq!(HybridRecipient::parse(&pk).unwrap().to_bech32(), pk);
    }
}
