//! Plaintext payload: file metadata + (optionally) compressed contents.
//!
//! Layout of the plaintext that gets encrypted:
//!
//! ```text
//! [method: u8] [body ...]
//!   method 0 = stored, 1 = DEFLATE (raw, miniz_oxide)
//!
//! body (after decompression):
//! [version: u8 = 1] [name_len: u16 BE] [name: UTF-8] [file bytes ...]
//! ```
//!
//! The file name lives *inside* the encryption, so nobody watching the
//! screen learns what is being sent.

use crate::{Error, MAX_NAME_BYTES, MAX_TRANSFER_BYTES, Result};

const PAYLOAD_VERSION: u8 = 1;
const METHOD_STORED: u8 = 0;
const METHOD_DEFLATE: u8 = 1;
const DEFLATE_LEVEL: u8 = 9;

/// A file recovered at the receiving end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFile {
    pub name: String,
    pub data: Vec<u8>,
}

/// Build the plaintext for `name`/`data`, compressing only if it helps.
pub(crate) fn pack(name: &str, data: &[u8]) -> Result<Vec<u8>> {
    if name.len() > MAX_NAME_BYTES {
        return Err(Error::NameTooLong(name.len()));
    }
    let mut body = Vec::with_capacity(3 + name.len() + data.len());
    body.push(PAYLOAD_VERSION);
    body.extend_from_slice(&(name.len() as u16).to_be_bytes());
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(data);
    if body.len() > MAX_TRANSFER_BYTES {
        return Err(Error::TooLarge(body.len()));
    }

    let deflated = miniz_oxide::deflate::compress_to_vec(&body, DEFLATE_LEVEL);
    let (method, chosen) = if deflated.len() < body.len() {
        (METHOD_DEFLATE, deflated)
    } else {
        // Already-compressed input (JPEG, zip, ...): don't pay for nothing.
        (METHOD_STORED, body)
    };

    let mut out = Vec::with_capacity(1 + chosen.len());
    out.push(method);
    out.extend_from_slice(&chosen);
    Ok(out)
}

/// Reverse of [`pack`]. Decompression is capped so a hostile sender can't
/// zip-bomb the receiver's memory.
pub(crate) fn unpack(plaintext: &[u8]) -> Result<ReceivedFile> {
    let (&method, rest) = plaintext.split_first().ok_or(Error::BadPayload("empty"))?;
    let body = match method {
        METHOD_STORED => rest.to_vec(),
        METHOD_DEFLATE => {
            miniz_oxide::inflate::decompress_to_vec_with_limit(rest, MAX_TRANSFER_BYTES)
                .map_err(|_| Error::Decompress)?
        }
        _ => return Err(Error::BadPayload("unknown compression method")),
    };

    if body.len() < 3 {
        return Err(Error::BadPayload("truncated header"));
    }
    if body[0] != PAYLOAD_VERSION {
        return Err(Error::BadPayload("unsupported payload version"));
    }
    let name_len = u16::from_be_bytes([body[1], body[2]]) as usize;
    let name_end = 3 + name_len;
    if body.len() < name_end {
        return Err(Error::BadPayload("truncated file name"));
    }
    let name = std::str::from_utf8(&body[3..name_end])
        .map_err(|_| Error::BadPayload("file name is not UTF-8"))?
        .to_owned();

    Ok(ReceivedFile {
        name,
        data: body[name_end..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_gets_compressed() {
        let text = "# Note\n".repeat(500);
        let packed = pack("note.md", text.as_bytes()).unwrap();
        assert_eq!(packed[0], METHOD_DEFLATE);
        assert!(packed.len() < text.len() / 4);
        let back = unpack(&packed).unwrap();
        assert_eq!(back.name, "note.md");
        assert_eq!(back.data, text.as_bytes());
    }

    #[test]
    fn incompressible_is_stored() {
        // Pseudo-random bytes (xorshift) don't compress.
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let data: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let packed = pack("blob.bin", &data).unwrap();
        assert_eq!(packed[0], METHOD_STORED);
        assert_eq!(unpack(&packed).unwrap().data, data);
    }

    #[test]
    fn rejects_garbage() {
        assert!(unpack(&[]).is_err());
        assert!(unpack(&[7, 1, 2, 3]).is_err());
        assert!(unpack(&[METHOD_STORED, 1, 0, 50, b'a']).is_err());
    }
}
