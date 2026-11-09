//! Scrubbing real encoded images: metadata gone, pixels decode identically.

use std::io::Cursor;

fn photo() -> image::RgbImage {
    image::RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 5) as u8, 128])
    })
}

fn exif_app1() -> Vec<u8> {
    // Little-endian TIFF with Make + GPS pointer, like a phone writes.
    let mut t = b"II\x2a\0\x08\0\0\0".to_vec();
    t.extend_from_slice(&2u16.to_le_bytes());
    t.extend_from_slice(&[0x0F, 0x01, 2, 0, 6, 0, 0, 0]);
    t.extend_from_slice(&38u32.to_le_bytes());
    t.extend_from_slice(&[0x25, 0x88, 4, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
    t.extend_from_slice(&0u32.to_le_bytes());
    t.extend_from_slice(b"Apple\0");
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&t);
    seg
}

#[test]
fn real_jpeg_still_decodes_after_scrub() {
    let mut jpg = Vec::new();
    photo()
        .write_to(&mut Cursor::new(&mut jpg), image::ImageFormat::Jpeg)
        .unwrap();
    let mut tagged = jpg[..2].to_vec();
    tagged.extend(exif_app1());
    tagged.extend_from_slice(&[0xFF, 0xFE, 0, 12]);
    tagged.extend_from_slice(b"Fletch cam");
    tagged.extend_from_slice(&jpg[2..]);
    assert!(image::load_from_memory(&tagged).is_ok());

    let s = courierqr_core::scrub("IMG_0001.JPG", &tagged);
    assert!(s.report.cleaned, "{:?}", s.report);
    assert!(
        s.report.removed.iter().any(|r| r == "GPS location"),
        "{:?}",
        s.report
    );
    assert!(!s.data.windows(5).any(|w| w == b"Apple"));
    assert!(!s.data.windows(6).any(|w| w == b"Fletch"));
    let before = image::load_from_memory(&jpg).unwrap().to_rgb8();
    let after = image::load_from_memory(&s.data).unwrap().to_rgb8();
    assert_eq!(before, after, "pixels must be untouched");
}

#[test]
fn real_png_still_decodes_after_scrub() {
    let mut png = Vec::new();
    photo()
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    // Insert a tEXt chunk after IHDR (8-byte signature + 25-byte IHDR).
    let body = b"Author\0Hayden";
    let mut chunk = (body.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(body);
    let crc = crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    let mut tagged = png[..33].to_vec();
    tagged.extend(chunk);
    tagged.extend_from_slice(&png[33..]);
    assert!(image::load_from_memory(&tagged).is_ok());

    let s = courierqr_core::scrub("shot.png", &tagged);
    assert!(!s.data.windows(6).any(|w| w == b"Hayden"));
    assert_eq!(
        image::load_from_memory(&s.data).unwrap().to_rgb8(),
        photo(),
        "pixels must be untouched"
    );
}

fn crc32(d: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in d {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}
