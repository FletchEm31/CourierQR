//! Sending side: file -> payload -> age ciphertext -> endless frame stream.

use std::borrow::Borrow;
use std::collections::VecDeque;

use raptorq::{Encoder, ObjectTransmissionInformation};

use crate::crypto::{self, SealKey};
use crate::frame::{self, MIN_FRAME_BYTES, OVERHEAD_BYTES};
use crate::{Error, MAX_TRANSFER_BYTES, Result, payload};

/// Binary (byte-mode) capacity of common QR versions at error-correction
/// level L. Pick the largest one your camera reads reliably.
pub mod qr_capacity {
    pub const V15_L: usize = 520;
    pub const V20_L: usize = 858;
    pub const V25_L: usize = 1273;
    pub const V30_L: usize = 1732;
    pub const V40_L: usize = 2953;
}

/// Repair symbols generated per source block each time the stream runs dry.
const REPAIR_BATCH: u32 = 32;
/// RaptorQ encoding symbol ids are 24 bits on the wire.
const MAX_ESI: u32 = (1 << 24) - 1;

/// How big each frame may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameConfig {
    /// Upper bound on bytes per frame — i.e. the QR code's byte capacity.
    pub max_frame_bytes: usize,
}

impl Default for FrameConfig {
    fn default() -> Self {
        FrameConfig {
            max_frame_bytes: qr_capacity::V25_L,
        }
    }
}

/// A prepared, encrypted transfer ready to be displayed.
pub struct Transmission {
    transfer_id: u32,
    oti: ObjectTransmissionInformation,
    oti_bytes: [u8; 12],
    encoder: Encoder,
    ciphertext_len: usize,
}

impl Transmission {
    /// Package, compress, encrypt and fountain-encode a file.
    pub fn new(name: &str, data: &[u8], key: &SealKey, config: FrameConfig) -> Result<Self> {
        if config.max_frame_bytes < MIN_FRAME_BYTES {
            return Err(Error::FrameBudgetTooSmall(config.max_frame_bytes));
        }
        let plaintext = payload::pack(name, data)?;
        let ciphertext = crypto::seal(key, &plaintext)?;
        if ciphertext.len() > MAX_TRANSFER_BYTES {
            return Err(Error::TooLarge(ciphertext.len()));
        }

        let symbol_budget = (config.max_frame_bytes - OVERHEAD_BYTES).min(u16::MAX as usize) as u16;
        let encoder = Encoder::with_defaults(&ciphertext, symbol_budget);
        let oti = encoder.get_config();

        Ok(Transmission {
            transfer_id: random_u32(),
            oti,
            oti_bytes: oti.serialize(),
            encoder,
            ciphertext_len: ciphertext.len(),
        })
    }

    /// Random id that lets receivers ignore frames from other transfers.
    pub fn transfer_id(&self) -> u32 {
        self.transfer_id
    }

    /// Size of the encrypted object being sent.
    pub fn ciphertext_len(&self) -> usize {
        self.ciphertext_len
    }

    /// Bytes of ciphertext carried per frame.
    pub fn symbol_size(&self) -> usize {
        self.oti.symbol_size() as usize
    }

    /// Minimum frames a receiver needs (it usually needs this plus 0–2).
    pub fn source_frames(&self) -> usize {
        self.encoder
            .get_block_encoders()
            .iter()
            .map(|b| b.source_packets().len())
            .sum()
    }

    /// Endless stream of frames: every source symbol once, then fresh
    /// repair symbols forever. Any `source_frames()`-ish subset decodes,
    /// in any order, so the display can simply loop on this.
    pub fn frames(&self) -> FrameStream<&Transmission> {
        FrameStream::new(self)
    }

    /// Like [`frames`](Self::frames), but the stream owns the transmission
    /// (handy when there's no stack frame to borrow from, e.g. in wasm).
    pub fn into_frames(self) -> FrameStream<Transmission> {
        FrameStream::new(self)
    }
}

/// Iterator returned by [`Transmission::frames`] / [`Transmission::into_frames`].
/// Never ends.
pub struct FrameStream<T: Borrow<Transmission>> {
    tx: T,
    queue: VecDeque<Vec<u8>>,
    sent_source: bool,
    repair_round: u32,
}

impl<T: Borrow<Transmission>> FrameStream<T> {
    fn new(tx: T) -> Self {
        FrameStream {
            tx,
            queue: VecDeque::new(),
            sent_source: false,
            repair_round: 0,
        }
    }

    /// The transmission this stream is drawing from.
    pub fn transmission(&self) -> &Transmission {
        self.tx.borrow()
    }

    fn refill(&mut self) {
        let blocks = self.tx.borrow().encoder.get_block_encoders();
        if !self.sent_source {
            for block in blocks {
                self.queue
                    .extend(block.source_packets().iter().map(|p| p.serialize()));
            }
            self.sent_source = true;
            return;
        }

        let mut start = self.repair_round.saturating_mul(REPAIR_BATCH);
        if start > MAX_ESI / 2 {
            // Days of continuous display; wrap rather than overflow 24 bits.
            self.repair_round = 0;
            start = 0;
        }
        // Interleave blocks so loss is spread evenly across them.
        let batches: Vec<_> = blocks
            .iter()
            .map(|b| b.repair_packets(start, REPAIR_BATCH))
            .collect();
        for i in 0..REPAIR_BATCH as usize {
            for batch in &batches {
                self.queue.push_back(batch[i].serialize());
            }
        }
        self.repair_round += 1;
    }
}

impl<T: Borrow<Transmission>> Iterator for FrameStream<T> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        if self.queue.is_empty() {
            self.refill();
        }
        let packet = self.queue.pop_front()?;
        let tx = self.tx.borrow();
        Some(frame::encode(tx.transfer_id, &tx.oti_bytes, &packet))
    }
}

/// Random transfer id from the OS / browser CSPRNG (same source age uses).
fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).expect("system random number generator unavailable");
    u32::from_ne_bytes(b)
}
