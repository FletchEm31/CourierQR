Yes—this version includes the encryption types, metadata stripping, RaptorQ, device separation, recovery, and planned network features. Your screenshots also show that the currently paired phone needs re-pairing before post-quantum mode is available; the README preserves that distinction. [ppl-ai-file-upload.s3.amazonaws](https://ppl-ai-file-upload.s3.amazonaws.com/web/direct-files/attachments/images/1351163241/d3626d59-bbc9-42a6-ba0a-bf92dc6f50c4/Screenshot-From-2026-11-09-05-04-58.jpg)

***

# CourierQR

Encrypted messaging and file transfer through animated QR codes.

CourierQR is a Rust-based project for exchanging encrypted data through camera-to-screen communication. It supports direct QR transfers and is being developed into a messenger with online, offline, and gateway configurations.

Think of it as a **cold wallet for messaging**: an offline device keeps its own messaging keys and decrypts messages locally, while an online gateway transports ciphertext.

> Experimental software under active development. The QR-transfer core exists; the shared messaging network and unified account architecture are still being developed. No claim of an independent security audit is made.

## Features

- Send text and files through animated QR codes.
- Encrypt before converting data into QR codes.
- Use separate messaging keys for each device.
- Reconstruct transfers despite missed codes using RaptorQ fountain coding.
- Pair devices using public keys and fingerprint verification.
- Strip supported metadata before encryption.
- Support classic, passphrase-based, and hybrid post-quantum encryption paths.
- Protect local secrets using encrypted vault primitives.

Metadata removal is format-dependent. It does not guarantee that message contents are anonymous or that every identifying detail has been removed.

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
| Public account ID | Identifies the account; not a recovery secret |
| Account password | Authenticates account access |
| Device unlock credential | Unlocks a local vault |
| Vault recovery code | Unlocks the matching vault or backup |
| Master recovery seed | Recreates device keys covered by its derivation scheme |

Account-derived and independently generated device identities are separate options.

Anyone with sufficient master recovery material can recreate the corresponding device keys. Keep that material off gateways when maintaining offline-device isolation.

Recovery of keys does not automatically restore deleted messages or files.

## Shared public QR feed

The proposed network combines users’ encrypted messages into a shared, publicly accessible QR broadcast.

Anyone could view, record, or relay the feed. Only holders of the appropriate private keys should be able to decrypt a particular message.

An offline device could scan any compatible display carrying the required feed—not only its owner’s desktop.

Recipient discovery, relay distribution, scheduling, retention, and abuse controls remain development work. Tor transport and Matrix-based replication are under consideration.

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

- Unified account and device-role setup.
- Hardware-backed vault access, including YubiKey integration.
- Device enrollment, revocation, and recovery.
- Shared-feed scheduling and recipient discovery.
- Optional long-term encrypted archives.

Blockchain-backed archival is exploratory and is not required for messaging. Archived messages would be stored separately rather than repeated forever in the active feed.

## Security limitations

CourierQR is experimental. Do not rely on it for high-risk communications without an independent security review.

It does not guarantee perfect anonymity, permanent confidentiality, or protection from compromised endpoints. Public ciphertext may be recorded, and removing it from the active feed cannot erase previously captured copies.

QR exchange is a data channel, not a guarantee that parsers, imported files, or receiving software are safe.

## Repository structure

```text
crates/courierqr-core/   Transfer and cryptographic core
crates/courierqr-cli/    Command-line application
crates/courierqr-wasm/   WebAssembly integration
web/                    Web interface
DESIGN.md               Design documentation
```

## Contributing

Feedback is welcome on cryptographic design, offline-device isolation, metadata handling, recovery, QR performance, and shared-feed scalability.

Never include private keys, passwords, recovery seeds, or sensitive vault contents in issues.

## License

MIT
