# CourierQR

Encrypted messaging and file transfer through animated QR codes.

CourierQR is a Rust-based project for exchanging encrypted data through camera-to-screen communication. It supports direct QR transfers and is being developed into a messenger with online, offline, and gateway configurations.

Think of it as a **cold wallet for messaging**: an offline device keeps its own messaging keys and decrypts messages locally, while an online gateway transports ciphertext.

> Experimental software under active development. Built today: the QR-transfer core, accounts with recovery, the terminal UI, and the first stage of the public feed (each gateway's Receive tab plays it). Still in development: gateway-to-gateway sync over Tor, the phone's feed receiver, and public stream mirrors. No independent security audit has been done.

## Features

- Send text and files through animated QR codes.
- Encrypt before converting data into QR codes.
- Use separate messaging keys for each device.
- Reconstruct transfers despite missed codes using RaptorQ fountain coding.
- Pair devices using public keys and fingerprint verification.
- Strip supported metadata before encryption.
- Support classic, passphrase-based, and hybrid post-quantum encryption paths.
- Keep this computer's keys encrypted at rest, unlocked with your account number.
- Create an account, or restore one from a recovery seed, in a first-run setup screen.
- Play a public feed of encrypted messages on the gateway's Receive tab for offline devices to scan.

Metadata removal is format-dependent. It does not guarantee that message contents are anonymous or that every identifying detail has been removed.

## Install

You need a Rust toolchain ([rustup.rs](https://rustup.rs)). On Linux, the on-screen window needs X11 headers: `sudo apt install libx11-dev libxkbcommon-dev`.

```sh
git clone https://github.com/FletchEm31/CourierQR
cd CourierQR
cargo install --path crates/courierqr-cli
```

This installs `courierqr` into `~/.cargo/bin`. Run the same `cargo install` command with `--force` after a `git pull` to update. For a headless build (export/receive only, no window or camera): `cargo install --path crates/courierqr-cli --no-default-features`.

The phone side is a web app in `web/` (installable PWA, built to WebAssembly); CI builds it and publishes it to GitHub Pages.

## Quick start

```sh
courierqr                 # opens the terminal UI; first run offers account setup
```

On first run, choose **Create a new account**. Write down the **account number** (it unlocks CourierQR on this computer) and the **recovery seed** (the only way to rebuild your keys on a new device). Both are shown once. Setup writes nothing until you type the account number back.

The terminal UI has three tabs:

| Tab | What it does |
|---|---|
| `1` Send | Type or paste text, pick a paired device or a passphrase, and the terminal shows it as animated QR codes. |
| `2` Receive | Plays the public feed. Press Enter to scan your phone with the webcam instead; `v` opens a full-resolution camera window. |
| `3` Devices | Pair devices and show this computer's key. |

Common commands:

```sh
courierqr pair show                         # this computer's key, as a QR code for the phone
courierqr pair add phone age1… age1pq1…     # pair a phone from its key set
courierqr pair list
courierqr send FILE --to phone              # show a file as QR codes in a window
courierqr receive --camera                  # receive from the webcam
courierqr feed publish FILE --to phone      # add an encrypted message to the public feed
courierqr feed list
courierqr restore SEED --device desktop     # set up a replacement computer from your seed
courierqr vault status
courierqr --help                            # everything else
```

The window has a retro look with five colour themes (Midnight, Classic, Amber, Phosphor, Synthwave). Click **Theme** in the title bar to switch (the choice is remembered) and **Logo** to change the header size. Both are also in the `Ctrl+P` command palette. The design it's being rebuilt toward is in [docs/design](docs/design/README.md).

Paste works in every text field: right-click → Paste, Ctrl+Shift+V, or Ctrl+V (Ctrl+V reads the clipboard through `wl-paste`, `xclip` or `xsel`, whichever is installed). Set `COURIERQR_ACCOUNT_NUMBER` to unlock without a prompt, for example in scripts.

**Camera tip:** if the webcam runs at half its frame rate in dim light, the camera is lengthening exposures. On Linux, `v4l2-ctl -d /dev/video0 -c exposure_dynamic_framerate=0` (from `v4l-utils`) keeps the full frame rate. The setting resets on reboot.

## Operating modes

| Mode | Purpose |
|---|---|
| Online messenger | Send and receive messages using the device’s own keys. |
| Offline messenger | Encrypt and decrypt locally; exchange data through camera and screen. |
| Messenger + gateway | Read the device’s own messages while relaying ciphertext for offline devices. |
| Gateway only | Transport ciphertext and display the public QR feed without holding recipients’ private messaging keys. |

These modes describe the target application architecture; not all network functionality is complete.

## How offline messaging works

### Sending

1. Compose text or select a file on the offline device.
2. Apply supported metadata stripping.
3. Encrypt locally for the intended recipient.
4. Display the ciphertext as animated QR codes.
5. An online gateway scans and forwards the encrypted data.

### Receiving

1. A gateway displays incoming ciphertext as animated QR codes.
2. The offline device scans the display.
3. RaptorQ reconstructs the transfer from enough usable pieces.
4. The offline device decrypts the message locally.

The gateway does not need the offline device’s private keys or readable messages.

## Encryption

| Component | Approach |
|---|---|
| Message encryption | Age-based encryption using ChaCha20-Poly1305 |
| Classic device encryption | X25519 recipient identities |
| Hybrid post-quantum path | X-Wing, combining ML-KEM-768 and X25519 |
| Passphrase protection | Scrypt-based key derivation |
| Account-derived device identities | HKDF-SHA256 from a secret master recovery seed and device label |
| Transfer recovery | RaptorQ fountain coding—not encryption |

Post-quantum mode requires a compatible recipient key and pairing. A classic-only pairing does not become post-quantum merely because the sender supports it.

The hybrid mode is a recipient-key establishment mechanism used with symmetric message encryption—not a promise that ciphertext can never be decrypted in the future.

## Metadata handling

CourierQR’s transfer workflow includes metadata stripping before encryption where supported.

The intended distinction is:

- Strip supported embedded metadata.
- Encrypt message contents and filename information.
- Keep private keys on the receiving endpoint.

Encryption does not automatically conceal ciphertext length, transmission timing, physical presence, or network activity. Metadata stripping also cannot remove identifying information a user writes into the message itself.

## Accounts and device identities

The intended model is one public account identity with separate messaging endpoints:

```text
CourierQR account
├── Desktop
│   ├── Desktop messaging keys
│   ├── Online messenger
│   └── Gateway
└── Offline phone
    ├── Separate messaging keys
    └── Offline messenger
```

The desktop can transport the phone’s ciphertext without being authorized to decrypt it. Sharing an account does not automatically grant access to every device’s private keys.

### Credentials and recovery

| Item | Purpose |
|---|---|
| Account number | 16 hex characters issued at sign-up; the login that unlocks the local vault. Not key material |
| Master recovery seed | Shown once at sign-up; recreates device keys from the seed + device label |
| Vault recovery code | Unlocks a standalone vault created without an account |

Account-derived and independently generated device identities are separate options.

Running `courierqr` on a computer with no keys opens a first-run setup screen: create an account, restore from your recovery seed, or continue without an account. After that, the terminal UI unlocks with your account number. Set `COURIERQR_ACCOUNT_NUMBER` to skip the prompt in scripts.

Anyone with sufficient master recovery material can recreate the corresponding device keys. Keep that material off gateways when maintaining offline-device isolation.

Recovery of keys does not automatically restore deleted messages or files.

## Shared public QR feed

The network combines users’ encrypted messages into one shared, publicly viewable QR broadcast. Anyone can view, record, or relay it; only the holder of the right private key can open a given message. An offline device can scan any display carrying the feed, not only its owner’s desktop.

**Built (stage 1):** each gateway keeps a local copy of the feed (encrypted envelopes, kept 72 hours, newest 512) and plays every message on its Receive tab whenever the camera is idle. Gateways store and pass on ciphertext only; they draw the QR codes locally and cannot open the messages. Add messages with `courierqr feed publish` or `courierqr feed import`.

**In development:**
- Gateways sync over Tor: push new messages to peers, and catch up on missed ones at login.
- A phone receiver that follows many messages on the feed at once and keeps only its own.
- Any gateway can mirror the feed to public video streams (YouTube, Twitch, Kick), for example inside a lofi stream.
- Abuse controls (proof-of-work or rate limits), size padding, and retention tuning.

See `DESIGN.md` for details.

## QR transfer and RaptorQ

CourierQR displays successive QR codes so a camera can collect data over time. Grid size, QR version, and display rate affect capacity and scanning reliability.

RaptorQ helps recover a transfer despite missed codes. It does not:

- Increase QR-code capacity.
- Encrypt data.
- Eliminate bandwidth limits.
- Automatically acknowledge delivery.
- Remove received messages from a feed.

Actual throughput depends on the display, camera, focus, lighting, QR settings, and successful decoding.

## Local security and planned features

Development priorities include:

- Gateway-to-gateway feed sync over Tor.
- A phone receiver for the public feed.
- Hardware-backed vault access, including YubiKey integration.
- Device enrollment and revocation.
- Account setup and restore in the phone app.
- Public stream mirrors of the feed.
- Optional long-term encrypted archives.

Blockchain-backed archival is exploratory and is not required for messaging. Archived messages would be stored separately rather than repeated forever in the active feed.

## Security limitations

CourierQR is experimental. Do not rely on it for high-risk communications without an independent security review.

It does not guarantee perfect anonymity, permanent confidentiality, or protection from compromised endpoints. Public ciphertext may be recorded, and removing it from the active feed cannot erase previously captured copies.

QR exchange is a data channel, not a guarantee that parsers, imported files, or receiving software are safe.

## Repository structure

```text
crates/courierqr-core/   Transfer, cryptography, accounts, feed playlist
crates/courierqr-cli/    Command-line app and terminal UI (gateway)
crates/courierqr-wasm/   WebAssembly build of the core, for the phone web app
web/                     Phone web app (PWA)
.github/workflows/       CI: tests, builds the web app, publishes it to GitHub Pages
DESIGN.md                Design documentation
```

## Contributing

Feedback is welcome on cryptographic design, offline-device isolation, metadata handling, recovery, QR performance, and shared-feed scalability.

Never include private keys, passwords, recovery seeds, or sensitive vault contents in issues.

## License

MIT
