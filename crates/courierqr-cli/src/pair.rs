//! Paired devices: named `age1...` public keys the sender can encrypt to.
//!
//! Stored one per line as `<name> <age1...>` in
//! `$COURIERQR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/courierqr`, else
//! `~/.config/courierqr`, file `devices`. Only public keys live here —
//! nothing in this file is secret.

use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use courierqr_core::secrecy::ExposeSecret;
use courierqr_core::{fingerprint, generate_device_keys, validate_public_key};

pub fn config_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("COURIERQR_CONFIG_DIR") {
        return Ok(PathBuf::from(d));
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d).join("courierqr"));
    }
    let home = std::env::var_os("HOME").context("can't find your home directory (set HOME)")?;
    Ok(PathBuf::from(home).join(".config").join("courierqr"))
}

fn devices_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("devices"))
}

/// (name, public key) pairs, in file order.
pub fn load() -> Result<Vec<(String, String)>> {
    let path = devices_file()?;
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(char::is_whitespace))
        .map(|(n, k)| (n.to_owned(), k.trim().to_owned()))
        .collect())
}

fn save(devices: &[(String, String)]) -> Result<()> {
    let path = devices_file()?;
    fs::create_dir_all(path.parent().unwrap())?;
    let mut out =
        String::from("# CourierQR paired devices: <name> <age1 public key>. Public keys only.\n");
    for (n, k) in devices {
        out.push_str(&format!("{n} {k}\n"));
    }
    fs::write(&path, out).with_context(|| format!("writing {}", path.display()))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with("age1")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Validate a device name and key; returns the normalized key and its fingerprint.
pub fn check(name: &str, public_key: &str) -> Result<(String, String)> {
    if !valid_name(name) {
        bail!("device names are 1–32 letters, digits, '-' or '_' (and can't start with age1)");
    }
    let pk = validate_public_key(public_key).context(
        "that isn't a valid age1… public key (a typo is the usual cause; the checksum caught it)",
    )?;
    let fp = fingerprint(&pk)?;
    Ok((pk, fp))
}

/// Save (or replace) a paired device without printing anything.
pub fn add_quiet(name: &str, public_key: &str) -> Result<String> {
    let (pk, fp) = check(name, public_key)?;
    let mut devices = load()?;
    devices.retain(|(n, _)| n != name);
    devices.push((name.to_owned(), pk));
    save(&devices)?;
    Ok(fp)
}

pub fn add(name: &str, public_key: &str, yes: bool) -> Result<()> {
    let (pk, fp) = check(name, public_key)?;
    eprintln!("device:      {name}");
    eprintln!("fingerprint: {fp}");
    if !yes {
        eprint!("Does your phone show exactly this fingerprint? [y/N] ");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
            bail!("not paired. If the fingerprints differ, the key was changed on the way here");
        }
    }
    add_quiet(name, &pk)?;
    eprintln!("paired. Send with: courierqr send <file> --to {name}");
    Ok(())
}

pub fn list() -> Result<()> {
    let devices = load()?;
    if devices.is_empty() {
        eprintln!("no paired devices yet: courierqr pair add <name> <age1…>");
    }
    for (n, k) in devices {
        println!(
            "{n:<16} {}  {k}",
            fingerprint(&k).unwrap_or_else(|_| "INVALID".into())
        );
    }
    Ok(())
}

pub fn remove_quiet(name: &str) -> Result<()> {
    let mut devices = load()?;
    let before = devices.len();
    devices.retain(|(n, _)| n != name);
    if devices.len() == before {
        bail!("no paired device named {name:?}");
    }
    save(&devices)
}

pub fn remove(name: &str) -> Result<()> {
    remove_quiet(name)?;
    eprintln!("removed {name}");
    Ok(())
}

// ---------------------------------------------------------------- this computer's identity

fn identity_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("identity"))
}

