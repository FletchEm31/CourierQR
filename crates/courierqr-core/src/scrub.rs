//! Metadata scrubbing, run automatically before anything is encrypted.
//!
//! Encryption hides content from people watching the screen; this hides
//! the *incidental* data inside the content from whoever receives it:
//! GPS coordinates, camera make/model and capture time in photos,
//! location and creation dates in videos, and invisible characters in
//! text (which can carry a hidden per-copy fingerprint).
//!
//! Everything here edits containers without re-encoding pixels or video,
//! so quality is untouched. Formats we can't clean are sent unchanged and
//! reported as such, so the sender always knows.

/// What scrubbing found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubReport {
    /// Detected kind, e.g. "JPEG photo", "text".
    pub kind: &'static str,
    /// Human-readable items that were removed (empty = nothing found).
    pub removed: Vec<String>,
    /// False when the format isn't one we know how to clean: the bytes
    /// were sent unchanged.
    pub cleaned: bool,
}

impl ScrubReport {
    /// One line for a status bar.
    pub fn summary(&self) -> String {
        if !self.cleaned {
            return format!(
                "{}: metadata NOT removed (format not supported), sent as-is",
                self.kind
            );
        }
        if self.removed.is_empty() {
            format!("{}: no metadata found", self.kind)
        } else {
            format!("{}: removed {}", self.kind, self.removed.join(", "))
        }
    }
}

/// A file after scrubbing.
#[derive(Debug, Clone)]
pub struct Scrubbed {
    pub name: String,
    pub data: Vec<u8>,
    pub report: ScrubReport,
}

/// Strip metadata from `data` (and folders/control characters from `name`).
pub fn scrub(name: &str, data: &[u8]) -> Scrubbed {
    let mut report = ScrubReport {
        cleaned: true,
        ..Default::default()
    };
    let clean_name = scrub_name(name);
    if clean_name != name {
        report.removed.push("folder path from the file name".into());
    }

    let out = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        report.kind = "JPEG photo";
        jpeg(data, &mut report.removed)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        report.kind = "PNG image";
        png(data, &mut report.removed)
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        report.kind = "WebP image";
        webp(data, &mut report.removed)
    } else if let Some(kind) = isobmff_kind(data) {
        report.kind = kind;
        if kind == "HEIC/AVIF image" {
            None
        } else {
            isobmff(data, &mut report.removed)
        }
    } else if let Some(text) = as_text(data) {
        report.kind = "text";
        Some(scrub_text(text, &mut report.removed).into_bytes())
    } else {
        report.kind = "file";
        None
    };

    let data = match out {
        Some(d) => d,
        None => {
            report.cleaned = false;
            report.removed.clear();
            data.to_vec()
        }
    };
    Scrubbed {
        name: clean_name,
        data,
        report,
    }
}

// ------------------------------------------------------------------ names

fn scrub_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let base: String = base.chars().filter(|c| !c.is_control()).collect();
    let base = base.trim();
    if base.is_empty() || base == "." || base == ".." {
        "file".into()
    } else {
        base.into()
    }
}

// ------------------------------------------------------------------ text

fn as_text(data: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(data).ok()?;
    (!s.contains('\0')).then_some(s)
}

