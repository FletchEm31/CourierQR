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
| 3 | Phone receiver: offline web app (PWA), the same Rust core compiled to WebAssembly | ✅ done |
| 4 | Linux webcam receiver (phone → desktop) | planned |

## Quick start (Linux)

```sh
# needs X11 dev libs for the window: sudo apt install libx11-dev libxkbcommon-dev
cargo build --release
B=target/release/courierqr

# Show a file as a 2x2 grid of QR codes (prompts for a passphrase).
$B send notes.md
#   Space = pause · ↑/↓ = faster/slower · Esc/Q = quit (progress also prints in the terminal)

# Paired mode (no passphrases): pair the phone once, then send by name.
$B pair add phone age1...          # from the phone's "Device key" screen; confirm the fingerprint
$B send notes.md --to phone
$B pair list                       # names, fingerprints, keys

# No camera yet? Export frames, or decode photos/screenshots/video stills.
$B export notes.md -o frames/
$B receive frames/*.png -o ~/Downloads             # any order, gaps are fine
```

`COURIERQR_PASSPHRASE` skips the prompt for scripting. Received file names are reduced to
a bare file name, so a sender can't write outside the output folder (`../../.bashrc`
becomes `.bashrc`).

**Choosing settings:** the defaults (`--grid 4 --qr-version 15 --fps 8`) came out of
camera tests. A 2×2 grid of v15 codes was read in 100% of blurry, tilted camera frames,
while v20 codes at the same screen size dropped to 22%. More, smaller codes beat fewer,
bigger ones. If your phone struggles, lower `--fps` first, then try `--qr-version 12`.

## Phone receiver

The receiver is a web app with no install and no account. It works offline once loaded,
and nothing leaves the phone.

1. **Host it once.** Push to GitHub, then go to repo **Settings → Pages → Source: GitHub
   Actions**. The workflow in `.github/workflows/` runs the tests, builds the WebAssembly,
   and publishes `web/` to `https://<you>.github.io/CourierQR/`. Phones need HTTPS for the
   camera, and Pages provides it.
2. **Open it on your phone** and add it to your home screen (Share → Add to Home Screen on
   iOS, ⋮ → Install app on Android). The service worker caches the whole app, so after this
   it works in airplane mode.
3. **Receive:** run `courierqr send file.md` on the computer, tap **Start camera**, and point
   the phone at the codes. The bar fills as pieces arrive, in any order. When it's full, type
   the passphrase and then save, share or copy the file.

**Pair the phone (once):** tap **Device key → Create device key**. The phone generates a
keypair inside WebAssembly and keeps the secret half in its own storage. It shows a
`courierqr pair add phone age1…` command; run that on your computer (copy it, text it to
yourself, or type it, since the public key isn't secret) and check that both screens show
the same fingerprint, like `EEHD-GY2Z-XADG`. From then on, `courierqr send file --to phone`
needs no passphrase: the phone unlocks the file by itself as soon as the last piece arrives.
Files sent with a passphrase still prompt for one. Install the app to your home screen so iOS
doesn't clear the stored key.

The app is locked down: a strict Content-Security-Policy allows no outside origins at all,
decryption happens in a Web Worker inside WebAssembly, and the decrypted bytes are wiped
from WebAssembly memory as soon as they're handed to the page.

Develop locally with `./web/build.sh --serve`, which serves on `http://localhost:8080`.
Desktop browsers allow the camera on localhost.

## Build & test

```sh
cargo test                 # fast suite (core, CLI, and the wasm crate's C ABI natively)
cargo test --release       # also runs the 12 MB multi-block test
./web/build.sh             # build web/courierqr.wasm (rustup target add wasm32-unknown-unknown)

# Compare QR detection strategies on a camera recording:
cargo run --release -p courierqr-wasm --example scanbench -- capture.y4m
```

The core tests simulate the camera as a hostile channel: 40% frame loss, 10% bit-flip
corruption, duplicates and reordering. They also cover wrong keys, foreign QR codes,
interleaved transfers, and hostile CRC-valid headers. The CLI tests render **real QR
images**, add blur, contrast loss and noise, decode them with
[rxing](https://github.com/rxing-core/rxing) (a Rust port of ZXing), and reassemble the
encrypted file. This covers the whole optical path except the physical camera.

The web app was tested in real headless Chrome with a **fake webcam**: a synthesized video
of the sender as a phone would film it (perspective tilt, rotation, blur, washed-out
contrast, lighting gradient, sensor noise, frames caught mid-change). The app received a
63 KB file in 2.6 s, rejected a wrong passphrase, and saved a byte-identical download. It
also ran again with the network switched off, served entirely from its offline cache.

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
