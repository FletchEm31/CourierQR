//! This gateway's copy of the public feed: one file per [`Envelope`] in
//! `<config dir>/feed/<id>.cqx`, kept for [`RETENTION`]. Gateways replicate
//! these envelopes (not images); each one renders the slideshow locally.
//!
//! Stage 1 (this file): the local store, plus `courierqr feed publish /
//! import / list / prune`. Gateway-to-gateway sync over Tor comes next and
//! writes into the same store.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use courierqr_core::Envelope;

/// How long a message stays on the feed.
pub const RETENTION: Duration = Duration::from_secs(72 * 3600);
/// Most messages a gateway keeps (newest win).
pub const MAX_MESSAGES: usize = 512;

pub fn dir() -> Result<PathBuf> {
    Ok(crate::pair::config_dir()?.join("feed"))
}

fn path_for(dir: &Path, env: &Envelope) -> PathBuf {
    dir.join(format!("{}.cqx", env.id_hex()))
}

/// Add an envelope. Returns `false` if it was already there (duplicate id).
pub fn add(env: &Envelope) -> Result<bool> {
    let d = dir()?;
    fs::create_dir_all(&d)?;
    let path = path_for(&d, env);
    if path.exists() {
        return Ok(false);
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, env.encode()).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path)?;
    Ok(true)
}

/// Every envelope still inside the retention window, oldest first. Expired
/// or unreadable files are deleted on the way.
pub fn load() -> Result<Vec<Envelope>> {
    let d = dir()?;
    let entries = match fs::read_dir(&d) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let now = SystemTime::now();
    let mut found: Vec<(SystemTime, Envelope)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("cqx") {
            continue;
        }
        let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or(now);
        if now.duration_since(modified).unwrap_or_default() > RETENTION {
            let _ = fs::remove_file(&path);
            continue;
        }
        match fs::read(&path).ok().and_then(|b| Envelope::decode(&b).ok()) {
            Some(env) => found.push((modified, env)),
            None => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    found.sort_by_key(|(t, _)| *t);
    if found.len() > MAX_MESSAGES {
        for (_, env) in found.drain(..found.len() - MAX_MESSAGES) {
            let _ = fs::remove_file(path_for(&d, &env));
        }
    }
    Ok(found.into_iter().map(|(_, e)| e).collect())
}

/// Ids currently stored (cheap check for "did the feed change?").
pub fn ids() -> Result<Vec<String>> {
    Ok(load()?.iter().map(Envelope::id_hex).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_dedupes_and_load_drops_junk() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("cqr-feed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::set_var("COURIERQR_CONFIG_DIR", &base) };

        assert!(load().unwrap().is_empty());
        let env = Envelope::new(b"age-encryption.org/v1 fake ciphertext".to_vec());
        assert!(add(&env).unwrap());
        assert!(!add(&env).unwrap(), "same id is a duplicate");
        fs::write(dir().unwrap().join("junk.cqx"), b"not an envelope").unwrap();
        let got = load().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id_hex(), env.id_hex());
        assert!(!dir().unwrap().join("junk.cqx").exists(), "junk removed");

        let _ = fs::remove_dir_all(&base);
    }
}