/// Characters that render as nothing (or as an ordinary space) but differ
/// byte-wise: the classic way to watermark copies of a message.
fn scrub_text(s: &str, removed: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut invisible, mut spaces, mut bom) = (0usize, 0usize, false);
    let mut prev: Option<char> = None;
    let mut in_flag_tag = false;
    for c in s.chars() {
        let u = c as u32;
        match u {
            0xFEFF => bom = true,
            // zero-width space, word joiner & invisible operators,
            // Mongolian vowel separator, bidi embeddings/overrides/isolates
            0x200B | 0x2060..=0x2064 | 0x180E | 0x202A..=0x202E | 0x2066..=0x2069 => invisible += 1,
            // supplementary variation selectors: no visible use, popular for
            // smuggling bytes
            0xE0100..=0xE01EF => invisible += 1,
            // standard variation selectors: one after a visible character is
            // legitimate (emoji vs text style), repeats are not
            0xFE00..=0xFE0F if prev.is_some_and(|p| (0xFE00..=0xFE0F).contains(&(p as u32))) => {
                invisible += 1
            }
            // tag characters: legitimate only inside a 🏴 subdivision flag
            0xE0020..=0xE007F if !in_flag_tag => invisible += 1,
            // odd-width spaces that look like a normal space
            0x2000..=0x200A | 0x205F => {
                spaces += 1;
                out.push(' ');
            }
            _ => out.push(c),
        }
        if u == 0x1F3F4 {
            in_flag_tag = true;
        } else if !(0xE0020..=0xE007F).contains(&u) {
            in_flag_tag = false;
        }
        prev = Some(c);
    }
    if bom {
        removed.push("byte-order mark".into());
    }
    if invisible > 0 {
        removed.push(format!(
            "{invisible} invisible character{}",
            if invisible == 1 { "" } else { "s" }
        ));
    }
    if spaces > 0 {
        removed.push(format!(
            "{spaces} unusual space{} (made normal)",
            if spaces == 1 { "" } else { "s" }
        ));
    }
    out
}

// ------------------------------------------------------------------ JPEG

/// Drop every APPn/COM segment except the ones needed to show the image
/// correctly (JFIF, ICC colour profile, Adobe colour transform), keep only
/// the orientation from EXIF, and cut anything after the image ends
/// (iPhone depth maps / gain maps carry their own EXIF there).
#[allow(clippy::match_overlapping_arm)] // specific APPn first, then "any other APPn"
fn jpeg(d: &[u8], removed: &mut Vec<String>) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(d.len());
    out.extend_from_slice(&d[..2]);
    let orient_at = out.len();
    let mut orientation = 1u16;
    let mut found: Vec<&'static str> = Vec::new();
    let mut note = |s: &'static str| {
        if !found.contains(&s) {
            found.push(s);
        }
    };
    let mut i = 2;
    loop {
        // find next marker
        if i + 1 >= d.len() || d[i] != 0xFF {
            return None;
        }
        while i < d.len() && d[i] == 0xFF {
            i += 1;
        }
        let m = *d.get(i)?;
        i += 1;
        match m {
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                if i < d.len() {
                    note("extra images after the photo (depth/gain maps)");
                }
                break;
            }
            0x01 | 0xD0..=0xD7 => {
                out.extend_from_slice(&[0xFF, m]);
                continue;
            }
            _ => {}
        }
        let len = u16::from_be_bytes([*d.get(i)?, *d.get(i + 1)?]) as usize;
        if len < 2 || i + len > d.len() {
            return None;
        }
        let seg = &d[i - 2..i + len];
        let body = &d[i + 2..i + len];
        i += len;
        let keep = match m {
            0xE0 => body.starts_with(b"JFIF\0") || body.starts_with(b"JFXX\0"),
            0xE2 if body.starts_with(b"MPF\0") => {
                note("multi-picture index");
                false
            }
            0xE2 => body.starts_with(b"ICC_PROFILE\0"),
            0xEE => body.starts_with(b"Adobe"),
            0xE1 if body.starts_with(b"Exif\0\0") => {
                let t = exif_scan(&body[6..]);
                orientation = t.orientation;
                if t.gps {
                    note("GPS location");
                }
                if t.camera {
                    note("camera make/model");
                }
                if t.time {
                    note("date/time taken");
                }
                if t.other {
                    note("other EXIF data");
                }
                false
            }
            0xE1 => {
                note("XMP data");
                false
            }
            0xED => {
                note("IPTC/Photoshop data");
                false
            }
            0xFE => {
                note("comment");
                false
            }
            0xE0..=0xEF => {
                note("other app data");
                false
            }
            _ => true,
        };
        if keep {
            out.extend_from_slice(seg);
        }
        if m == 0xDA {
            // Entropy-coded data: copy until the next real marker.
            let start = i;
            while i + 1 < d.len()
                && !(d[i] == 0xFF && d[i + 1] != 0x00 && !(0xD0..=0xD7).contains(&d[i + 1]))
            {
                i += 1;
            }
            if i + 1 >= d.len() {
                return None;
            }
            out.extend_from_slice(&d[start..i]);
        }
    }
    if orientation != 1 {
        // Keep the photo upright: a minimal EXIF holding only orientation.
        let mut app1 = vec![0xFF, 0xE1, 0, 34];
        app1.extend_from_slice(b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01");
        app1.extend_from_slice(&orientation.to_be_bytes());
        app1.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        out.splice(orient_at..orient_at, app1);
    }
    removed.extend(found.into_iter().map(String::from));
    Some(out)
}

