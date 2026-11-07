//! Find and decode every QR code in a grayscale image, returning raw bytes.
//!
//! Thin wrapper over `courierqr_core::scan` — the exact detector the phone
//! runs as WebAssembly, so desktop and phone behave the same.

/// All QR payloads found in the image (possibly none).
pub fn decode_all(pixels: &[u8], width: usize, height: usize) -> Vec<Vec<u8>> {
    courierqr_core::scan::decode_all(pixels, width as u32, height as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Grid, Layout, Luma};
    use courierqr_core::{Accepted, FrameConfig, OpenKey, Receiver, SealKey, Transmission};

    /// Deterministic noise source.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Crude "camera": 3x3 box blur, lowered contrast, and ±noise.
    fn degrade(img: &Luma, seed: u64) -> Vec<u8> {
        let (w, h) = (img.width, img.height);
        let mut rng = XorShift(seed);
        let mut out = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0u32;
                let mut n = 0u32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                        if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h {
                            sum += img.pixels[yy as usize * w + xx as usize] as u32;
                            n += 1;
                        }
                    }
                }
                // Map 0..255 to 40..215 (washed-out screen), then add noise.
                let v = 40 + (sum / n) as i32 * 175 / 255 + (rng.next() % 61) as i32 - 30;
                out[y * w + x] = v.clamp(0, 255) as u8;
            }
        }
        out
    }

    #[test]
    fn every_byte_value_survives_a_real_qr_roundtrip() {
        let layout = Layout::fit(25, Grid::from_count(1).unwrap(), 600, 600).unwrap();
        let cap = layout.frame_bytes();
        // All 256 byte values, including 0x00 and invalid UTF-8, at full capacity.
        let data: Vec<u8> = (0..cap).map(|i| (i * 7 + 3) as u8).collect();
        let img = layout.render(std::slice::from_ref(&data)).unwrap();
        let found = decode_all(&img.pixels, img.width, img.height);
        assert_eq!(found, vec![data]);
    }

    #[test]
    fn finds_all_four_codes_in_a_2x2_grid() {
        let layout = Layout::fit(20, Grid::from_count(4).unwrap(), 1000, 1000).unwrap();
        let cap = layout.frame_bytes();
        let frames: Vec<Vec<u8>> = (0..4u8).map(|k| vec![k.wrapping_mul(61); cap]).collect();
        let img = layout.render(&frames).unwrap();
        let mut found = decode_all(&img.pixels, img.width, img.height);
        found.sort();
        let mut want = frames.clone();
        want.sort();
        assert_eq!(found, want);
    }

    /// The whole optical path except the physical camera: core frames ->
    /// QR pixels in a 2x2 grid -> blur/noise -> QR decode -> Receiver -> file.
    #[test]
    fn full_transfer_through_degraded_qr_images() {
        let note: Vec<u8> = "## Optical test\n- compress\n- encrypt\n- fountain\n"
            .repeat(400)
            .into_bytes();
        let layout = Layout::fit(20, Grid::from_count(4).unwrap(), 1000, 1000).unwrap();
        let key = SealKey::Passphrase {
            passphrase: "optical-test-pass".to_string().into(),
            work_factor: 10,
        };
        let tx = Transmission::new(
            "optical.md",
            &note,
            &key,
            FrameConfig {
                max_frame_bytes: layout.frame_bytes(),
            },
        )
        .unwrap();

        let mut rx = Receiver::new();
        let mut stream = tx.frames();
        let mut images = 0;
        while !rx.is_complete() {
            images += 1;
            assert!(images < 50, "no completion after {images} images");
            let frames: Vec<Vec<u8>> = stream.by_ref().take(4).collect();
            let img = layout.render(&frames).unwrap();
            let pixels = degrade(&img, images);
            for payload in decode_all(&pixels, img.width, img.height) {
                if rx.push(&payload) == Ok(Accepted::Complete) {
                    break;
                }
            }
        }
        let file = rx
            .open(&OpenKey::passphrase("optical-test-pass"))
            .unwrap()
            .unwrap();
        assert_eq!(file.name, "optical.md");
        assert_eq!(file.data, note);
    }
}
