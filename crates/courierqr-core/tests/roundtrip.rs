//! End-to-end tests of the optical pipeline with the camera simulated as a
//! lossy, shuffling, occasionally-corrupting channel.

use courierqr_core::{
    Accepted, Error, FrameConfig, FrameError, Method, OpenKey, Receiver, SealKey, Transmission,
    generate_device_keys, qr_capacity, secrecy::ExposeSecret,
};

const PASS: &str = "tangerine-quokka-lighthouse-47";

/// Tiny deterministic PRNG so tests are reproducible without extra deps.
struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Passphrase key with a cheap scrypt cost so tests stay fast.
fn fast_pass() -> SealKey {
    SealKey::Passphrase {
        passphrase: PASS.to_string().into(),
        work_factor: 10,
    }
}

fn markdown_note(len: usize) -> Vec<u8> {
    let para = "## Field notes\n\n- RaptorQ is order independent\n- compress, then encrypt\n\n";
    para.repeat(len / para.len() + 1).into_bytes()[..len].to_vec()
}

/// Simulated camera: drops `loss_pct`% of frames, corrupts `corrupt_pct`%,
/// sometimes sees a frame twice, and reorders within a small window.
/// Returns (frames shown, final receiver).
fn transmit(
    tx: &Transmission,
    seed: u64,
    loss_pct: u64,
    corrupt_pct: u64,
    max_frames: usize,
) -> (usize, Receiver) {
    let mut rng = XorShift(seed);
    let mut rx = Receiver::new();
    let mut window: Vec<Vec<u8>> = Vec::new();

    for (shown, frame) in tx.frames().take(max_frames).enumerate() {
        if rng.chance(loss_pct) {
            continue;
        }
        let mut f = frame;
        if rng.chance(corrupt_pct) {
            let i = (rng.next() as usize) % f.len();
            f[i] ^= 1 << (rng.next() % 8);
        }
        if rng.chance(5) {
            window.push(f.clone()); // double-read
        }
        window.push(f);
        if window.len() >= 4 {
            let idx = (rng.next() as usize) % window.len();
            let pick = window.swap_remove(idx);
            if let Ok(Accepted::Complete) = rx.push(&pick) {
                return (shown + 1, rx);
            }
        }
    }
    for f in window {
        if let Ok(Accepted::Complete) = rx.push(&f) {
            break;
        }
    }
    (max_frames, rx)
}

#[test]
fn clean_channel_needs_about_source_frames() {
    let data = markdown_note(50_000);
    let tx = Transmission::new(
        "notes/field.md",
        &data,
        &fast_pass(),
        FrameConfig::default(),
    )
    .unwrap();

    let mut rx = Receiver::new();
    let mut used = 0;
    for f in tx.frames() {
        used += 1;
        if rx.push(&f).unwrap() == Accepted::Complete {
            break;
        }
    }
    // Systematic code: on a perfect channel the source frames are enough.
    assert_eq!(used, tx.source_frames());
    // Compression should have made this tiny compared with the raw note.
    assert!(
        tx.ciphertext_len() < data.len() / 10,
        "ct={}",
        tx.ciphertext_len()
    );

    let file = rx.open(&OpenKey::passphrase(PASS)).unwrap().unwrap();
    // Folders are stripped from names before encryption.
    assert_eq!(file.name, "field.md");
    assert_eq!(file.data, data);
}

#[test]
fn survives_heavy_loss_shuffle_duplicates_and_corruption() {
    let mut rng = XorShift(7);
    let data = rng.bytes(200_000); // incompressible, ~160 frames at V25-L
    let tx = Transmission::new("photo.jpg", &data, &fast_pass(), FrameConfig::default()).unwrap();
    let k = tx.source_frames();

    for seed in 1..=5 {
        let (shown, rx) = transmit(&tx, seed, 40, 10, k * 4);
        assert!(
            rx.is_complete(),
            "seed {seed}: incomplete after {shown} frames"
        );
        // With 40% loss + 10% corruption we expect ~2x K shown; give headroom.
        assert!(
            shown < k * 3,
            "seed {seed}: needed {shown} frames for K={k}"
        );
        let file = rx.open(&OpenKey::passphrase(PASS)).unwrap().unwrap();
        assert_eq!(file.data, data, "seed {seed}");
    }
}

