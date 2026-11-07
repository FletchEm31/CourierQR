# CourierQR

Encrypted, one-way file transfer between your own devices over a **screen → camera** link.
No Wi-Fi, Bluetooth, USB, or cables: the sender shows a looping stream of QR codes, and
the receiver points a camera at it until the file reassembles.

It's built for moving notes, keys and documents on and off an air-gapped machine, where
plugging anything in is the thing you're trying to avoid.

## How it works

```
file ─► pack (name + DEFLATE) ─► age encrypt ─► RaptorQ ─► frames ─► QR codes on screen
                                                                          │ camera
file ◄─ unpack ◄─ age decrypt ◄─ RaptorQ decode ◄─ CRC check ◄─ frames ◄──┘
```

- **Compress, then encrypt.** Ciphertext looks random and can't be compressed, so
  compression has to come first. Text notes typically shrink 5–20×, which means 5–20× less
  screen time.
- **[age](https://age-encryption.org) encryption** (ChaCha20-Poly1305) instead of a custom
  scheme. There are two key modes:
  - *Passphrase:* type the same passphrase on both devices (scrypt key derivation).
  - *Paired devices:* the receiver generates an X25519 keypair once and shows its
    **public** key to the sender. A public key on screen is safe to show, so pairing needs no
    secret channel.
- **RaptorQ fountain codes ([RFC 6330](https://www.rfc-editor.org/rfc/rfc6330)).** The
  sender produces an endless stream of distinct symbols. The receiver needs roughly any *K*
  of them (K ≈ ciphertext size ÷ symbol size), in any order. Missed frames, glare, a shaky
  hand, or starting halfway through the loop don't matter.
- **Self-describing frames with a CRC-32.** Every frame carries the transfer ID and decoding
  parameters, so the receiver can lock on from any frame. A mis-read frame is dropped before
  it can corrupt the decode. The file name travels *inside* the encryption, so someone
  watching the screen can't see it.

## Status

| Stage | What | State |
|---|---|---|
| 1 | `courierqr-core`: pipeline, frame format, simulated-camera tests | ✅ done |
| 2 | Linux sender: render frames as QR codes in a window | next |
| 3 | Receiver: offline PWA, core compiled to WASM, camera decode in a Web Worker | planned |
| 4 | Linux webcam receiver (phone → desktop) | planned |

## Build & test

```sh
cargo test                 # fast suite
cargo test --release       # also runs the 12 MB multi-block test
```

The test suite simulates the camera as a hostile channel: 40% frame loss, 10% bit-flip
corruption, duplicates and reordering. It also covers wrong keys, foreign QR codes,
interleaved transfers, and hostile CRC-valid headers.

## Using the core

```rust
use courierqr_core::{Accepted, FrameConfig, OpenKey, Receiver, SealKey, Transmission};

let tx = Transmission::new("note.md", data, &SealKey::passphrase(pw), FrameConfig::default())?;
for frame in tx.frames() { /* render `frame` as a QR code, ~10–15 per second */ }

let mut rx = Receiver::new();
// for each QR payload the camera decodes:
if rx.push(&bytes) == Ok(Accepted::Complete) {
    let file = rx.open(&OpenKey::passphrase(pw)).unwrap()?;
}
```

See [`DESIGN.md`](DESIGN.md) for the wire format and threat model.

## License

MIT
