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

## Known follow-ups

- Confirm the core builds for `wasm32-unknown-unknown` (needs `getrandom`'s `wasm_js`
  backend), and that rxing works in the browser with raw-byte output.
- Tune the scrypt cost against real phone browser timings.