#[derive(Default)]
struct ExifTags {
    orientation: u16,
    gps: bool,
    camera: bool,
    time: bool,
    other: bool,
}

/// Read just enough of a TIFF/EXIF block to report what's in it.
fn exif_scan(t: &[u8]) -> ExifTags {
    let mut r = ExifTags {
        orientation: 1,
        ..Default::default()
    };
    let le = match t.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => {
            r.other = true;
            return r;
        }
    };
    let u16_at = |o: usize| {
        t.get(o..o + 2).map(|b| {
            if le {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            }
        })
    };
    let u32_at = |o: usize| {
        t.get(o..o + 4).map(|b| {
            let a = [b[0], b[1], b[2], b[3]];
            if le {
                u32::from_le_bytes(a)
            } else {
                u32::from_be_bytes(a)
            }
        })
    };
    let mut ifds = vec![u32_at(4).unwrap_or(0) as usize];
    let mut seen = 0;
    while let Some(off) = ifds.pop() {
        seen += 1;
        if off == 0 || seen > 8 {
            continue;
        }
        let Some(n) = u16_at(off) else { continue };
        for e in 0..n.min(512) as usize {
            let at = off + 2 + e * 12;
            let Some(tag) = u16_at(at) else { break };
            match tag {
                0x0112 => {
                    r.orientation = u16_at(at + 8).filter(|v| (1..=8).contains(v)).unwrap_or(1)
                }
                0x8825 => r.gps = true,
                0x010F | 0x0110 | 0xA431 | 0xA434 | 0xA435 => r.camera = true,
                0x0132 | 0x9003 | 0x9004 => r.time = true,
                0x8769 => {
                    if let Some(sub) = u32_at(at + 8) {
                        ifds.push(sub as usize);
                    }
                }
                _ => r.other = true,
            }
        }
        if let Some(next) = u32_at(off + 2 + n as usize * 12) {
            ifds.push(next as usize);
        }
    }
    r
}

// ------------------------------------------------------------------ PNG

fn png(d: &[u8], removed: &mut Vec<String>) -> Option<Vec<u8>> {
    const KEEP: [&[u8; 4]; 18] = [
        b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT",
        b"bKGD", b"hIST", b"pHYs", b"acTL", b"fcTL", b"fdAT", b"cICP", b"cLLi",
    ];
    let mut out = d[..8].to_vec();
    let mut found: Vec<&'static str> = Vec::new();
    let mut i = 8;
    while i < d.len() {
        let len = u32::from_be_bytes(d.get(i..i + 4)?.try_into().ok()?) as usize;
        let end = i.checked_add(12)?.checked_add(len)?;
        let ty: &[u8; 4] = d.get(i + 4..i + 8)?.try_into().ok()?;
        if end > d.len() {
            return None;
        }
        if KEEP.contains(&ty) {
            out.extend_from_slice(&d[i..end]);
        } else {
            let what = match ty {
                b"tEXt" | b"zTXt" | b"iTXt" => "text fields (author, software, XMP)",
                b"tIME" => "timestamp",
                b"eXIf" => "EXIF data",
                _ => "other ancillary chunks",
            };
            if !found.contains(&what) {
                found.push(what);
            }
        }
        i = end;
        if ty == b"IEND" {
            break;
        }
    }
    removed.extend(found.into_iter().map(String::from));
    Some(out)
}

// ------------------------------------------------------------------ WebP

