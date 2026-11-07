//! Wire format for a single optical frame (one QR code's worth of bytes).
//!
//! Every frame is self-describing, so the receiver can start watching at
//! any point in the loop and lock on from the first frame it reads.
//!
//! ```text
//! offset  size  field
//! 0       2     magic "CQ"
//! 2       1     format version (1)
//! 3       1     flags (reserved, 0)
//! 4       4     transfer id (random per transfer, big-endian)
//! 8       12    RaptorQ object transmission info (RFC 6330 OTI)
//! 20      4     CRC-32 over bytes 0..20 and the packet
//! 24      4     RaptorQ payload id (source block number + symbol id)
//! 28      ...   RaptorQ symbol
//! ```
//!
//! QR codes carry their own Reed-Solomon error correction, so the CRC is a
//! second line of defense: a mis-read frame must never reach the fountain
//! decoder, where one bad symbol would silently corrupt the whole file.
//! (Authenticity is age's job — the CRC is only about accidental damage.)

pub(crate) const MAGIC: [u8; 2] = *b"CQ";
pub(crate) const FORMAT_VERSION: u8 = 1;

/// Bytes of CourierQR header before the RaptorQ packet.
pub const HEADER_BYTES: usize = 24;
/// RaptorQ's own per-packet payload id.
pub const PAYLOAD_ID_BYTES: usize = 4;
/// Total fixed overhead per frame.
pub const OVERHEAD_BYTES: usize = HEADER_BYTES + PAYLOAD_ID_BYTES;
/// Smallest frame budget that still allows 8-byte symbol alignment.
pub const MIN_FRAME_BYTES: usize = OVERHEAD_BYTES + 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Frame<'a> {
    pub transfer_id: u32,
    pub oti: [u8; 12],
    /// Serialized RaptorQ `EncodingPacket` (payload id + symbol).
    pub packet: &'a [u8],
}

/// Why a frame was rejected before it reached the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Not a CourierQR frame at all (some other QR code in view).
    NotCourierQr,
    /// A CourierQR frame from a newer/unknown format version.
    UnsupportedVersion(u8),
    /// Too short to hold a header and packet.
    Truncated,
    /// Checksum mismatch: the frame was mis-read.
    Checksum,
    /// Header parsed but its parameters are nonsensical or unsupported.
    BadParameters,
}

pub(crate) fn encode(transfer_id: u32, oti: &[u8; 12], packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES + packet.len());
    out.extend_from_slice(&MAGIC);
    out.push(FORMAT_VERSION);
    out.push(0);
    out.extend_from_slice(&transfer_id.to_be_bytes());
    out.extend_from_slice(oti);
    let crc = checksum(&out, packet);
    out.extend_from_slice(&crc.to_be_bytes());
    out.extend_from_slice(packet);
    out
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Frame<'_>, FrameError> {
    if bytes.len() < 2 || bytes[..2] != MAGIC {
        return Err(FrameError::NotCourierQr);
    }
    if bytes.len() < 3 {
        return Err(FrameError::Truncated);
    }
    if bytes[2] != FORMAT_VERSION {
        return Err(FrameError::UnsupportedVersion(bytes[2]));
    }
    if bytes.len() < OVERHEAD_BYTES + 1 {
        return Err(FrameError::Truncated);
    }
    let (head, packet) = bytes.split_at(HEADER_BYTES);
    let stored = u32::from_be_bytes(head[20..24].try_into().unwrap());
    if checksum(&head[..20], packet) != stored {
        return Err(FrameError::Checksum);
    }
    Ok(Frame {
        transfer_id: u32::from_be_bytes(head[4..8].try_into().unwrap()),
        oti: head[8..20].try_into().unwrap(),
        packet,
    })
}

fn checksum(header: &[u8], packet: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(header);
    h.update(packet);
    h.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_bitflip() {
        let oti = [9u8; 12];
        let packet = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut f = encode(0xDEAD_BEEF, &oti, &packet);
        let d = decode(&f).unwrap();
        assert_eq!(d.transfer_id, 0xDEAD_BEEF);
        assert_eq!(d.oti, oti);
        assert_eq!(d.packet, &packet[..]);

        // Flip one bit anywhere past the magic/version and the CRC must catch it.
        for i in 3..f.len() {
            f[i] ^= 0x10;
            assert_eq!(decode(&f), Err(FrameError::Checksum), "byte {i}");
            f[i] ^= 0x10;
        }
    }

    #[test]
    fn rejects_foreign_and_short() {
        assert_eq!(
            decode(b"https://example.com"),
            Err(FrameError::NotCourierQr)
        );
        assert_eq!(decode(b"CQ"), Err(FrameError::Truncated));
        assert_eq!(
            decode(b"CQ\x09rest"),
            Err(FrameError::UnsupportedVersion(9))
        );
        assert_eq!(decode(&[b'C', b'Q', 1, 0, 0]), Err(FrameError::Truncated));
    }
}
