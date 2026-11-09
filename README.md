# CourierQR

Encrypted, one-way file and message transfer between **your own devices** over a
**screen → camera** link, in both directions: **computer → phone** and **phone → computer
(webcam)**. No Wi-Fi, Bluetooth, USB or cables. The sender shows a looping stream of QR
codes, and the receiver points a camera at it until the data reassembles.

It's built for moving notes, keys and documents on and off an **air-gapped** machine,
where plugging anything in is exactly what you're trying to avoid.

## A cold wallet for your data

Think of it the way people use a hardware wallet. The device that holds your keys and
readable data never touches a network. When something has to move between it and an
online device you own, it crosses as light: a stream of QR codes on one screen, read by the
other device's camera. Only ciphertext ever crosses. Keys are generated on each device and
the secret halves never leave them.

CourierQR works the same between two online devices, but the offline case is the point.

## How sending and receiving work

**Sending** (from either device):

1. Write a message or pick a file.
2. CourierQR strips metadata, compresses, then encrypts it locally, before anything is shown.
3. The device displays the ciphertext as an animated loop of QR codes (one code, or a 2×2 grid).

**Receiving:**

1. Point the other device's camera at the screen. Starting mid-loop is fine.
2. It collects encoded pieces until it has enough to rebuild the ciphertext.
3. It decrypts locally with its own key (or a passphrase) and saves the result.

Nothing is sent over a network at any point. The receiving device only needs a camera.

```
file ─► scrub metadata ─► pack (name + DEFLATE) ─► age encrypt ─► RaptorQ ─► frames ─► QR codes
                                                                                        │ camera
file ◄─ unpack ◄─ age decrypt ◄─ RaptorQ decode ◄─ CRC check ◄─ frames ◄────────────────┘
```

### Why the codes keep changing

A QR code holds a few hundred bytes, so anything bigger is split across many codes, shown in
sequence (four at a time in grid mode). The default computer sender shows 10 grid updates a
second (40 codes/s). The useful rate depends on the QR size, the camera, and how many codes
it actually reads.

Even a message that fits in **one** code keeps cycling. The sender can't know when the
camera has caught it, so it keeps showing fresh encodings of the same data until you stop it.

### What RaptorQ does