#[test]
fn corrupted_frames_are_rejected_not_absorbed() {
    let tx = Transmission::new("a.txt", b"hello", &fast_pass(), FrameConfig::default()).unwrap();
    let mut frame = tx.frames().next().unwrap();
    let last = frame.len() - 1;
    frame[last] ^= 0xFF;
    let mut rx = Receiver::new();
    assert_eq!(rx.push(&frame), Err(FrameError::Checksum));
    assert_eq!(rx.progress().received, 0);
    assert_eq!(rx.push(b"WIFI:S:cafe;;"), Err(FrameError::NotCourierQr));
}

#[test]
fn paired_device_keys() {
    let keys = generate_device_keys();
    assert!(keys.public.starts_with("age1"));
    let data = markdown_note(3_000);
    let tx = Transmission::new(
        "todo.md",
        &data,
        &SealKey::recipient(keys.public.clone()),
        FrameConfig::default(),
    )
    .unwrap();
    let (_, rx) = transmit(&tx, 3, 20, 0, 500);
    assert!(rx.is_complete());

    let ok = rx
        .open(&OpenKey::identity(keys.secret.expose_secret()))
        .unwrap()
        .unwrap();
    assert_eq!(ok.data, data);

    // Each method on its own: post-quantum carries ~1.4 KB more header.
    let mut sizes = vec![];
    for method in [Method::PostQuantum, Method::Classic] {
        let tx = Transmission::new(
            "todo.md",
            &data,
            &SealKey::for_device(&keys.public, method).unwrap(),
            FrameConfig::default(),
        )
        .unwrap();
        sizes.push(tx.ciphertext_len());
        let (_, rx) = transmit(&tx, 3, 20, 0, 500);
        let got = rx
            .open(&OpenKey::identity(keys.secret.expose_secret()))
            .unwrap()
            .unwrap();
        assert_eq!(got.data, data, "{method:?}");
    }
    assert!(
        sizes[0] > sizes[1] + 1300 && sizes[0] < sizes[1] + 1800,
        "{sizes:?}"
    );

    // A device paired before post-quantum keys existed (classic only)
    // still works, and asking it for post-quantum is a clear error.
    let classic_only = keys.public.split(' ').next().unwrap();
    assert!(SealKey::for_device(classic_only, Method::Classic).is_ok());
    assert!(SealKey::for_device(classic_only, Method::PostQuantum).is_err());

    // A different device's key can't open it.
    let other = generate_device_keys();
    let err = rx
        .open(&OpenKey::identity(other.secret.expose_secret()))
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, Error::Decrypt(_)), "{err:?}");
}

#[test]
fn wrong_passphrase_fails_cleanly() {
    let tx = Transmission::new("s.txt", b"secret", &fast_pass(), FrameConfig::default()).unwrap();
    let mut rx = Receiver::new();
    for f in tx.frames() {
        if rx.push(&f).unwrap() == Accepted::Complete {
            break;
        }
    }
    let err = rx.open(&OpenKey::passphrase("nope")).unwrap().unwrap_err();
    assert!(matches!(err, Error::Decrypt(_)), "{err:?}");
}

