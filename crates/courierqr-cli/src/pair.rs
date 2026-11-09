//! Paired devices: named `age1...` public keys the sender can encrypt to.
//!
//! Stored one per line as `<name> <age1...>` in
//! `$COURIERQR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/courierqr`, else
//! `~/.config/courierqr`, file `devices`. Only public keys live here —
//! nothing in this file is secret.

use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use courierqr_core::secrecy::{ExposeSecret, SecretString};
use courierqr_core::{
    Account, AccountNumber, DeviceKeys, fingerprint, generate_device_keys, validate_public_key,
};

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

/// The stored identity file's full contents, plus the recovery code it was
/// locked with (so a rewrite can re-lock it the same way).
struct Identity {
    text: String,
    lock_code: Option<String>,
}

/// Where a recovery code comes from when the vault is locked: the
/// `COURIERQR_RECOVERY_CODE` env var, else an interactive prompt on a TTY.
fn unlock_code() -> Result<String> {
    if let Ok(code) = std::env::var("COURIERQR_RECOVERY_CODE")
        && !code.trim().is_empty()
    {
        return Ok(code);
    }
    if std::io::stdin().is_terminal() {
        eprint!("Recovery code to unlock this computer's keys: ");
        std::io::stderr().flush()?;
        let mut code = String::new();
        std::io::stdin().lock().read_line(&mut code)?;
        return Ok(code);
    }
    bail!(
        "the identity vault is locked. Set COURIERQR_RECOVERY_CODE, or run on a terminal to be prompted"
    )
}

/// Read the identity file, transparently unlocking a vault. `None` if there
/// is no identity yet.
fn load_identity() -> Result<Option<Identity>> {
    let bytes = match fs::read(identity_file()?) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if courierqr_core::vault::is_locked(&bytes) {
        let code = unlock_code()?;
        let plain = courierqr_core::vault::unlock(&bytes, &code)
            .context("wrong recovery code, or the vault file is damaged")?;
        Ok(Some(Identity {
            text: String::from_utf8(plain).context("vault contents are not valid UTF-8")?,
            lock_code: Some(code),
        }))
    } else {
        Ok(Some(Identity {
            text: String::from_utf8(bytes).context("identity file is not valid UTF-8")?,
            lock_code: None,
        }))
    }
}

