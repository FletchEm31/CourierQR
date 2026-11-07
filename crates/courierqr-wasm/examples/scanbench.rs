//! Benchmark QR detection strategies on a camera recording (.y4m, 8-bit luma).
//!   cargo run --release -p courierqr-wasm --example scanbench -- capture.y4m
use std::collections::HashSet;
use std::time::Instant;

use rxing::common::{BitMatrix, DetectorRXingResult, GlobalHistogramBinarizer, HybridBinarizer};
use rxing::multi::qrcode::detector::MultiDetector;
use rxing::qrcode::decoder::qrcode_decoder::decode_bitmatrix_with_hints;
use rxing::qrcode::detector::Detector;
use rxing::{BarcodeFormat, BinaryBitmap, DecodeHints, Luma8LuminanceSource};

fn hints() -> DecodeHints {
    DecodeHints {
        PossibleFormats: Some(HashSet::from([BarcodeFormat::QR_CODE])),
        TryHarder: Some(true),
        ..Default::default()
    }
}
fn dec(bits: &BitMatrix, h: &DecodeHints) -> Option<Vec<u8>> {
    decode_bitmatrix_with_hints(bits, h)
        .ok()
        .map(|r| r.getByteSegments().concat())
}
fn multi(m: &BitMatrix, h: &DecodeHints) -> Vec<Vec<u8>> {
    MultiDetector::new(m)
        .detectMulti(h)
        .map(|v| v.iter().filter_map(|d| dec(d.getBits(), h)).collect())
        .unwrap_or_default()
}
fn single(m: &BitMatrix, h: &DecodeHints) -> Vec<Vec<u8>> {
    Detector::new(m)
        .detect_with_hints(h)
        .ok()
        .and_then(|d| dec(d.getBits(), h))
        .into_iter()
        .collect()
}
fn crop(p: &[u8], w: usize, x0: usize, y0: usize, cw: usize, ch: usize) -> Vec<u8> {
    let mut o = Vec::with_capacity(cw * ch);
    for y in y0..y0 + ch {
        o.extend_from_slice(&p[y * w + x0..y * w + x0 + cw]);
    }
    o
}
fn hybrid(p: Vec<u8>, w: usize, h: usize) -> BinaryBitmap<HybridBinarizer<Luma8LuminanceSource>> {
    BinaryBitmap::new(HybridBinarizer::new(
        Luma8LuminanceSource::new(p, w as u32, h as u32).unwrap(),
    ))
}

fn main() {
    let path = std::env::args().nth(1).expect("y4m path");
    let data = std::fs::read(path).unwrap();
    let nl = data.iter().position(|&b| b == b'\n').unwrap();
    let hdr = std::str::from_utf8(&data[..nl]).unwrap();
    let get = |k: char| {
        hdr.split(' ').find(|t| t.starts_with(k)).unwrap()[1..]
            .parse::<usize>()
            .unwrap()
    };
    let (w, h) = (get('W'), get('H'));
    let fsz = 6 + w * h + 2 * (w / 2) * (h / 2);
    let frames: Vec<&[u8]> = (0..)
        .map(|i| nl + 1 + i * fsz)
        .take_while(|o| o + fsz <= data.len())
        .map(|o| &data[o + 6..o + 6 + w * h])
        .collect();
    let hh = hints();

    type S = Box<dyn Fn(&[u8]) -> Vec<Vec<u8>>>;
    let strategies: Vec<(&str, S)> = vec![
        (
            "multi (hybrid)",
            Box::new(move |p| multi(hybrid(p.to_vec(), w, h).get_black_matrix(), &hints())),
        ),
        (
            "multi+single (current)",
            Box::new(move |p| {
                let b = hybrid(p.to_vec(), w, h);
                let m = b.get_black_matrix();
                let r = multi(m, &hints());
                if r.is_empty() { single(m, &hints()) } else { r }
            }),
        ),
        (
            "global-histogram multi",
            Box::new(move |p| {
                let b = BinaryBitmap::new(GlobalHistogramBinarizer::new(
                    Luma8LuminanceSource::new(p.to_vec(), w as u32, h as u32).unwrap(),
                ));
                multi(b.get_black_matrix(), &hints())
            }),
        ),
        (
            "3x3 overlapping tiles, single each",
            Box::new(move |p| {
                // tiles of half the frame, stepping by a quarter: any code up to ~half the frame fits whole in one tile
                let (tw, th) = (w / 2, h / 2);
                let mut out: Vec<Vec<u8>> = vec![];
                for ty in 0..3 {
                    for tx in 0..3 {
                        let (x0, y0) = (tx * w / 4, ty * h / 4);
                        let b = hybrid(crop(p, w, x0, y0, tw, th), tw, th);
                        for r in single(b.get_black_matrix(), &hints()) {
                            if !out.contains(&r) {
                                out.push(r)
                            }
                        }
                    }
                }
                out
            }),
        ),
        (
            "multi, then tiles if <2",
            Box::new(move |p| {
                let b = hybrid(p.to_vec(), w, h);
                let mut out = multi(b.get_black_matrix(), &hints());
                if out.len() < 2 {
                    let (tw, th) = (w / 2, h / 2);
                    for ty in 0..3 {
                        for tx in 0..3 {
                            let (x0, y0) = (tx * w / 4, ty * h / 4);
                            let bb = hybrid(crop(p, w, x0, y0, tw, th), tw, th);
                            for r in single(bb.get_black_matrix(), &hints()) {
                                if !out.contains(&r) {
                                    out.push(r)
                                }
                            }
                        }
                    }
                }
                out
            }),
        ),
    ];
    let _ = hh;
    println!("{} frames {}x{}", frames.len(), w, h);
    for (name, f) in &strategies {
        let t = Instant::now();
        let mut hist = [0usize; 5];
        let mut uniq = HashSet::new();
        for fr in &frames {
            let r = f(fr);
            hist[r.len().min(4)] += 1;
            for x in r {
                uniq.insert(x);
            }
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / frames.len() as f64;
        let total: usize = hist.iter().enumerate().map(|(i, c)| i * c).sum();
        println!(
            "{name:<36} found/frame {:.2}  frames with ≥1: {:>3}%  distinct codes {:>3}  {ms:>6.1} ms/frame",
            total as f64 / frames.len() as f64,
            100 * (frames.len() - hist[0]) / frames.len(),
            uniq.len()
        );
    }
}
