# CourierQR: notes for Claude Code

Encrypted messaging and file transfer through animated QR codes ("a cold wallet for messaging"): an offline device
keeps its keys and decrypts locally; an online gateway only carries ciphertext. Rust workspace, public repo.
Owner: Fletch (GitHub `FletchEm31`).

## Layout
- `crates/courierqr-core/`: transfer, cryptography, accounts, feed playlist
- `crates/courierqr-cli/`: command-line app and terminal UI (gateway)
- `crates/courierqr-wasm/`: WebAssembly build of the core for the phone web app
- `web/`: phone web app (PWA), published to GitHub Pages by `.github/workflows/`
- `DESIGN.md`: the technical design (source of truth for protocol and crypto); `docs/design/`: UI mockups

## Rules
- Commit and push straight to `main`: no branches, no pull requests.
- **Crypto and protocol changes:** say what changes and why before writing code, and update `DESIGN.md` in the same
  commit. No new crypto primitives or hand-rolled crypto.
- Never put keys, passwords, recovery seeds, account numbers or vault contents in code, tests, logs, issues or docs.
- The README is the public face of the project: keep its "built today / in development" status honest.

## Build and test
- `cargo build`, `cargo test --workspace` (deps are optimized even in dev because scrypt is slow unoptimized)
- Install the app: `cargo install --path crates/courierqr-cli` (headless: add `--no-default-features`)
- CI (`.github/workflows/pages.yml`) runs `cargo test --workspace --release`, builds the web app and publishes it on every push to `main`.
- Camera and screen testing happens on the owner's machines; a cloud session can't run it.

## Tasks and handoffs live in the Obsidian vault
Tasks, session handoffs, the roadmap, decisions and open questions live in the owner's Obsidian vault on the desktop,
`~/Obsidian/BlackholeNetwork-Notes/Projects/CourierQR/`, not in this repo:
- Before starting: read the vault's `CLAUDE.md`, then `Projects/CourierQR/Tasks.md` and the newest note in
  `Projects/CourierQR/Handoffs/`.
- At the end: update that `Tasks.md` and write a handoff there (format in the vault's `CLAUDE.md`). Don't add
  session records or to-do lists to this repo.
- From a cloud session, reach the vault through the desktop bridge. No vault access: end the reply with the Tasks
  changes and the handoff in markdown so the owner can paste them in.
- The rules in this file win for work in this repo.
