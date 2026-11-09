//! Official age test vectors (C2SP/CCTV, 0BSD) for the post-quantum hybrid
//! recipient type: proves our ML-KEM-768 + X25519 implementation is
//! byte-compatible with age v1.3+, including every malformed-input case.

use std::io::Read;

use courierqr_core::pq::HybridIdentity;
use sha2::{Digest, Sha256};

struct Vector {
    name: String,
    expect: String,
    payload: Option<String>,
    identities: Vec<String>,
    file: Vec<u8>,
}

fn load(path: &std::path::Path) -> Vector {
    let raw = std::fs::read(path).unwrap();
    let split = raw.windows(2).position(|w| w == b"\n\n").unwrap();
    let head = std::str::from_utf8(&raw[..split]).unwrap();
    let mut file = raw[split + 2..].to_vec();
    let mut v = Vector {
        name: path.file_name().unwrap().to_string_lossy().into(),
        expect: String::new(),
        payload: None,
        identities: vec![],
        file: vec![],
    };
    for line in head.lines() {
        let (k, val) = line.split_once(": ").unwrap();
        match k {
            "expect" => v.expect = val.into(),
            "payload" => v.payload = Some(val.into()),
            "identity" => v.identities.push(val.into()),
            "compressed" => {
                assert_eq!(val, "zlib");
                file = miniz_oxide::inflate::decompress_to_vec_zlib(&file).unwrap();
            }
            _ => {}
        }
    }
    v.file = file;
    v
}

#[test]
fn c2sp_hybrid_vectors() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cctv");
    let mut ran = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let v = load(&entry.unwrap().path());
        if !v.name.starts_with("hybrid") {
            continue;
        }
        let ids: Vec<Box<dyn age::Identity>> = v
            .identities
            .iter()
            .map(|i| -> Box<dyn age::Identity> {
                if i.starts_with("AGE-SECRET-KEY-PQ-1") {
                    Box::new(HybridIdentity::parse(i).unwrap())
                } else {
                    Box::new(i.parse::<age::x25519::Identity>().unwrap())
                }
            })
            .collect();
        let result = age::Decryptor::new(&v.file[..]).and_then(|d| {
            let mut r = d.decrypt(ids.iter().map(|i| i.as_ref()))?;
            let mut out = vec![];
            r.read_to_end(&mut out).map_err(age::DecryptError::from)?;
            Ok(out)
        });
        match (v.expect.as_str(), &result) {
            ("success", Ok(pt)) => {
                let h = format!("{:x}", Sha256::digest(pt));
                assert_eq!(Some(&h), v.payload.as_ref(), "{}: payload hash", v.name);
            }
            ("no match", Err(age::DecryptError::NoMatchingKeys)) => {}
            ("header failure", Err(e)) if !matches!(e, age::DecryptError::NoMatchingKeys) => {}
            (want, got) => panic!(
                "{}: expected {want}, got {:?}",
                v.name,
                got.as_ref().map(|p| p.len())
            ),
        }
        ran += 1;
    }
    assert_eq!(ran, 18, "all hybrid vectors ran");
}

#[test]
fn round_trip_through_age_and_classic_keys_refused_alongside() {
    let id = HybridIdentity::generate();
    let r = id.to_public();
    let ct = age::encrypt(&r, b"post-quantum hello").unwrap();
    assert_eq!(age::decrypt(&id, &ct).unwrap(), b"post-quantum hello");
    // Wrong PQ key: cleanly "not for me".
    let other = HybridIdentity::generate();
    assert!(matches!(
        age::decrypt(&other, &ct),
        Err(age::DecryptError::NoMatchingKeys)
    ));
    // Mixing with a classic X25519 recipient would quietly drop the
    // post-quantum protection, so age refuses.
    let classic = age::x25519::Identity::generate().to_public();
    let mixed: Vec<Box<dyn age::Recipient>> = vec![Box::new(r), Box::new(classic)];
    assert!(age::Encryptor::with_recipients(mixed.iter().map(|b| b.as_ref())).is_err());
}
