//! Public feed playlist: many messages interleaved into one frame stream.
//!
//! Gateways replicate [`Envelope`]s (opaque ciphertext + random id), never
//! images. Each gateway turns its current set of envelopes into frames
//! locally with a [`Playlist`]: every message gets its own endless
//! RaptorQ stream, and the playlist takes one frame from each in turn. A
//! viewer that catches any large-enough subset of one message's frames can
//! rebuild it, so missed frames, late joiners and lossy re-encoding (a
//! YouTube mirror) only cost time.
//!
//! Nothing here decrypts. A gateway can show messages it has no key for.

use std::collections::HashSet;

use crate::{Envelope, FrameConfig, FrameStream, Transmission};

/// Round-robin frame source over a changing set of envelopes.
pub struct Playlist {
    config: FrameConfig,
    entries: Vec<(String, FrameStream<Transmission>)>,
    cursor: usize,
}

impl Playlist {
    pub fn new(config: FrameConfig) -> Self {
        Playlist {
            config,
            entries: Vec::new(),
            cursor: 0,
        }
    }

    /// Messages currently in rotation.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Make the rotation match `envelopes`: new ones join at the end,
    /// ones no longer present leave, and the rest keep their place in their
    /// stream (so a viewer mid-collection isn't reset). Returns how many
    /// envelopes were skipped because they couldn't be framed.
    pub fn sync(&mut self, envelopes: &[Envelope]) -> usize {
        let wanted: HashSet<String> = envelopes.iter().map(Envelope::id_hex).collect();
        let before = self.entries.len();
        self.entries.retain(|(id, _)| wanted.contains(id));
        if self.entries.len() != before {
            self.cursor = 0;
        }
        let have: HashSet<String> = self.entries.iter().map(|(id, _)| id.clone()).collect();
        let mut skipped = 0;
        for env in envelopes {
            let id = env.id_hex();
            if have.contains(&id) {
                continue;
            }
            match Transmission::from_ciphertext(env.ciphertext(), self.config) {
                Ok(tx) => self.entries.push((id, tx.into_frames())),
                Err(_) => skipped += 1,
            }
        }
        skipped
    }

    /// The next frame to show, taking one from each message in turn.
    /// `None` only when the playlist is empty.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        if self.entries.is_empty() {
            return None;
        }
        self.cursor %= self.entries.len();
        let frame = self.entries[self.cursor].1.next();
        self.cursor = (self.cursor + 1) % self.entries.len();
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrecy::ExposeSecret;
    use crate::{Accepted, OpenKey, Receiver, SealKey, generate_device_keys, split_key_set};

    fn sealed_to(text: &[u8]) -> (Envelope, String) {
        let keys = generate_device_keys();
        let (_, pq) = split_key_set(&keys.public).unwrap();
        let (env, _) =
            Envelope::seal_file("note.txt", text, &SealKey::recipient(pq.unwrap())).unwrap();
        (env, keys.secret.expose_secret().to_string())
    }

    /// Two messages to two different people share one feed; each person
    /// rebuilds theirs from the interleaved frames, even dropping a third.
    #[test]
    fn interleaved_feed_delivers_each_message() {
        let (a, ka) = sealed_to(b"for alice");
        let (b, kb) = sealed_to(b"for bob, a little longer than alice's");
        let mut pl = Playlist::new(FrameConfig::default());
        assert_eq!(pl.sync(&[a.clone(), b.clone()]), 0);
        assert_eq!(pl.len(), 2);

        let (mut ra, mut rb) = (Receiver::new(), Receiver::new());
        let (mut done_a, mut done_b) = (false, false);
        for i in 0..400 {
            let f = pl.next_frame().unwrap();
            if i % 3 == 0 {
                continue; // a third of the frames never reach the camera
            }
            // A receiver locks onto the first transfer it sees. The second
            // one takes the frames the first rejects as another transfer
            // (and everything, once the first is done).
            let r = ra.push(&f);
            if matches!(r, Ok(Accepted::Complete)) {
                done_a = true;
            }
            if (done_a || matches!(r, Ok(Accepted::OtherTransfer)))
                && matches!(rb.push(&f), Ok(Accepted::Complete))
            {
                done_b = true;
            }
            if done_a && done_b {
                break;
            }
        }
        assert!(done_a && done_b, "both transfers completed");
        // Whichever receiver locked onto which message, each opens with the
        // matching key and the other key fails.
        let open = |rx: &Receiver, k: &str| rx.open(&OpenKey::identity(k.to_string())).unwrap();
        let got: Vec<Vec<u8>> = [&ra, &rb]
            .iter()
            .map(|rx| {
                open(rx, &ka)
                    .or_else(|_| open(rx, &kb))
                    .expect("opens with one of the keys")
                    .data
            })
            .collect();
        assert!(got.contains(&b"for alice".to_vec()));
        assert!(got.contains(&b"for bob, a little longer than alice's".to_vec()));
    }

    #[test]
    fn sync_adds_and_removes_without_resetting_the_rest() {
        let (a, _) = sealed_to(b"a");
        let (b, _) = sealed_to(b"b");
        let mut pl = Playlist::new(FrameConfig::default());
        pl.sync(std::slice::from_ref(&a));
        for _ in 0..5 {
            pl.next_frame();
        }
        pl.sync(&[a.clone(), b.clone()]);
        assert_eq!(pl.len(), 2);
        pl.sync(std::slice::from_ref(&b));
        assert_eq!(pl.len(), 1);
        pl.sync(&[]);
        assert!(pl.is_empty() && pl.next_frame().is_none());
    }
}