/// Write the identity file (0600), re-locking it if `lock_code` is set.
fn save_identity(id: &Identity) -> Result<()> {
    let path = identity_file()?;
    fs::create_dir_all(path.parent().unwrap())?;
    let bytes = match &id.lock_code {
        Some(code) => courierqr_core::vault::lock(id.text.as_bytes(), code)?,
        None => id.text.clone().into_bytes(),
    };
    let tmp = path.with_extension("tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(&tmp)
        .and_then(|mut f| f.write_all(&bytes))
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// This computer's own secret identities (classic and post-quantum, one per
/// line), if it has any. Unlocks the vault if the keys are encrypted at rest.
pub fn own_identity() -> Result<Option<String>> {
    Ok(load_identity()?.map(|id| {
        id.text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("AGE-SECRET-KEY-"))
            .collect::<Vec<_>>()
            .join("\n")
    }))
}

/// Is the identity encrypted at rest?
pub fn vault_is_locked() -> Result<bool> {
    match fs::read(identity_file()?) {
        Ok(b) => Ok(courierqr_core::vault::is_locked(&b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Encrypt the identity at rest under a fresh recovery code, which is
/// returned to be shown once. Creates the identity first if needed.
pub fn vault_enable_with(code: &str) -> Result<()> {
    own_public_key_quiet()?; // ensure an identity exists
    let mut id = load_identity()?.context("no identity to lock")?;
    id.lock_code = Some(code.to_owned());
    save_identity(&id)
}

/// Remove at-rest encryption, leaving a plaintext (0600) identity file.
pub fn vault_disable() -> Result<()> {
    let mut id = load_identity()?.context("no identity found")?;
    id.lock_code = None;
    save_identity(&id)
}

// ------------------------------------------------- account setup / recovery

/// Write an account-derived identity for `label`, locked at rest under
/// `unlock` (the account number's passphrase form).
fn write_derived(label: &str, keys: &DeviceKeys, unlock: &str) -> Result<()> {
    let text = format!(
        "# CourierQR identity for this computer. SECRET: never share this file.\n\
         # account-derived, device label: {label}\n\
         # public keys: {}\n{}\n",
        keys.public,
        keys.secret.expose_secret()
    );
    save_identity(&Identity {
        text,
        lock_code: Some(unlock.to_owned()),
    })
}

/// First-time setup: make a recovery seed and an account number, derive this
/// device's keys from the seed + `label`, and store them encrypted at rest
/// under the account number. Returns (account number, recovery seed) to show
/// once.
pub fn setup(label: &str, force: bool) -> Result<(String, SecretString)> {
    if identity_file()?.exists() && !force {
        bail!(
            "an identity already exists here. Use --force to replace it (you'll re-pair devices)."
        );
    }
    let seed = Account::generate();
    let account = AccountNumber::generate();
    let keys = seed.device_keys(label);
    write_derived(label, &keys, &account.passphrase())?;
    Ok((account.formatted(), seed.number()))
}

/// Set this device up from an existing recovery seed (e.g. a replacement
/// device). Derives the same-label keys and locks them under a fresh account
/// number for this device, which is returned to show once.
pub fn restore(seed: &str, label: &str, force: bool) -> Result<String> {
    if identity_file()?.exists() && !force {
        bail!("an identity already exists here. Use --force to replace it.");
    }
    let seed = Account::parse(seed).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let account = AccountNumber::generate();
    let keys = seed.device_keys(label);
    write_derived(label, &keys, &account.passphrase())?;
    Ok(account.formatted())
}

/// The device label stored in an account-derived identity, if any.
pub fn device_label() -> Result<Option<String>> {
    let Some(id) = load_identity()? else {
        return Ok(None);
    };
    Ok(id
        .text
        .lines()
        .find_map(|l| l.trim().strip_prefix("# account-derived, device label: "))
        .map(|s| s.trim().to_owned()))
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
    if let Some(mut id) = load_identity()? {
        if !id.text.contains("AGE-SECRET-KEY-PQ-1") {
            // Created before post-quantum support: add a post-quantum
            // identity next to the classic one (which keeps working). This
            // goes through save_identity so a locked vault stays locked.
            id.text = format!(
                "{}\n# post-quantum (ML-KEM-768 + X25519) identity, added later\n{}\n",
                id.text.trim_end(),
                courierqr_core::generate_pq_identity().expose_secret()
            );
            save_identity(&id)?;
        }
        let secret: String = id
            .text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("AGE-SECRET-KEY-"))
            .collect::<Vec<_>>()
            .join("\n");
        let pk = courierqr_core::public_key_of(&secret)
            .with_context(|| format!("{} is corrupt", path.display()))?;
        return Ok((pk, false));
    }
    let keys = generate_device_keys();
    let text = format!(
        "# CourierQR identity for this computer. SECRET: never share this file.\n# public keys: {}\n{}\n",
        keys.public,
        keys.secret.expose_secret()
    );
    save_identity(&Identity {
        text,
        lock_code: None,
    })?;
    Ok((keys.public, true))
}

/// Show this computer's public key so a phone can pair with it (to send *to* it).
pub fn show(png: Option<&std::path::Path>) -> Result<()> {
    let pk = own_public_key()?;
    let fp = fingerprint(&pk)?;
    // A post-quantum key set is ~2 KB: low ECC keeps the code as coarse as possible.
    let code = crate::render::encode_smallest(pk.as_bytes(), qrcode::EcLevel::L)?;
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
    println!("\nThis computer's public keys (classic + post-quantum):\n  {pk}\nfingerprint: {fp}");
    eprintln!("\nOn the phone: Keys → Scan computer's key, then check the fingerprint matches.");
    eprintln!("The code is large (post-quantum keys are ~2 KB). If the phone struggles, zoom the");
    eprintln!("terminal out (Ctrl -) or use --png and open the image full screen.");
    Ok(())
}

/// `--to` accepts a raw `age1...` key or a paired device name.
/// Returns (public key, label for messages).
pub fn resolve(to: &str) -> Result<(String, String)> {
    if to.trim().to_ascii_lowercase().starts_with("age1") {
        // a raw key or key set
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

    #[test]
    fn vault_locks_and_unlocks_the_identity() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("cqr-vault-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", &dir) };
        unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };

        // Create the identity (plaintext), remember its keys.
        let (pk, created) = own_public_key_quiet().unwrap();
        assert!(created);
        let keys = own_identity().unwrap().unwrap();
        assert!(!vault_is_locked().unwrap());

        // Lock it. The file becomes an age vault; keys are no longer readable.
        let code = courierqr_core::recovery_code();
        vault_enable_with(&code).unwrap();
        assert!(vault_is_locked().unwrap());
        let raw = fs::read(identity_file().unwrap()).unwrap();
        assert!(
            !raw.windows(14).any(|w| w == b"AGE-SECRET-KEY"),
            "secret not in cleartext"
        );

        // With the code (via env), everything still works and is unchanged.
        unsafe { std::env::set_var("COURIERQR_RECOVERY_CODE", &code) };
        assert_eq!(own_identity().unwrap().unwrap(), keys);
        assert_eq!(own_public_key_quiet().unwrap().0, pk);

        // Wrong code fails cleanly.
        unsafe { std::env::set_var("COURIERQR_RECOVERY_CODE", "WRONG-CODE-HERE") };
        assert!(own_identity().is_err());

        // Unlock back to plaintext.
        unsafe { std::env::set_var("COURIERQR_RECOVERY_CODE", &code) };
        vault_disable().unwrap();
        assert!(!vault_is_locked().unwrap());
        unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
        assert_eq!(own_identity().unwrap().unwrap(), keys);

        unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn setup_then_restore_reproduces_the_same_device_key() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("cqr-setup-{}", std::process::id()));
        let (d1, d2) = (base.join("a"), base.join("b"));
        let _ = fs::remove_dir_all(&base);

        // First device.
        unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", &d1) };
        unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
        let (account, seed) = setup("desktop", false).unwrap();
        assert_eq!(account.len(), 19); // XXXX-XXXX-XXXX-XXXX
        assert!(vault_is_locked().unwrap(), "keys encrypted at rest");
        assert_eq!(
            device_label().ok().flatten(),
            None,
            "label unreadable while locked"
        );

        // Unlock with the account number; capture this device's public key.
        unsafe { std::env::set_var("COURIERQR_RECOVERY_CODE", &account) };
        let pk1 = own_public_key_quiet().unwrap().0;
        assert_eq!(device_label().unwrap().as_deref(), Some("desktop"));
        assert!(
            pk1.contains("age1pq1"),
            "account-derived keys include post-quantum"
        );

        // A replacement device restores from the seed + same label.
        unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", &d2) };
        unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
        let account2 = restore(seed.expose_secret(), "desktop", false).unwrap();
        unsafe { std::env::set_var("COURIERQR_RECOVERY_CODE", &account2) };
        let pk2 = own_public_key_quiet().unwrap().0;
        assert_eq!(pk1, pk2, "recovery reproduces the same keys and identity");

        // A different device label gives a different key.
        unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", base.join("c")) };
        unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
        let acc3 = restore(seed.expose_secret(), "iphone", false).unwrap();
        unsafe { std::env::set_var("COURIERQR_RECOVERY_CODE", &acc3) };
        assert_ne!(
            own_public_key_quiet().unwrap().0,
            pk1,
            "distinct per device label"
        );

        unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
        let _ = fs::remove_dir_all(&base);
    }
}
