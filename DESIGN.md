# CourierQR design notes

## Scope

One-way transfer between devices **you own**, across an air gap, using a display and a
camera. The receiver never transmits anything back to the sender.

## Pipeline

1. **Pack.** `[version=1][name_len u16][name][bytes]`
2. **Compress.** Raw DEFLATE (miniz_oxide, level 9), used only if it makes the data smaller.
   A leading method byte (`0` stored, `1` deflate) records which. Decompression is capped at
   `MAX_TRANSFER_BYTES` (64 MiB), so a malicious "zip bomb" can't exhaust the receiver's memory.
3. **Encrypt.** age v1, using either a scrypt passphrase recipient (default cost 2^16, and
   receivers refuse anything above 2^20) or an X25519 recipient.
4. **Fountain code.** RaptorQ (`raptorq` crate). Symbol size = frame budget − 28 bytes, rounded
   down to a multiple of 8. The sender emits every source symbol once, then fresh repair
   symbols forever, interleaved across source blocks.
5. **Frame.** See the format below. Each frame becomes one QR code (byte mode, ECC level L).

## Frame format (v1)

| offset | size | field |
|---|---|---|
| 0 | 2 | magic `"CQ"` |
| 2 | 1 | format version `1` |
| 3 | 1 | flags (reserved, `0`) |
| 4 | 4 | transfer ID (random u32, big-endian) |
| 8 | 12 | RaptorQ OTI (RFC 6330 §3.3) |
| 20 | 4 | CRC-32 over bytes 0..20 + packet |
| 24 | 4 | RaptorQ payload ID (SBN + 24-bit ESI) |
| 28 | n | symbol |

That's 28 bytes of overhead per frame. At QR version 25-L (1273 bytes) that leaves 1245
bytes of data per frame, about 97.8% efficiency.

## Throughput (rough)

| QR version (ECC L) | bytes/frame | at 12 good frames/s |
|---|---|---|
| 20 | 830 | ~10 KB/s |
| 25 | 1245 | ~15 KB/s |
| 30 | 1704 | ~20 KB/s |

