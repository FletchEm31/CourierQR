//! Find every QR code in a camera frame and return each payload's raw bytes.
//!
//! We drive rxing's QR *detector* and *decoder* directly instead of its
//! high-level readers. Two reasons:
//!
//! 1. The high-level readers wrap results in `RXingResult`, which stamps
//!    the current time via `chrono` → `SystemTime::now()` — and that call
//!    panics on `wasm32-unknown-unknown` (there's no clock without JS glue).
//! 2. The decoder result hands us the byte-mode payload directly. We never
//!    touch `getText()`, which charset-guesses binary into garbage.
//!
//! `MultiDetector` finds several codes per frame (the 2×2 grid); if it
//! finds none, the single-code `Detector` (more tolerant of distortion)
//! gets a try.

use std::collections::HashSet;

use rxing::common::{BitMatrix, DecoderRXingResult, DetectorRXingResult, HybridBinarizer};
use rxing::multi::qrcode::detector::MultiDetector;
use rxing::qrcode::decoder::qrcode_decoder::decode_bitmatrix_with_hints;
use rxing::qrcode::detector::Detector;
use rxing::{BarcodeFormat, BinaryBitmap, DecodeHints, Luma8LuminanceSource};

/// All QR payloads in an 8-bit grayscale image (row-major, `w*h` bytes).
pub fn decode_all(pixels: &[u8], w: u32, h: u32) -> Vec<Vec<u8>> {
    let n = (w as usize) * (h as usize);
    if n == 0 || pixels.len() < n {
        return Vec::new();
    }
    let Ok(src) = Luma8LuminanceSource::new(pixels[..n].to_vec(), w, h) else {
        return Vec::new();
    };
    let bitmap = BinaryBitmap::new(HybridBinarizer::new(src));
    let matrix = bitmap.get_black_matrix();

    let hints = DecodeHints {
        PossibleFormats: Some(HashSet::from([BarcodeFormat::QR_CODE])),
        TryHarder: Some(true),
        ..Default::default()
    };

    let mut out: Vec<Vec<u8>> = MultiDetector::new(matrix)
        .detectMulti(&hints)
        .map(|found| {
            found
                .iter()
                .filter_map(|d| decode(d.getBits(), &hints))
                .collect()
        })
        .unwrap_or_default();

    if out.is_empty()
        && let Ok(d) = Detector::new(matrix).detect_with_hints(&hints)
        && let Some(bytes) = decode(d.getBits(), &hints)
    {
        out.push(bytes);
    }
    out.dedup();
    out
}

fn decode(bits: &BitMatrix, hints: &DecodeHints) -> Option<Vec<u8>> {
    decode_bitmatrix_with_hints(bits, hints)
        .ok()
        .and_then(|r| payload(&r))
}

fn payload(r: &DecoderRXingResult) -> Option<Vec<u8>> {
    let segs = r.getByteSegments();
    if !segs.is_empty() {
        return Some(segs.concat());
    }
    let raw = r.getRawBytes();
    (!raw.is_empty()).then(|| raw.clone())
}
