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
| 2 | `courierqr` CLI: on-screen sender (1/2/4-code grid), PNG export, decode from images, keygen | ✅ done |
| 3 | Receiver: offline PWA, core compiled to WASM, camera decode in a Web Worker | next |
| 4 | Linux webcam receiver (phone → desktop) | planned |

## Quick start (Linux)

```sh
# needs X11 dev libs for the window: sudo apt install libx11-dev libxkbcommon-dev
cargo build --release
B=target/release/courierqr

# Show a file as a 2x2 grid of QR codes (prompts for a passphrase).
$B send notes.md --grid 4 --qr-version 20 --fps 8
#   Space = pause · ↑/↓ = faster/slower · Esc/Q = quit

# Paired mode: make a key on the receiving device once, then send to its public key.
$B keygen -o ~/.config/courierqr/device.key        # prints age1...
$B send notes.md --to age1...

# No camera yet? Export frames, or decode photos/screenshots/video stills.
$B export notes.md -o frames/
$B receive frames/*.png -o ~/Downloads             # any order, gaps are fine
```

`COURIERQR_PASSPHRASE` skips the prompt for scripting. Received file names are reduced to
a bare file name, so a sender can't write outside the output folder (`../../.bashrc`
becomes `.bashrc`).

**Choosing settings:** start with `--qr-version 20 --grid 4 --fps 8` on a laptop screen.
If the phone struggles, drop to `--grid 1` or a smaller version before you lower the fps.
Bigger codes carry more data per code but need sharper focus.

## Build & test

```sh
cargo test                 # fast suite
cargo test --release       # also runs the 12 MB multi-block test
```

The core tests simulate the camera as a hostile channel: 40% frame loss, 10% bit-flip
corruption, duplicates and reordering. They also cover wrong keys, foreign QR codes,
interleaved transfers, and hostile CRC-valid headers. The CLI tests render **real QR
images**, add blur, contrast loss and noise, decode them with
[rxing](https://github.com/rxing-core/rxing) (a Rust port of ZXing), and reassemble the
encrypted file. This covers the whole optical path except the physical camera.

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