RaptorQ fountain codes ([RFC 6330](https://www.rfc-editor.org/rfc/rfc6330)) mean the
receiver doesn't need every code, or any particular order. The sender produces an endless
stream of distinct encoded pieces. Once the receiver holds roughly *K* of them (K ≈
ciphertext size ÷ piece size), any K, it can rebuild the data. It's a puzzle where you
don't need specific pieces, just enough of them. Missed frames, glare, a shaky hand, or
joining halfway through don't matter.

RaptorQ doesn't make each code hold more, and it isn't encryption. Its job is to tolerate
missed codes.

### Other details

- **Compress, then encrypt.** Ciphertext looks random and won't compress, so compression
  comes first. Text typically shrinks 5–20×, which means 5–20× less screen time.
- **Self-describing frames with a CRC-32.** Every frame carries the transfer ID and decoding
  parameters, so the receiver can lock on from any frame, and a mis-read frame is dropped
  before it can corrupt the decode. The file name travels *inside* the encryption.

## Encryption

CourierQR uses [age](https://age-encryption.org) (ChaCha20-Poly1305 with a reviewed header
format) rather than a custom scheme. Pick per message:

| Option | What it is | Who can open it | Overhead |
|---|---|---|---|
| **Post-quantum** (default for paired devices) | hybrid **ML-KEM-768 + X25519** (X-Wing), age's `mlkem768x25519` recipient type | only the paired device | ~1.4 KB per message |
| **Classic** | X25519 | only the paired device | ~0.2 KB |
| **Passphrase** | scrypt key derivation | anyone given the passphrase | ~0.2 KB |

**Why post-quantum.** QR codes on a screen can be filmed. Someone who records an X25519
transfer today could decrypt it years from now if a large enough quantum computer exists
("harvest now, decrypt later"). The hybrid needs **both** ML-KEM-768 (NIST FIPS 203) and
X25519 broken: a quantum computer alone isn't enough, and neither is an unexpected flaw in the
newer lattice scheme. It's the same format the official age tool ships as `age1pq1…` keys
(age v1.3+). It's checked against all 18 official
[C2SP/CCTV](https://github.com/C2SP/CCTV) hybrid test vectors, including the malformed and
low-order-point cases, so files are interoperable with `age`. The lattice maths comes from
RustCrypto's `ml-kem`. CourierQR only implements the age wire format and the X-Wing/HPKE glue
(`crates/courierqr-core/src/pq.rs`).

Passphrase mode has no public-key maths for a quantum computer to attack. With a strong
passphrase (four or more random words) it holds up either way.

**Keys and pairing.** Each device generates its own keys locally from the OS random number
generator: a classic X25519 identity and a post-quantum identity. The secret halves never
leave the device. There are no accounts, usernames or servers. A device's **public** key set
(`age1… age1pq1…`) moves to the other device by QR scan or copy/paste, and both screens show
a short fingerprint (`XXXX-XXXX-XXXX`) that you compare by eye. That comparison is what
proves the key wasn't swapped on the way.

Devices paired before post-quantum support keep working with classic encryption. Each one
gains a post-quantum key automatically the next time the app runs, which changes its
fingerprint, so pair it again to enable post-quantum sends to it.

## Status

| Stage | What | State |
|---|---|---|
| 1 | `courierqr-core`: pipeline, frame format, simulated-camera tests | ✅ done |
| 2 | `courierqr` CLI: on-screen sender (1/2/4-code grid), PNG export, decode from images, keygen | ✅ done |
| 3 | Phone receiver: offline web app (PWA), the same Rust core compiled to WebAssembly | ✅ done |
| 4 | Phone → computer: phone app sends, `courierqr receive --camera` reads it with a webcam | ✅ done |
| 5 | Terminal UI (ratatui): Send / Receive / Devices, live camera view and camera diagnostics | ✅ done |
| 6 | Automatic metadata stripping before encryption (photos, video, text, names) | ✅ done |
| 7 | Post-quantum hybrid encryption (ML-KEM-768 + X25519), chosen per message | ✅ done |

## Limitations

Encryption hides **what** you send, not **that** you sent something:

- **Size and timing show.** The number of codes on screen reveals roughly how big a message
  is, and anyone watching sees when a transfer happens.
- **Screens and cameras are physical.** Someone who can film your screen gets the
  ciphertext. That's why keys stay on the devices and why post-quantum exists here.
  Shoulder-surfing the *plaintext* on either device is outside what any encryption can stop.
- **Endpoints matter most.** A compromised phone or computer defeats everything above.
  CourierQR keeps secret keys off the wire, not off a compromised device.
- **It's a narrow pipe.** Real-world rates run from a few hundred bytes/s (phone screen →
  webcam) to tens of KB/s (computer screen → phone). It's for notes, keys and documents,
  not video libraries.
- **Some formats keep their metadata.** HEIC, PDF and Office files are sent unchanged, and
  the sender says so.

## Terminal UI

Run `courierqr` with no arguments (or `courierqr ui`) for a full-screen terminal app:

Tabs: **1 Send · 2 Receive · 3 Devices**.

- **Send:** type or paste a note, pick a paired phone (or a passphrase), choose
  **post-quantum** or **classic** (`Space`), and see exactly how it will be protected: who
  can open it, that metadata is stripped, and the code speed (`←/→`) and size. `Ctrl+S` turns the terminal into the
  transmitter, a loop of QR codes sized to the window. `Space` pauses, `↑/↓` change speed,
  `Esc` stops.
- **Receive:** a live camera view drawn right in the terminal, a pieces gauge, a
  pieces-per-second graph, and a breakdown of any QR codes it turned away. When the
  transfer completes it unlocks with this computer's key (or asks for the passphrase) and
  saves to `~/Downloads` without overwriting anything.
- **Camera details:** device, driver and USB bus; the stream's resolution and megapixels,
  pixel format, and the frame rate it was set to next to the rate actually measured; decode
  time per frame; the sensor's max resolution, supported formats and autofocus. A status
  line says what's limiting the read rate (camera dropping frames in low light, decoder
  falling behind, or no codes in view).
- **Devices:** paired phones with their fingerprints and whether each has a post-quantum
  key. Press `a` to pair one (paste the key set from the phone) and confirm its fingerprint
  before saving, or `d d` to forget one. Press `f` to show this computer's pairing code full
  screen for the phone to scan. With the post-quantum key it's a large code, so zoom the
  terminal out if it doesn't fit.
- `^p` command palette, `1-3` tabs, and mouse clicks on tabs.

Built with [ratatui](https://ratatui.rs). The UI tests render every screen into ratatui's
test backend, run the real camera worker on a generated recording, and decode the Send
tab's on-screen QR codes back into the typed note.

## Metadata stripping

Everything is scrubbed **before** encryption, automatically, in every sender (CLI, terminal
UI and phone app), without re-encoding, so quality is untouched:

| Content | Removed | Kept |
|---|---|---|
| JPEG photos | EXIF (GPS, camera make/model, dates, serials), XMP, IPTC, comments, depth/gain-map images after the photo | orientation (so it stays upright), colour profile |
| PNG / WebP | text fields (author, software, XMP), timestamps, EXIF | pixels, colour info, animation |
| MP4 / MOV video | location, device and software tags, XMP, creation/modification dates | audio/video untouched (tags are blanked in place, so nothing is remuxed) |
| Text | byte-order marks, zero-width and other invisible characters, bidi overrides, smuggling variation selectors and tag characters, look-alike spaces | emoji sequences and flags |
| File names | folder paths; typed notes are just `note.txt` (no timestamp) | the file name itself |

Formats it can't clean yet (HEIC, PDF, Office files, archives) are sent unchanged **and the
sender says so**. `courierqr send --keep-metadata` sends a byte-exact copy.

## Quick start (Linux)

```sh
# needs X11 dev libs for the window: sudo apt install libx11-dev libxkbcommon-dev
cargo build --release
B=target/release/courierqr

# Show a file as a 2x2 grid of QR codes (prompts for a passphrase).
$B send notes.md
#   Space = pause · ↑/↓ = faster/slower · Esc/Q = quit (progress also prints in the terminal)

# Paired mode (no passphrases): pair the phone once, then send by name.
$B pair add phone age1... age1pq1...  # the command the phone's Keys tab shows; confirm the fingerprint
$B send notes.md --to phone          # post-quantum by default; add --classic for X25519 only
$B pair list                       # names, fingerprints, keys

# Phone -> computer: pair once (the phone scans this computer's key), then receive by webcam.
$B pair show                       # prints a QR code + fingerprint in the terminal
$B cameras                         # list webcams
$B receive --camera -o ~/Downloads # live preview; unlocks with this computer's key automatically

# No camera? Export frames, or decode photos/screenshots/video stills.
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

**Pair the phone (once):** tap **Device key → Create device key**. The phone generates its
keys (classic and post-quantum) inside WebAssembly and keeps the secret halves in its own
storage. It shows a `courierqr pair add phone age1… age1pq1…` command; run that on your computer (copy it, text it to
yourself, or type it, since the public key isn't secret) and check that both screens show
the same fingerprint, like `EEHD-GY2Z-XADG`. From then on, `courierqr send file --to phone`
needs no passphrase: the phone unlocks the file by itself as soon as the last piece arrives.
Files sent with a passphrase still prompt for one. Install the app to your home screen so iOS
doesn't clear the stored key.

**Send from the phone (phone → computer):**
1. On the computer, run `courierqr pair show`. On the phone, tap **Keys → Scan computer's
   key**, point it at the terminal, and check the fingerprints match.
2. On the phone: **Send → choose a file → Start sending**. The screen turns into a full-screen
   QR code loop (it keeps the screen awake; tap to pause).
3. On the computer: `courierqr receive --camera`. Hold the phone 20–30 cm from the webcam,
   with screen brightness up, until the preview's bar fills. It unlocks with the computer's
   own key and saves the file.

Webcams see far less detail than phone cameras, so the phone defaults to **one medium code
(QR v10) at 6 per second**, about 1.4 KB/s. That's fine for notes, keys and small documents.
If your webcam is sharp, **Code settings** lets you go smaller and faster.

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