/// This computer's own secret identity, if it has one.
pub fn own_identity() -> Result<Option<String>> {
    match fs::read_to_string(identity_file()?) {
        Ok(text) => Ok(text
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("AGE-SECRET-KEY-"))
            .map(str::to_owned)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Load this computer's identity, creating it (0600) on first use.
/// Returns the public key.
pub fn own_public_key() -> Result<String> {
    let (pk, created) = own_public_key_quiet()?;
    if created {
        eprintln!(
            "created this computer's identity at {}",
            identity_file()?.display()
        );
    }
    Ok(pk)
}

/// Like [`own_public_key`] without printing; also says whether it was just created.
pub fn own_public_key_quiet() -> Result<(String, bool)> {
    let path = identity_file()?;
    if let Some(secret) = own_identity()? {
        let pk = courierqr_core::public_key_of(&secret)
            .with_context(|| format!("{} is corrupt", path.display()))?;
        return Ok((pk, false));
    }
    let keys = generate_device_keys();
    fs::create_dir_all(path.parent().unwrap())?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    writeln!(
        f,
        "# CourierQR identity for this computer. SECRET: never share this file."
    )?;
    writeln!(f, "# public key: {}", keys.public)?;
    writeln!(f, "{}", keys.secret.expose_secret())?;
    Ok((keys.public, true))
}

/// Show this computer's public key so a phone can pair with it (to send *to* it).
pub fn show(png: Option<&std::path::Path>) -> Result<()> {
    let pk = own_public_key()?;
    let fp = fingerprint(&pk)?;
    let code = crate::render::encode_smallest(pk.as_bytes(), qrcode::EcLevel::M)?;
    if let Some(path) = png {
        let img = crate::render::code_image(&code, 10);
        image::save_buffer(
            path,
            &img.pixels,
            img.width as u32,
            img.height as u32,
            image::ExtendedColorType::L8,
        )?;
        eprintln!("wrote {}", path.display());
    } else {
        print!("{}", crate::render::terminal_qr(&code));
    }
    println!("\nThis computer's public key:\n  {pk}\nfingerprint: {fp}");
    eprintln!("\nOn the phone: Keys → Scan computer's key, then check the fingerprint matches.");
    Ok(())
}

/// `--to` accepts a raw `age1...` key or a paired device name.
/// Returns (public key, label for messages).
pub fn resolve(to: &str) -> Result<(String, String)> {
    if to.trim().to_ascii_lowercase().starts_with("age1") {
        let pk = validate_public_key(to)?;
        let fp = fingerprint(&pk)?;
        return Ok((pk, format!("key {fp}")));
    }
    let devices = load()?;
    match devices.into_iter().find(|(n, _)| n == to) {
        Some((n, k)) => {
            let fp = fingerprint(&k)?;
            Ok((k, format!("device '{n}' ({fp})")))
        }
        None => bail!("no paired device named {to:?} (see: courierqr pair list)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use courierqr_core::generate_device_keys;

    #[test]
    fn add_list_resolve_remove() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("cqr-pair-test-{}", std::process::id()));
        // SAFETY: only this test touches this variable.
        unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", &dir) };

        let k1 = generate_device_keys().public;
        let k2 = generate_device_keys().public;
        add("phone", &k1, true).unwrap();
        add("tablet", &k2.to_uppercase(), true).unwrap(); // normalized on save
        assert_eq!(
            load().unwrap(),
            vec![("phone".into(), k1.clone()), ("tablet".into(), k2.clone())]
        );

        assert_eq!(resolve("phone").unwrap().0, k1);
        assert_eq!(resolve(&k2).unwrap().0, k2);
        assert!(resolve("nobody").is_err());

        add("phone", &k2, true).unwrap(); // re-pairing replaces
        assert_eq!(resolve("phone").unwrap().0, k2);
        remove("tablet").unwrap();
        assert!(remove("tablet").is_err());

        assert!(add("bad name!", &k1, true).is_err());
        assert!(add("x", "age1typo", true).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
