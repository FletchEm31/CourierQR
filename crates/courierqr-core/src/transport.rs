//! Store-and-forward transport envelope.
//!
//! The optical path ([`crate::sender`]) is for a live screen → camera link,
//! where frames are lossy and order-free, so it uses RaptorQ. A **transport**
//! is the other case: a reliable store-and-forward channel inside a *closed,
//! admin-controlled group* — a private Matrix room, or a relay run by an
//! admitted member — where the bytes arrive intact and in order. There the
//! fountain coding is just overhead, so a transport message is the age
//! ciphertext wrapped in a tiny envelope.
//!
//! The envelope is deliberately metadata-poor. It carries **no sender and no
//! recipient in the clear**: within the group a message is simply broadcast,
//! and each member tries its own key ([`crate::Receiver::open`] →
//! `NoMatchingKeys` means "not mine, ignore"). All a relay or an onlooker can
//! see is an opaque blob and a random id. The id exists only so relays and
//! recipients can drop duplicates — it is not derived from the content or the
//! keys, so it links nothing.
//!
//! ```text
//! [magic "CQX" | version=1 | msg id (16 random bytes) | age ciphertext …]
//! ```
//!
//! This stays off the air-gapped device: the offline device produces the age
//! ciphertext optically as today; an online bridge reassembles it, wraps it
//! here, and posts the blob to the group. The reverse on the way in. Keys
//! never touch the bridge.

use crate::{Error, MAX_TRANSFER_BYTES, Result};

const MAGIC: &[u8; 3] = b"CQX";
const VERSION: u8 = 1;
const ID_LEN: usize = 16;
const HEADER_LEN: usize = MAGIC.len() + 1 + ID_LEN; // 20

/// A transport message: a random id plus the age ciphertext it carries.
#[derive(Clone, PartialEq, Eq)]
pub struct Envelope {
    id: [u8; ID_LEN],
    ciphertext: Vec<u8>,
}

impl Envelope {
    /// Wrap an age ciphertext (from [`crate::seal`] /
    /// [`crate::Transmission`]'s input) with a fresh random id.
    pub fn new(ciphertext: Vec<u8>) -> Self {
        let mut id = [0u8; ID_LEN];
        getrandom::getrandom(&mut id).expect("system randomness unavailable");
        Envelope { id, ciphertext }
    }

    /// The message id, for de-duplication. Lowercase hex.
    pub fn id_hex(&self) -> String {
        self.id.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The age ciphertext to hand to [`crate::open`] /
    /// [`crate::Receiver`]-style decryption with the member's own key.
    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }

    pub fn into_ciphertext(self) -> Vec<u8> {
        self.ciphertext
    }

    /// Serialize for posting to the group.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.ciphertext.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    /// Parse a blob pulled from the group. Rejects anything that isn't a
    /// well-formed envelope, so a relay carrying junk can't wedge a client.
    pub fn decode(bytes: &[u8]) -> Result<Envelope> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::BadEnvelope("too short"));
        }
        if &bytes[..3] != MAGIC {
            return Err(Error::BadEnvelope("wrong magic"));
        }
        if bytes[3] != VERSION {
            return Err(Error::BadEnvelope("unsupported version"));
        }
        let body = &bytes[HEADER_LEN..];
        // age ciphertext is never empty, and the transfer cap applies here too.
        if body.is_empty() {
            return Err(Error::BadEnvelope("no ciphertext"));
        }
        if body.len() > MAX_TRANSFER_BYTES {
            return Err(Error::BadEnvelope("larger than the transfer limit"));
        }
        let mut id = [0u8; ID_LEN];
        id.copy_from_slice(&bytes[4..4 + ID_LEN]);
        Ok(Envelope {
            id,
            ciphertext: body.to_vec(),
        })
    }
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Envelope")
            .field("id", &self.id_hex())
            .field("ciphertext_len", &self.ciphertext.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{open, seal};
    use crate::secrecy::ExposeSecret;
    use crate::{OpenKey, SealKey, generate_device_keys};

    #[test]
    fn round_trips_and_carries_an_openable_ciphertext() {
        let keys = generate_device_keys();
        let (classic, pq) = crate::split_key_set(&keys.public).unwrap();
        let ct = seal(&SealKey::recipient(pq.unwrap()), b"meet at the usual place").unwrap();
        let env = Envelope::new(ct.clone());

        let wire = env.encode();
        assert_eq!(&wire[..3], b"CQX");
        let back = Envelope::decode(&wire).unwrap();
        assert_eq!(back.id_hex(), env.id_hex());
        assert_eq!(back.ciphertext(), &ct[..]);

        // The recipient opens it with their own identity.
        let pt = open(
            &OpenKey::identity(keys.secret.expose_secret().to_string()),
            back.ciphertext(),
        )
        .unwrap();
        assert_eq!(pt, b"meet at the usual place");
        let _ = classic;
    }

    #[test]
    fn a_non_members_key_just_fails_to_open() {
        let alice = generate_device_keys();
        let (_, pq) = crate::split_key_set(&alice.public).unwrap();
        let ct = seal(&SealKey::recipient(pq.unwrap()), b"group secret").unwrap();
        let blob = Envelope::new(ct).encode();

        // Someone outside the key (or just another member) tries their key:
        // it's not addressed to them, so age reports no matching keys.
        let bob = generate_device_keys();
        let env = Envelope::decode(&blob).unwrap();
        let err = open(
            &OpenKey::identity(bob.secret.expose_secret().to_string()),
            env.ciphertext(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::Decrypt(age::DecryptError::NoMatchingKeys)
        ));
    }

    #[test]
    fn ids_are_unique_so_relays_can_dedupe() {
        let a = Envelope::new(vec![1, 2, 3]);
        let b = Envelope::new(vec![1, 2, 3]);
        assert_ne!(a.id_hex(), b.id_hex(), "fresh id per message");
        assert_eq!(a.id_hex().len(), 32);
    }

    #[test]
    fn malformed_blobs_are_rejected() {
        assert!(Envelope::decode(b"").is_err());
        assert!(Envelope::decode(b"CQX").is_err());
        assert!(Envelope::decode(b"XXX\x01 0123456789abcdef body").is_err());
        let mut v = b"CQX\x01".to_vec();
        v.extend_from_slice(&[0u8; ID_LEN]); // header but no ciphertext
        assert!(Envelope::decode(&v).is_err());
    }
}