A 2×2 grid of codes multiplies these numbers by up to 4. The real limit is the phone camera
(autofocus, exposure, rolling shutter), not the encoder. The display should hold each frame
for 2–3 screen refreshes rather than matching the camera's frame rate. Color barcodes
([libcimbar](https://github.com/sz3/libcimbar)) reach ~100 KB/s, so the barcode layer is kept
separate from the core so a denser format could replace QR later.

## Threat model

**Protects against:** anyone who sees or records the screen. They get ciphertext and the
transfer size, but not the contents or file name. Also protects against accidental frame
corruption (CRC + age's authentication tag) and malformed or hostile headers (bounded
parameters, bounded memory, no panics on untrusted input).

**Does not protect against:**
- *File size leakage.* An observer can count frames. Padding to fixed size buckets is a
  possible later option.
- *A compromised sender or receiver device.* That's outside what any transport can fix.
- *Sender authentication in paired mode.* Anyone who knows a receiver's public key could
  display a file for it. Passphrase mode authenticates implicitly, since only someone with
  the passphrase can produce a valid file. Signing the payload (e.g. Ed25519) is a possible
  later addition.
- *Weak passphrases.* scrypt slows down guessing but can't save a guessable passphrase. Use
  paired mode or a 5+ word passphrase.

## Lessons from stage 2

- **QR decoders want to give you text, and text breaks binary data.** rxing's `getText()`
  guesses a character set: 50 bytes of `0xC3` came back as 100 characters. CourierQR reads
  the byte-mode payload instead (`getRawBytes()` / byte segments), and the encoder forces a
  single byte-mode segment so there is nothing to guess. Any receiver (including the
  browser one) must do the same. The frame CRC is the safety net if it doesn't.
- **Integer module scaling.** Codes are drawn at a whole number of pixels per module and
  never resampled (`ScaleMode::Center`), so every module on screen is the same size.
- **Hold frames across refreshes.** The window redraws at 60 Hz but changes codes only at
  `--fps`, so a camera never catches a code half-drawn.

## Browser receiver (stage 3)

```
camera ─► <video> ─► createImageBitmap ─► Web Worker ─► OffscreenCanvas ─► grayscale
                                                └─► courierqr.wasm: QR detect ─► frames ─► RaptorQ ─► age
```

- **One Rust core, two targets.** `crates/courierqr-wasm` wraps the same `courierqr-core`
  the CLI uses, plus rxing's QR detector, behind about 20 plain `extern "C"` functions. There
  is no wasm-bindgen, so the build needs only `cargo`, and `web/courierqr.js` (the whole JS
  glue) is about 150 lines. The module imports exactly one function, `env.host_random`,
  which is backed by `crypto.getRandomValues` (getrandom's `custom` backend).
- **Lesson: no clock on `wasm32-unknown-unknown`.** rxing's high-level readers stamp every
  result with `chrono::Utc::now()`, which calls `SystemTime::now()` and panics in the
  browser. The scanner drives rxing's `MultiDetector`, `Detector` and
  `decode_bitmatrix_with_hints` directly. It never builds a timestamped result, and it gets
  the byte segments without any charset guessing.
- **Backpressure, not a queue.** The page sends a new camera frame only when the worker
  reports back, so slow phones skip frames instead of building lag. Frames are downscaled to
  1280 px on the long side before detection.
- **Offline and locked down.** A cache-first service worker means the installed app never
  re-fetches its code on its own; CI stamps the cache version with the commit SHA, so a new
  deploy is a new, explicit version. The CSP is `default-src 'none'` plus same-origin script,
  `wasm-unsafe-eval`, and nothing else.

### Pairing

The phone generates an X25519 identity inside WebAssembly (randomness from
`crypto.getRandomValues`) and stores the secret in IndexedDB. Only the public key leaves the
phone, by any channel, because it isn't secret. The risk is *substitution*: someone swapping
in their own key on the way. Both sides therefore show the same **fingerprint**: 60 bits of
`SHA-256("courierqr-fingerprint-v1:" ‖ key)` in Crockford base32 (`EEHD-GY2Z-XADG`),
computed by the same Rust function on both devices and pinned by a test vector. `pair add`
won't save a device until you confirm the match. The bech32 checksum in `age1…` keys catches
typing mistakes before that.

On completion the phone tries its device key first. age reports "no matching keys" for a
passphrase-sealed file, and the app then falls back to the passphrase prompt.

**Storage caveats:**
- **Shared origin on GitHub Pages.** Browser storage is per origin, and every Pages site on
  an account shares `<user>.github.io`. Any *other* Pages site you publish on the same
  account could read the stored device key. Keep CourierQR the only Pages site on that
  account, or give it its own origin (a custom domain).
- **iOS eviction.** Safari can clear storage for sites you haven't visited in a while. Apps
  added to the home screen are exempt, and the app also requests persistent storage. If the
  key is lost, create a new one and re-pair; files already sent to the old key can't be
  opened.

### What the camera tests showed

Synthetic phone-camera footage of the sender (keystone, ±3° rotation, Gaussian blur, 72%
contrast, noise), 1280×720, run through the actual decoder (`examples/scanbench.rs`):

| Sender setting | Frames with a read | Codes/frame | ≈ bytes/frame |
|---|---|---|---|
| 1 × v20 | 94% | 0.94 | 775 |
| 1 × v25 | 86% | 0.86 | 1,070 |
| **2×2 × v15** | **100%** | **3.56** | **1,740** |
| 2×2 × v20 | 22% | 0.31 | 255 |

Detection collapses once a QR module is only about 3 blurred pixels wide. Different
detection strategies barely moved the numbers (19% → 21% at 2× the cost); module size is
what matters. That's why the defaults are now 2×2 × v15.

## Known follow-ups

- Desktop webcam receiver (stage 4). It would also let `pair add` scan the phone's key QR
  directly instead of copying the text.
- Real-phone tuning (iOS Safari focus behaviour, frame rates). The synthetic tests are a
  floor, not a promise.
- Tune the scrypt cost against real phone browser timings.
