//! Receiving side: feed it whatever frames the camera manages to read,
//! in any order, with gaps and duplicates; it tells you when it's done.

use std::collections::HashSet;

use raptorq::{Decoder, EncodingPacket, ObjectTransmissionInformation};

use crate::crypto::{self, OpenKey};
use crate::frame::{self, FrameError, PAYLOAD_ID_BYTES};
use crate::payload::{self, ReceivedFile};
use crate::{MAX_TRANSFER_BYTES, Result};

/// What happened to a frame handed to [`Receiver::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepted {
    /// New symbol stored; not enough yet.
    Progress,
    /// Already had this symbol (the display looped, or the camera saw the
    /// same frame twice).
    Duplicate,
    /// Valid frame, but from a different transfer than the one we locked on.
    OtherTransfer,
    /// This frame completed the transfer. Call [`Receiver::open`].
    Complete,
    /// Transfer was already complete; frame ignored.
    AlreadyComplete,
}

/// Snapshot for a progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Distinct symbols received so far.
    pub received: usize,
    /// Symbols needed in the ideal case (actual need is this plus ~0–2).
    pub needed: usize,
    pub complete: bool,
}

impl Progress {
    /// 0.0..=1.0, clamped (can briefly exceed `needed` before completing).
    pub fn fraction(&self) -> f32 {
        if self.complete {
            1.0
        } else if self.needed == 0 {
            0.0
        } else {
            (self.received as f32 / self.needed as f32).min(0.99)
        }
    }
}

struct Locked {
    transfer_id: u32,
    oti_bytes: [u8; 12],
    symbol_size: usize,
    source_blocks: u8,
    needed: usize,
    decoder: Decoder,
    seen: HashSet<[u8; 4]>,
}

/// Accumulates frames for one transfer.
#[derive(Default)]
pub struct Receiver {
    locked: Option<Locked>,
    ciphertext: Option<Vec<u8>>,
}

impl Receiver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer one decoded QR payload. Errors mean the frame was dropped
    /// (unreadable, foreign QR code, corrupted) — just keep scanning.
    pub fn push(&mut self, bytes: &[u8]) -> std::result::Result<Accepted, FrameError> {
        let f = frame::decode(bytes)?;
        if self.ciphertext.is_some() {
            return Ok(Accepted::AlreadyComplete);
        }

        let locked = match &mut self.locked {
            Some(l) if l.transfer_id != f.transfer_id => return Ok(Accepted::OtherTransfer),
            Some(l) => {
                if l.oti_bytes != f.oti {
                    // Same id, different parameters: not one of ours.
                    return Err(FrameError::BadParameters);
                }
                l
            }
            None => self.locked.insert(lock_on(f.transfer_id, f.oti)?),
        };

        if f.packet.len() != PAYLOAD_ID_BYTES + locked.symbol_size {
            return Err(FrameError::BadParameters);
        }
        let id: [u8; 4] = f.packet[..4].try_into().unwrap();
        if id[0] >= locked.source_blocks {
            return Err(FrameError::BadParameters);
        }
        if !locked.seen.insert(id) {
            return Ok(Accepted::Duplicate);
        }

        match locked.decoder.decode(EncodingPacket::deserialize(f.packet)) {
            Some(ct) => {
                self.ciphertext = Some(ct);
                Ok(Accepted::Complete)
            }
            None => Ok(Accepted::Progress),
        }
    }

    pub fn progress(&self) -> Progress {
        match &self.locked {
            None => Progress {
                received: 0,
                needed: 0,
                complete: false,
            },
            Some(l) => Progress {
                received: l.seen.len(),
                needed: l.needed,
                complete: self.ciphertext.is_some(),
            },
        }
    }

    /// Transfer id we're locked on, if any.
    pub fn transfer_id(&self) -> Option<u32> {
        self.locked.as_ref().map(|l| l.transfer_id)
    }

    pub fn is_complete(&self) -> bool {
        self.ciphertext.is_some()
    }

    /// The reassembled (still encrypted) object, once complete.
    pub fn ciphertext(&self) -> Option<&[u8]> {
        self.ciphertext.as_deref()
    }

    /// Decrypt and unpack the finished transfer.
    /// Returns `None` if the transfer isn't complete yet.
    pub fn open(&self, key: &OpenKey) -> Option<Result<ReceivedFile>> {
        self.ciphertext.as_deref().map(|ct| open(ct, key))
    }

    /// Forget everything and lock onto the next transfer seen.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Decrypt and unpack a reassembled ciphertext.
pub fn open(ciphertext: &[u8], key: &OpenKey) -> Result<ReceivedFile> {
    let plaintext = crypto::open(key, ciphertext)?;
    payload::unpack(&plaintext)
}

/// Validate untrusted transmission parameters before building a decoder,
/// so a garbage (but CRC-valid) header can't make us allocate wildly or
/// trip a panic inside raptorq.
fn lock_on(transfer_id: u32, oti_bytes: [u8; 12]) -> std::result::Result<Locked, FrameError> {
    let oti = ObjectTransmissionInformation::deserialize(&oti_bytes);
    let symbol_size = oti.symbol_size() as usize;
    let align = oti.symbol_alignment() as usize;
    let len = oti.transfer_length();

    let sane = len > 0
        && len <= MAX_TRANSFER_BYTES as u64
        && matches!(align, 1 | 8)
        && symbol_size >= align
        && symbol_size.is_multiple_of(align)
        && oti.source_blocks() >= 1
        && oti.sub_blocks() >= 1
        && oti.sub_blocks() as usize <= symbol_size / align
        // Each block must hold at most RaptorQ's max of 56,403 symbols.
        && len.div_ceil(symbol_size as u64).div_ceil(oti.source_blocks() as u64) <= 56_403;
    if !sane {
        return Err(FrameError::BadParameters);
    }

    Ok(Locked {
        transfer_id,
        oti_bytes,
        symbol_size,
        source_blocks: oti.source_blocks(),
        needed: len.div_ceil(symbol_size as u64) as usize,
        decoder: Decoder::new(oti),
        seen: HashSet::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_valid_but_hostile_parameters_are_rejected() {
        let packet = [0u8; 4 + 64];
        // transfer_length = 0
        let zero_len = [0u8; 12];
        // absurd transfer length (2^40 - 1), symbol size 64, 1 block, 1 sub-block, align 8
        let huge = [
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x40, 0x01, 0x00, 0x01, 0x08,
        ];
        // alignment 3 is invalid
        let bad_align = [
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x40, 0x01, 0x00, 0x01, 0x03,
        ];
        for oti in [zero_len, huge, bad_align] {
            let f = frame::encode(1, &oti, &packet);
            let mut rx = Receiver::new();
            assert_eq!(rx.push(&f), Err(FrameError::BadParameters), "{oti:?}");
            assert!(rx.transfer_id().is_none());
        }
    }
}