#[test]
fn locks_onto_first_transfer_and_ignores_others() {
    let a = Transmission::new(
        "a.md",
        &markdown_note(20_000),
        &fast_pass(),
        FrameConfig::default(),
    )
    .unwrap();
    let b = Transmission::new(
        "b.md",
        &XorShift(5).bytes(8_000),
        &fast_pass(),
        FrameConfig::default(),
    )
    .unwrap();
    assert_ne!(a.transfer_id(), b.transfer_id());

    let mut rx = Receiver::new();
    let mut fb = b.frames();
    let mut saw_other = false;
    for f in a.frames() {
        // Another sender's frames keep sneaking into view.
        rx.push(&fb.next().unwrap()).unwrap();
        if rx.push(&f).unwrap() == Accepted::OtherTransfer {
            saw_other = true;
        }
        if rx.is_complete() {
            break;
        }
    }
    assert!(
        saw_other,
        "a's frames should have been flagged as another transfer"
    );
    // The very first frame pushed was b's, so we actually locked onto b...
    assert_eq!(rx.transfer_id(), Some(b.transfer_id()));
    let file = rx.open(&OpenKey::passphrase(PASS)).unwrap().unwrap();
    assert_eq!(file.name, "b.md");

    // ...and after a reset we pick up a cleanly.
    rx.reset();
    for f in a.frames() {
        if rx.push(&f).unwrap() == Accepted::Complete {
            break;
        }
    }
    assert_eq!(
        rx.open(&OpenKey::passphrase(PASS)).unwrap().unwrap().name,
        "a.md"
    );
}

#[test]
fn frames_fit_every_qr_budget() {
    let data = markdown_note(30_000);
    for cap in [
        qr_capacity::V15_L,
        qr_capacity::V20_L,
        qr_capacity::V25_L,
        qr_capacity::V30_L,
        qr_capacity::V40_L,
    ] {
        let tx = Transmission::new(
            "n.md",
            &data,
            &fast_pass(),
            FrameConfig {
                max_frame_bytes: cap,
            },
        )
        .unwrap();
        for f in tx.frames().take(tx.source_frames() + 100) {
            assert!(f.len() <= cap, "frame {} > cap {cap}", f.len());
        }
    }
    assert!(matches!(
        Transmission::new(
            "n.md",
            &data,
            &fast_pass(),
            FrameConfig {
                max_frame_bytes: 40
            }
        ),
        Err(Error::FrameBudgetTooSmall(40))
    ));
}

#[test]
fn empty_file_and_long_name_limits() {
    let tx = Transmission::new("empty", b"", &fast_pass(), FrameConfig::default()).unwrap();
    let (_, rx) = transmit(&tx, 9, 0, 0, 50);
    assert_eq!(
        rx.open(&OpenKey::passphrase(PASS)).unwrap().unwrap().data,
        b""
    );

    let long = "x".repeat(300);
    assert!(matches!(
        Transmission::new(&long, b"", &fast_pass(), FrameConfig::default()),
        Err(Error::NameTooLong(300))
    ));
}

#[test]
fn progress_reporting() {
    let tx = Transmission::new(
        "p.bin",
        &XorShift(1).bytes(20_000),
        &fast_pass(),
        FrameConfig::default(),
    )
    .unwrap();
    let mut rx = Receiver::new();
    assert_eq!(rx.progress().fraction(), 0.0);
    let mut frames = tx.frames();
    rx.push(&frames.next().unwrap()).unwrap();
    let p = rx.progress();
    assert_eq!(p.received, 1);
    assert_eq!(p.needed, tx.source_frames());
    assert!(p.fraction() > 0.0 && p.fraction() < 1.0);
    for f in frames {
        if rx.push(&f).unwrap() == Accepted::Complete {
            break;
        }
    }
    assert_eq!(rx.progress().fraction(), 1.0);
}

/// Over the 10 MB-per-block default, RaptorQ splits into multiple source
/// blocks; make sure interleaving and multi-block decode work.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "slow unoptimized; run with `cargo test --release`"
)]
fn multi_block_transfer() {
    let data = XorShift(42).bytes(12 * 1024 * 1024);
    let tx = Transmission::new(
        "big.bin",
        &data,
        &fast_pass(),
        FrameConfig {
            max_frame_bytes: qr_capacity::V40_L,
        },
    )
    .unwrap();
    let (shown, rx) = transmit(&tx, 11, 15, 0, tx.source_frames() * 2);
    assert!(rx.is_complete(), "incomplete after {shown}");
    assert_eq!(
        rx.open(&OpenKey::passphrase(PASS)).unwrap().unwrap().data,
        data
    );
}