fn webp(d: &[u8], removed: &mut Vec<String>) -> Option<Vec<u8>> {
    const KEEP: [&[u8; 4]; 7] = [
        b"VP8 ", b"VP8L", b"VP8X", b"ALPH", b"ANIM", b"ANMF", b"ICCP",
    ];
    let riff_end = (u32::from_le_bytes(d[4..8].try_into().ok()?) as usize + 8).min(d.len());
    let mut out = d[..12].to_vec();
    let mut i = 12;
    while i + 8 <= riff_end {
        let ty: &[u8; 4] = d[i..i + 4].try_into().ok()?;
        let len = u32::from_le_bytes(d[i + 4..i + 8].try_into().ok()?) as usize;
        let end = i + 8 + len + (len & 1);
        if i + 8 + len > d.len() {
            return None;
        }
        let end = end.min(d.len());
        if KEEP.contains(&ty) {
            let at = out.len();
            out.extend_from_slice(&d[i..end]);
            if ty == b"VP8X" && len >= 1 {
                out[at + 8] &= !(0x08 | 0x04); // EXIF + XMP flags
            }
        } else {
            removed.push(match ty {
                b"EXIF" => "EXIF data (incl. any GPS/camera info)".into(),
                b"XMP " => "XMP data".into(),
                _ => format!("{} chunk", String::from_utf8_lossy(ty).trim()),
            });
        }
        i = end;
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Some(out)
}

// ------------------------------------------------------------------ MP4 / MOV

fn isobmff_kind(d: &[u8]) -> Option<&'static str> {
    let ty = d.get(4..8)?;
    if ty == b"ftyp" {
        let size = u32::from_be_bytes(d[0..4].try_into().ok()?) as usize;
        let brands = d.get(8..size.min(d.len()))?;
        let heif = brands.chunks(4).any(|b| {
            matches!(
                b,
                b"heic" | b"heix" | b"heim" | b"heis" | b"mif1" | b"msf1" | b"avif" | b"avis"
            )
        });
        let video = brands.chunks(4).any(|b| {
            matches!(
                b,
                b"isom"
                    | b"iso2"
                    | b"mp41"
                    | b"mp42"
                    | b"qt  "
                    | b"M4V "
                    | b"avc1"
                    | b"3gp4"
                    | b"3gp5"
            )
        });
        return Some(if heif && !video {
            "HEIC/AVIF image"
        } else {
            "MP4/MOV video"
        });
    }
    matches!(ty, b"moov" | b"mdat" | b"wide" | b"free").then_some("MP4/MOV video")
}

/// Boxes are neutralised *in place* (renamed to `free`, contents zeroed,
/// timestamps zeroed) so every size and sample offset stays valid: no
/// remuxing, no risk of breaking playback.
fn isobmff(d: &[u8], removed: &mut Vec<String>) -> Option<Vec<u8>> {
    let mut out = d.to_vec();
    let mut found: Vec<&'static str> = Vec::new();
    walk(&mut out, 0, d.len(), 0, &mut found)?;
    removed.extend(found.into_iter().map(String::from));
    Some(out)
}

