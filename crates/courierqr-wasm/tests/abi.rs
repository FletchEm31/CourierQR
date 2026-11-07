//! Drive the exact C ABI the web app uses (natively — same code as the
//! .wasm), with real QR codes rendered as grayscale "camera frames".

use courierqr_core::{FrameConfig, SealKey, Transmission};
use courierqr_wasm::*;
use qrcode::bits::Bits;
use qrcode::{Color, EcLevel, QrCode, Version};

const VERSION: i16 = 15;
const SCALE: usize = 4;
const PASS: &str = "abi-test-passphrase";

/// Render up to 4 frames as a 2x2 grid of byte-mode QR codes.
fn grid(frames: &[Vec<u8>]) -> (Vec<u8>, u32, u32) {
    let cell = (17 + 4 * VERSION as usize + 8) * SCALE;
    let (w, h) = (cell * 2, cell * 2);
    let mut px = vec![255u8; w * h];
    for (i, f) in frames.iter().enumerate() {
        let mut bits = Bits::new(Version::Normal(VERSION));
        bits.push_byte_data(f).unwrap();
        bits.push_terminator(EcLevel::L).unwrap();
        let code = QrCode::with_bits(bits, EcLevel::L).unwrap();
        let n = code.width();
        let colors = code.to_colors();
        let (ox, oy) = ((i % 2) * cell + 4 * SCALE, (i / 2) * cell + 4 * SCALE);
        for y in 0..n * SCALE {
            for x in 0..n * SCALE {
                if colors[(y / SCALE) * n + x / SCALE] == Color::Dark {
                    px[(oy + y) * w + ox + x] = 0;
                }
            }
        }
    }
    (px, w as u32, h as u32)
}

fn read(ptr: *const u8, len: usize) -> Vec<u8> {
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

#[test]
fn scan_open_and_take_file_through_the_c_abi() {
    let note = "# notes\n- offline\n- encrypted\n".repeat(300).into_bytes();
    let seal = SealKey::Passphrase {
        passphrase: PASS.to_string().into(),
        work_factor: 10,
    };
    let cfg = FrameConfig {
        max_frame_bytes: courierqr_core::qr_capacity::V15_L,
    };
    let tx = Transmission::new("abi.md", &note, &seal, cfg).unwrap();

    cq_reset();
    let mut frames = tx.frames();
    let mut images = 0;
    loop {
        images += 1;
        assert!(images < 40, "never completed");
        let batch: Vec<Vec<u8>> = frames.by_ref().take(4).collect();
        let (px, w, h) = grid(&batch);
        let st = unsafe { cq_scan(px.as_ptr(), w, h) };
        assert_eq!(
            cq_last_found(),
            4,
            "all four codes should be found in a clean grid"
        );
        if st == status::COMPLETE {
            break;
        }
        assert_eq!(st, status::PROGRESS);
    }
    assert_eq!(cq_is_complete(), 1);
    assert_eq!(cq_total_rejected(), 0);

    // Wrong passphrase: clean error, no file.
    let bad = b"nope";
    assert_eq!(
        unsafe { cq_open_passphrase(bad.as_ptr(), bad.len()) },
        open_status::DECRYPT
    );
    assert!(
        String::from_utf8(read(cq_error_ptr(), cq_error_len()))
            .unwrap()
            .contains("decryption failed")
    );
    assert_eq!(cq_file_data_len(), 0);

    // Right passphrase.
    assert_eq!(
        unsafe { cq_open_passphrase(PASS.as_ptr(), PASS.len()) },
        open_status::OK
    );
    assert_eq!(read(cq_file_name_ptr(), cq_file_name_len()), b"abi.md");
    assert_eq!(read(cq_file_data_ptr(), cq_file_data_len()), note);

    cq_forget_file();
    assert_eq!(cq_file_data_len(), 0);
}

#[test]
fn alloc_free_and_garbage_input() {
    cq_reset();
    let p = cq_alloc(1024);
    assert!(!p.is_null());
    unsafe { cq_free(p, 1024) };

    // Random noise "camera frame": nothing found, nothing breaks.
    let noise: Vec<u8> = (0..320 * 240u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    assert_eq!(
        unsafe { cq_scan(noise.as_ptr(), 320, 240) },
        status::NOTHING
    );
    // Truncated buffer claims (w*h larger than data) are rejected, not read.
    assert_eq!(unsafe { cq_scan(noise.as_ptr(), 0, 0) }, status::NOTHING);
    // Opening before completion.
    assert_eq!(
        unsafe { cq_open_passphrase(b"x".as_ptr(), 1) },
        open_status::NOT_COMPLETE
    );
    // A non-CourierQR payload pushed directly.
    let other = b"https://example.com";
    assert_eq!(
        unsafe { cq_push(other.as_ptr(), other.len()) },
        status::NOT_COURIERQR
    );
}

#[test]
fn pairing_keygen_fingerprint_and_qr() {
    cq_reset();
    cq_generate_keys();
    let public = String::from_utf8(read(cq_key_public_ptr(), cq_key_public_len())).unwrap();
    let secret = String::from_utf8(read(cq_key_secret_ptr(), cq_key_secret_len())).unwrap();
    assert!(public.starts_with("age1"));
    assert!(secret.starts_with("AGE-SECRET-KEY-1"));
    cq_forget_keys();
    assert_eq!(cq_key_secret_len(), 0);

    // Same fingerprint as the core (and therefore as the CLI).
    assert_eq!(unsafe { cq_fingerprint(public.as_ptr(), public.len()) }, 0);
    let fp = String::from_utf8(read(cq_text_ptr(), cq_text_len())).unwrap();
    assert_eq!(fp, courierqr_core::fingerprint(&public).unwrap());
    assert_eq!(
        unsafe { cq_fingerprint(b"age1nope".as_ptr(), 8) },
        open_status::BAD_KEY
    );

    // The public-key QR decodes back to the key with our own scanner.
    let w = unsafe { cq_qr_encode(public.as_ptr(), public.len()) } as usize;
    assert!(w >= 21);
    let modules = read(cq_qr_ptr(), w * w);
    let scale = 4;
    let side = (w + 8) * scale;
    let mut px = vec![255u8; side * side];
    for y in 0..w * scale {
        for x in 0..w * scale {
            if modules[(y / scale) * w + x / scale] == 1 {
                px[(y + 4 * scale) * side + x + 4 * scale] = 0;
            }
        }
    }
    assert_eq!(
        courierqr_wasm::scan::decode_all(&px, side as u32, side as u32),
        vec![public.into_bytes()]
    );
    let _ = secret;
}