fn walk(
    b: &mut [u8],
    mut i: usize,
    end: usize,
    depth: u8,
    found: &mut Vec<&'static str>,
) -> Option<()> {
    let note = |s: &'static str, found: &mut Vec<&'static str>| {
        if !found.contains(&s) {
            found.push(s);
        }
    };
    while i + 8 <= end {
        let mut size = u32::from_be_bytes(b[i..i + 4].try_into().ok()?) as usize;
        let ty: [u8; 4] = b[i + 4..i + 8].try_into().ok()?;
        let mut hdr = 8;
        if size == 1 {
            size = u64::from_be_bytes(b.get(i + 8..i + 16)?.try_into().ok()?) as usize;
            hdr = 16;
        } else if size == 0 {
            size = end - i;
        }
        if size < hdr || i + size > end {
            return None;
        }
        let (body, stop) = (i + hdr, i + size);
        match &ty {
            b"moov" | b"trak" | b"mdia" if depth < 6 => walk(b, body, stop, depth + 1, found)?,
            b"udta" | b"meta" | b"uuid" | b"XMP_" => {
                note(
                    match &ty {
                        b"meta" => "metadata (location, device, creation date)",
                        b"uuid" | b"XMP_" => "XMP data",
                        _ => "user data (location, device, software)",
                    },
                    found,
                );
                b[i + 4..i + 8].copy_from_slice(b"free");
                b[body..stop].fill(0); // a 64-bit size stays in the header
            }
            b"mvhd" | b"tkhd" | b"mdhd" => {
                let v = *b.get(body)?;
                let n = if v == 1 { 16 } else { 8 };
                let t = b.get_mut(body + 4..body + 4 + n)?;
                if t.iter().any(|&x| x != 0) {
                    t.fill(0);
                    note("creation/modification dates", found);
                }
            }
            _ => {}
        }
        i = stop;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_folders_and_control_chars() {
        assert_eq!(scrub_name("/home/fletch/Pictures/IMG_1.jpg"), "IMG_1.jpg");
        assert_eq!(scrub_name("C:\\Users\\x\\a.txt"), "a.txt");
        assert_eq!(scrub_name("bad\u{7}name.txt"), "badname.txt");
        assert_eq!(scrub_name("../"), "file");
    }

    #[test]
    fn text_invisibles_removed_emoji_kept() {
        let s = "\u{FEFF}hi\u{200B} there\u{2009}you \u{E0101}ok ❤\u{FE0F}\u{FE0F} 👨\u{200D}👩 🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F} \u{E0041}x";
        let mut r = vec![];
        let out = scrub_text(s, &mut r);
        assert_eq!(
            out,
            "hi there you ok ❤\u{FE0F} 👨\u{200D}👩 🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F} x"
        );
        assert_eq!(
            r,
            [
                "byte-order mark",
                "4 invisible characters",
                "1 unusual space (made normal)"
            ]
        );
        let plain = "plain text\nstays exactly the same\r\n";
        let s = scrub("a.md", plain.as_bytes());
        assert_eq!(s.data, plain.as_bytes());
        assert_eq!(s.report.summary(), "text: no metadata found");
    }

    fn exif_with_gps(orientation: u16) -> Vec<u8> {
        // Big-endian TIFF: IFD0 {Make, Orientation, DateTime(ptr), GPS ptr}
        let mut t = b"MM\0\x2a\0\0\0\x08".to_vec();
        t.extend_from_slice(&4u16.to_be_bytes());
        for (tag, ty, val) in [
            (0x010Fu16, 2u16, 0u32),
            (0x0112, 3, (orientation as u32) << 16),
            (0x0132, 2, 0),
            (0x8825, 4, 0),
        ] {
            t.extend_from_slice(&tag.to_be_bytes());
            t.extend_from_slice(&ty.to_be_bytes());
            t.extend_from_slice(&1u32.to_be_bytes());
            t.extend_from_slice(&val.to_be_bytes());
        }
        t.extend_from_slice(&0u32.to_be_bytes());
        t.extend_from_slice(b"Apple iPhone 15 37.7749N 122.4194W");
        let mut seg = vec![0xFF, 0xE1];
        seg.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend_from_slice(&t);
        seg
    }

    fn fake_jpeg(orientation: u16) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        j.extend_from_slice(&[0xFF, 0xE0, 0, 16]);
        j.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        j.extend(exif_with_gps(orientation));
        j.extend_from_slice(&[0xFF, 0xFE, 0, 9]);
        j.extend_from_slice(b"secret!");
        j.extend_from_slice(&[0xFF, 0xDB, 0, 4, 0xAA, 0xBB]); // DQT stand-in
        j.extend_from_slice(&[0xFF, 0xDA, 0, 4, 0x01, 0x02]); // SOS
        j.extend_from_slice(&[0x11, 0xFF, 0x00, 0x22, 0xFF, 0xD3, 0x33]); // stuffed + RST
        j.extend_from_slice(&[0xFF, 0xD9]);
        j.extend_from_slice(b"\xFF\xD8MPF depth map with its own Exif");
        j
    }

    #[test]
    fn jpeg_metadata_removed_orientation_kept() {
        let s = scrub("IMG.jpg", &fake_jpeg(6));
        let d = &s.data;
        let has = |n: &[u8]| d.windows(n.len()).any(|w| w == n);
        assert!(!has(b"iPhone") && !has(b"secret") && !has(b"depth map"));
        assert!(has(b"JFIF") && has(&[0x11, 0xFF, 0x00, 0x22, 0xFF, 0xD3, 0x33]));
        assert!(d.ends_with(&[0xFF, 0xD9]));
        // minimal EXIF with orientation 6 only
        let t = exif_scan(&d[12..]);
        assert_eq!(t.orientation, 6);
        assert!(!t.gps && !t.camera && !t.time && !t.other);
        for want in [
            "GPS location",
            "camera make/model",
            "date/time taken",
            "comment",
            "extra images",
        ] {
            assert!(
                s.report.removed.iter().any(|r| r.starts_with(want)),
                "{want}: {:?}",
                s.report
            );
        }
        // Upright photos get no EXIF at all.
        let s = scrub("IMG.jpg", &fake_jpeg(1));
        assert!(!s.data.windows(4).any(|w| w == b"Exif"));
    }

    #[test]
    fn png_text_and_time_removed() {
        let chunk = |ty: &[u8], body: &[u8]| {
            let mut c = (body.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(ty);
            c.extend_from_slice(body);
            c.extend_from_slice(&[0, 0, 0, 0]);
            c
        };
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        p.extend(chunk(b"IHDR", &[0; 13]));
        p.extend(chunk(b"tEXt", b"Author\0Fletch"));
        p.extend(chunk(b"tIME", &[7; 7]));
        p.extend(chunk(b"IDAT", b"pixels"));
        p.extend(chunk(b"IEND", b""));
        let s = scrub("a.png", &p);
        assert!(!s.data.windows(6).any(|w| w == b"Fletch"));
        assert!(s.data.windows(6).any(|w| w == b"pixels"));
        assert_eq!(s.report.removed.len(), 2);
    }

    #[test]
    fn mp4_location_and_dates_neutralised_in_place() {
        let bx = |ty: &[u8], body: &[u8]| {
            let mut c = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            c.extend_from_slice(ty);
            c.extend_from_slice(body);
            c
        };
        let mut mvhd = vec![0, 0, 0, 0];
        mvhd.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0xDE, 0xAD, 0xBE, 0xEF]);
        mvhd.extend_from_slice(&[1; 88]);
        let udta = bx(b"udta", &bx(b"\xA9xyz", b"+37.7749-122.4194/"));
        let mdhd = bx(b"mdhd", &[0, 0, 0, 0, 9, 9, 9, 9, 9, 9, 9, 9, 0, 0]);
        let trak = bx(
            b"trak",
            &[
                bx(b"tkhd", &[0, 0, 0, 0, 5, 5, 5, 5, 5, 5, 5, 5]),
                bx(b"mdia", &mdhd),
            ]
            .concat(),
        );
        let meta = bx(b"meta", b"com.apple.quicktime.model iPhone");
        let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), udta, trak, meta].concat());
        let file = [
            bx(b"ftyp", b"qt  \0\0\0\0qt  "),
            moov,
            bx(b"mdat", b"FRAMEDATA"),
        ]
        .concat();
        let s = scrub("clip.mov", &file);
        assert_eq!(s.report.kind, "MP4/MOV video");
        assert_eq!(s.data.len(), file.len());
        let has = |n: &[u8]| s.data.windows(n.len()).any(|w| w == n);
        assert!(!has(b"37.7749") && !has(b"iPhone") && !has(&[0xDE, 0xAD]) && !has(&[9, 9, 9, 9]));
        assert!(has(b"FRAMEDATA") && has(b"mvhd"));
        assert_eq!(s.report.removed.len(), 3, "{:?}", s.report.removed);
    }

    #[test]
    fn unknown_and_heic_reported_not_cleaned() {
        let s = scrub("a.bin", &[0, 1, 2, 3, 0xFF]);
        assert!(!s.report.cleaned);
        assert!(s.report.summary().contains("NOT removed"));
        let heic = [&[0, 0, 0, 16][..], b"ftypheic\0\0\0\0"].concat();
        let s = scrub("a.heic", &heic);
        assert_eq!(s.report.kind, "HEIC/AVIF image");
        assert!(!s.report.cleaned);
    }
}
