//! `courierqr` — send files as animated QR codes, export frames, decode captures.

mod camera;
mod decode;
mod pair;
mod render;
mod tui;

/// Tests that touch process-wide env vars (COURIERQR_CONFIG_DIR) take this.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(feature = "window")]
mod window;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use courierqr_core::secrecy::ExposeSecret;
use courierqr_core::{
    Accepted, DEFAULT_SCRYPT_WORK_FACTOR, FrameConfig, Method, OpenKey, Receiver, SealKey,
    Transmission, generate_device_keys,
};

use render::{Grid, Layout};

const PASSPHRASE_ENV: &str = "COURIERQR_PASSPHRASE";

#[derive(Parser)]
#[command(
    name = "courierqr",
    version,
    about = "Encrypted screen-to-camera file transfer between your own devices"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Full-screen terminal UI (also what plain `courierqr` opens).
    Ui {
        /// Camera device to use (default: the first one found).
        #[arg(long, value_name = "DEVICE")]
        camera: Option<String>,
        /// Replay a .y4m recording instead of a camera (testing/demo).
        #[arg(long, value_name = "FILE.y4m", hide = true)]
        camera_file: Option<PathBuf>,
        /// Where to save received files (default: ~/Downloads).
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Show a file as a looping stream of QR codes in a window.
    #[cfg(feature = "window")]
    Send {
        file: PathBuf,
        #[command(flatten)]
        key: SealArgs,
        #[command(flatten)]
        look: LookArgs,
        /// Code changes per second (each change shows `--grid` new codes).
        #[arg(long, default_value_t = 10.0)]
        fps: f64,
    },
    /// Write the stream to PNG images instead of a window (testing, or for
    /// displaying on another screen).
    Export {
        file: PathBuf,
        #[command(flatten)]
        key: SealArgs,
        #[command(flatten)]
        look: LookArgs,
        /// Output directory.
        #[arg(short, long)]
        out: PathBuf,
        /// Number of images (default: 2x the clean-channel minimum).
        #[arg(long)]
        count: Option<usize>,
    },
    /// Receive a file: live from a webcam (`--camera`), or from photos,
    /// screenshots or extracted video frames.
    Receive {
        /// Image files (PNG or JPEG), in any order. Omit when using --camera.
        #[arg(required_unless_present_any = ["camera", "camera_file"])]
        images: Vec<PathBuf>,
        /// Read codes live from a webcam (default /dev/video0; see `courierqr cameras`).
        #[arg(long, value_name = "DEVICE", num_args = 0..=1, default_missing_value = "/dev/video0", conflicts_with = "images")]
        camera: Option<String>,
        /// Camera pixel format: mjpg, yuyv or grey (default: best available).
        #[arg(long, value_name = "FORMAT", requires = "camera")]
        camera_format: Option<String>,
        /// Replay a .y4m recording as if it were the camera (testing/debugging).
        #[arg(long, value_name = "FILE.y4m", hide = true, conflicts_with_all = ["images", "camera"])]
        camera_file: Option<PathBuf>,
        /// Don't open the camera preview window.
        #[arg(long)]
        no_preview: bool,
        #[command(flatten)]
        key: OpenArgs,
        /// Directory to save the file into.
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        /// Overwrite an existing file of the same name.
        #[arg(long)]
        force: bool,
    },
    /// Manage paired devices (phones you can send to without a passphrase).
    Pair {
        #[command(subcommand)]
        cmd: PairCmd,
    },
    /// First-time setup: create your account number + recovery seed, and
    /// this device's keys. Shows both secrets once.
    Setup {
        /// A short name for this device (e.g. "desktop", "iphone").
        #[arg(long, default_value = "desktop")]
        device: String,
        /// Replace an existing identity.
        #[arg(long)]
        force: bool,
    },
    /// Set up this device from your recovery seed (e.g. a replacement
    /// machine). Prints a fresh account number for local unlock.
    Restore {
        /// Your recovery seed (from `setup`).
        seed: String,
        /// The device label to restore (must match the original to get the
        /// same keys).
        #[arg(long, default_value = "desktop")]
        device: String,
        #[arg(long)]
        force: bool,
    },
    /// Encrypt this computer's keys at rest with a recovery code (or a
    /// YubiKey). Without this, the key file is readable by anyone who gets
    /// the disk.
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
    /// List webcams usable with `receive --camera`.
    #[cfg(feature = "camera")]
    Cameras,
    /// Create a standalone keypair file (advanced; `pair show` manages this
    /// computer's own key for you). Prints the public key.
    Keygen {
        /// Where to write the secret identity (created with 0600 permissions).
        #[arg(short, long)]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum PairCmd {
    /// Pair a device: paste the age1… key from the phone's "Device key" screen.
    Add {
        /// Your name for the device, e.g. "phone".
        name: String,
        /// The device's key set: `age1… age1pq1…` (as the phone shows it),
        /// or a single age1… key from an older pairing.
        #[arg(required = true, num_args = 1..)]
        public_key: Vec<String>,
        /// Skip the fingerprint confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// List paired devices with their fingerprints.
    List,
    /// Show this computer's public key as a QR code, so a phone can pair
    /// with it and send files *to* this computer.
    Show {
        /// Write the QR code to a PNG instead of printing it in the terminal.
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Forget a paired device.
    Remove { name: String },
}

#[derive(Args)]
struct SealArgs {
    /// Encrypt to a paired device (name from `pair list`) or an age1… public
    /// key, instead of a passphrase.
    #[arg(long, value_name = "DEVICE|AGE1_KEY")]
    to: Option<String>,
    /// scrypt cost (log2) for passphrase mode. Higher = slower to brute-force
    /// and slower for the receiver to open.
    #[arg(long, default_value_t = DEFAULT_SCRYPT_WORK_FACTOR, value_parser = clap::value_parser!(u8).range(10..=20))]
    scrypt_cost: u8,
    /// Send the file byte-for-byte. By default photo/video metadata (GPS,
    /// camera, dates), folder names and hidden text characters are stripped
    /// before encryption.
    #[arg(long)]
    keep_metadata: bool,
    /// With --to: use classic X25519 instead of the default post-quantum
    /// hybrid (ML-KEM-768 + X25519). Saves ~1.4 KB per message.
    #[arg(long)]
    classic: bool,
}

#[derive(Args)]
struct OpenArgs {
    /// Secret identity file to decrypt with. Default: this computer's own
    /// identity (from `pair show`), then a passphrase prompt.
    #[arg(long)]
    identity: Option<PathBuf>,
}

#[derive(Args)]
struct LookArgs {
    /// QR version (bigger = more data per code, harder for cameras). 10–40.
    /// Default 15: in camera tests a 2×2 grid of v15 codes read in 100% of
    /// frames, while v20 codes at the same screen size dropped to ~22%.
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(i16).range(10..=40))]
    qr_version: i16,
    /// Codes on screen at once: 1, 2 (side by side) or 4 (2×2).
    #[arg(long, default_value_t = 4)]
    grid: usize,
    /// Max size of the code area in pixels (WIDTHxHEIGHT).
    #[arg(long, default_value = "1000x1000", value_parser = parse_size)]
    size: (usize, usize),
}

fn parse_size(s: &str) -> std::result::Result<(usize, usize), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or("expected WIDTHxHEIGHT, e.g. 1000x1000")?;
    Ok((
        w.parse().map_err(|_| "bad width")?,
        h.parse().map_err(|_| "bad height")?,
    ))
}

fn main() -> Result<()> {
    let Some(cmd) = Cli::parse().cmd else {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            return tui::run(tui::Opts {
                camera: None,
                camera_file: None,
                out: default_download_dir(),
            });
        }
        use clap::CommandFactory;
        Cli::command().print_help()?;
        return Ok(());
    };
    match cmd {
        Cmd::Ui {
            camera,
            camera_file,
            out,
        } => tui::run(tui::Opts {
            camera,
            camera_file,
            out: out.unwrap_or_else(default_download_dir),
        }),
        #[cfg(feature = "window")]
        Cmd::Send {
            file,
            key,
            look,
            fps,
        } => {
            let layout = layout(&look)?;
            let tx = prepare(&file, &key, &layout)?;
            summary(&tx, &layout, Some(fps));
            window::run(&tx, &layout, fps)
        }
        Cmd::Export {
            file,
            key,
            look,
            out,
            count,
        } => {
            let layout = layout(&look)?;
            let tx = prepare(&file, &key, &layout)?;
            summary(&tx, &layout, None);
            export(&tx, &layout, &out, count)
        }
        Cmd::Receive {
            images,
            camera,
            camera_format,
            camera_file,
            no_preview,
            key,
            out,
            force,
        } => {
            let rx = if let Some(path) = camera_file {
                camera::scan_until_complete(&mut camera::Y4mSource::open(&path)?, !no_preview)?
            } else if let Some(dev) = camera {
                open_camera(&dev, camera_format.as_deref(), !no_preview)?
            } else {
                receive_images(&images)?
            };
            finish(rx, &key, &out, force)
        }
        #[cfg(feature = "camera")]
        Cmd::Cameras => camera::list_cameras(),
        Cmd::Pair { cmd } => match cmd {
            PairCmd::Add {
                name,
                public_key,
                yes,
            } => pair::add(&name, &public_key.join(" "), yes),
            PairCmd::List => pair::list(),
            PairCmd::Show { png } => pair::show(png.as_deref()),
            PairCmd::Remove { name } => pair::remove(&name),
        },
        Cmd::Keygen { out } => keygen(&out),
        Cmd::Vault { cmd } => vault(cmd),
        Cmd::Setup { device, force } => setup(&device, force),
        Cmd::Restore {
            seed,
            device,
            force,
        } => restore(&seed, &device, force),
    }
}

fn setup(device: &str, force: bool) -> Result<()> {
    let (account, seed) = pair::setup(device, force)?;
    println!("Your CourierQR identity is set up for device \"{device}\".\n");
    println!("  Account number (unlocks this app):   {account}");
    println!(
        "  Recovery seed  (restores your keys): {}\n",
        seed.expose_secret()
    );
    eprintln!("Write BOTH down and keep them safe and separate from this computer:");
    eprintln!("  • the account number is what you type to unlock CourierQR day to day;");
    eprintln!("  • the recovery seed is the ONLY way to rebuild your keys on a new device");
    eprintln!("    (lose it and the keys are gone — it is shown once and cannot be recovered).");
    eprintln!("\nKeys are encrypted at rest under your account number. To unlock without a");
    eprintln!("prompt, set COURIERQR_ACCOUNT_NUMBER.");
    Ok(())
}

fn restore(seed: &str, device: &str, force: bool) -> Result<()> {
    let account = pair::restore(seed, device, force)?;
    println!("Restored device \"{device}\" from your recovery seed.\n");
    println!("  New account number (unlocks this device): {account}\n");
    eprintln!("Your keys match the ones on your other devices for this label, so anyone you");
    eprintln!("paired with still recognizes you. Keep this account number to unlock this device.");
    Ok(())
}

#[derive(Subcommand)]
enum VaultCmd {
    /// Turn on at-rest encryption. Prints a one-time recovery code.
    On,
    /// Turn off at-rest encryption (needs the current code to unlock first).
    Off,
    /// Show whether the keys are encrypted at rest.
    Status,
}

fn vault(cmd: VaultCmd) -> Result<()> {
    match cmd {
        VaultCmd::On => {
            if pair::vault_is_locked()? {
                eprintln!("already encrypted at rest. Use `vault off` first to change the code.");
                return Ok(());
            }
            let code = courierqr_core::recovery_code();
            pair::vault_enable_with(&code)?;
            println!(
                "Your keys are now encrypted at rest.
"
            );
            println!(
                "  Recovery code:  {code}
"
            );
            eprintln!(
                "Write this down and keep it somewhere safe and separate from this computer."
            );
            eprintln!(
                "You'll need it to use your keys after a reboot. It is shown ONCE and is the"
            );
            eprintln!("only way in — CourierQR cannot recover it for you. To unlock without the");
            eprintln!(
                "prompt (e.g. in scripts), set COURIERQR_RECOVERY_CODE (or COURIERQR_ACCOUNT_NUMBER)."
            );
            Ok(())
        }
        VaultCmd::Off => {
            if !pair::vault_is_locked()? {
                eprintln!("keys are not encrypted at rest; nothing to do.");
                return Ok(());
            }
            pair::vault_disable()?;
            eprintln!("at-rest encryption removed. The key file is now plaintext (0600).");
            Ok(())
        }
        VaultCmd::Status => {
            let locked = pair::vault_is_locked()?;
            println!(
                "keys at rest: {}",
                if locked {
                    "encrypted (account number required)"
                } else {
                    "plaintext (0600 file)"
                }
            );
            if let Some(label) = pair::device_label()? {
                println!("device label: {label}");
            } else if !locked {
                println!("device:       not account-derived (run `courierqr setup`)");
            }
            Ok(())
        }
    }
}

fn layout(look: &LookArgs) -> Result<Layout> {
    Layout::fit(
        look.qr_version,
        Grid::from_count(look.grid)?,
        look.size.0,
        look.size.1,
    )
}

fn prepare(file: &Path, args: &SealArgs, layout: &Layout) -> Result<Transmission> {
    let data = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .context("file name must be valid UTF-8")?;

    let key = match &args.to {
        Some(to) => {
            let (set, label) = pair::resolve(to)?;
            let method = if args.classic {
                Method::Classic
            } else {
                Method::PostQuantum
            };
            match SealKey::for_device(&set, method) {
                Ok(k) => {
                    eprintln!("encrypting to {label}: {}", method.label());
                    k
                }
                Err(_) if method == Method::PostQuantum => {
                    eprintln!(
                        "encrypting to {label}: classic (X25519). It has no post-quantum key yet; \
                         pair it again from the updated app to enable post-quantum"
                    );
                    SealKey::for_device(&set, Method::Classic)?
                }
                Err(e) => return Err(e.into()),
            }
        }
        None => SealKey::Passphrase {
            passphrase: passphrase(true)?.into(),
            work_factor: args.scrypt_cost,
        },
    };
    let config = FrameConfig {
        max_frame_bytes: layout.frame_bytes(),
    };
    let tx = if args.keep_metadata {
        Transmission::new_exact(name, &data, &key, config)?
    } else {
        Transmission::new(name, &data, &key, config)?
    };
    let r = tx.scrub_report();
    if args.keep_metadata {
        eprintln!("metadata: kept (--keep-metadata), sending the file byte-for-byte");
    } else if r.cleaned {
        eprintln!("metadata: {}", r.summary());
    } else {
        eprintln!("warning: {}", r.summary());
    }
    Ok(tx)
}

fn summary(tx: &Transmission, layout: &Layout, fps: Option<f64>) {
    let k = tx.source_frames();
    eprintln!(
        "encrypted {} bytes -> {k} frames of {} bytes (QR v{}-L, {}x{} grid, {} px/module)",
        tx.ciphertext_len(),
        tx.symbol_size(),
        layout.version,
        layout.grid.cols,
        layout.grid.rows,
        layout.scale
    );
    if let Some(fps) = fps {
        let secs = k as f64 / (fps * layout.grid.cells() as f64);
        eprintln!(
            "clean-channel minimum ≈ {secs:.1}s at {fps} fps; expect 1.5–2× with a handheld phone"
        );
    }
}

fn export(tx: &Transmission, layout: &Layout, out: &Path, count: Option<usize>) -> Result<()> {
    fs::create_dir_all(out)?;
    let cells = layout.grid.cells();
    let images = count.unwrap_or_else(|| (tx.source_frames() * 2).div_ceil(cells).max(1));
    let mut stream = tx.frames();
    for i in 0..images {
        let frames: Vec<Vec<u8>> = stream.by_ref().take(cells).collect();
        let img = layout.render(&frames)?;
        let path = out.join(format!("frame_{i:05}.png"));
        image::save_buffer(
            &path,
            &img.pixels,
            img.width as u32,
            img.height as u32,
            image::ExtendedColorType::L8,
        )
        .with_context(|| format!("writing {}", path.display()))?;
    }
    eprintln!("wrote {images} images to {}", out.display());
    Ok(())
}

#[cfg(feature = "camera")]
fn open_camera(dev: &str, format: Option<&str>, preview: bool) -> Result<Receiver> {
    let pix = format.map(camera::parse_format).transpose()?;
    camera::scan_until_complete(&mut camera::V4lSource::open(dev, pix)?, preview)
}

#[cfg(not(feature = "camera"))]
fn open_camera(_: &str, _: Option<&str>, _: bool) -> Result<Receiver> {
    bail!("this build has no webcam support (rebuild with the `camera` feature)")
}

fn receive_images(images: &[PathBuf]) -> Result<Receiver> {
    let mut rx = Receiver::new();
    let (mut codes, mut rejected) = (0usize, 0usize);
    'images: for path in images {
        let img = image::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .into_luma8();
        let (w, h) = img.dimensions();
        for payload in decode::decode_all(img.as_raw(), w as usize, h as usize) {
            codes += 1;
            match rx.push(&payload) {
                Ok(Accepted::Complete) => break 'images,
                Ok(_) => {}
                Err(_) => rejected += 1,
            }
        }
    }
    eprintln!(
        "read {codes} codes from {} images ({rejected} rejected)",
        images.len()
    );
    Ok(rx)
}

/// Decrypt a completed transfer and save it. Key order: `--identity` if
/// given; else this computer's own identity, silently; then a passphrase.
fn finish(rx: Receiver, args: &OpenArgs, out: &Path, force: bool) -> Result<()> {
    let p = rx.progress();
    if p.needed == 0 {
        bail!(
            "no CourierQR transfer was seen. Is the phone on its Send screen, showing the full-screen codes?"
        );
    }
    if !p.complete {
        bail!(
            "not enough frames yet: have {} distinct, need about {}. Capture more of the loop",
            p.received,
            p.needed
        );
    }

    let file = if let Some(path) = &args.identity {
        rx.open(&OpenKey::identity(read_identity(path)?))
            .expect("complete")?
    } else {
        let mut opened = None;
        if let Some(secret) = pair::own_identity()? {
            match rx.open(&OpenKey::identity(secret)).expect("complete") {
                Ok(f) => {
                    eprintln!("unlocked with this computer's key");
                    opened = Some(f);
                }
                Err(courierqr_core::Error::Decrypt(_)) => {
                    eprintln!("not sent to this computer's key; it needs a passphrase");
                }
                Err(e) => return Err(e.into()),
            }
        }
        match opened {
            Some(f) => f,
            None => rx
                .open(&OpenKey::passphrase(passphrase(false)?))
                .expect("complete")?,
        }
    };

    let dest = out.join(safe_name(&file.name)?);
    if dest.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            dest.display()
        );
    }
    fs::create_dir_all(out)?;
    fs::write(&dest, &file.data)?;
    eprintln!("saved {} ({} bytes)", dest.display(), file.data.len());
    Ok(())
}

/// Save a received file into `out` without overwriting anything: if the
/// name is taken, use "name (1).ext", "name (2).ext", ... Returns the path.
pub(crate) fn save_unique(out: &Path, name: &str, data: &[u8]) -> Result<PathBuf> {
    fs::create_dir_all(out)?;
    let base = safe_name(name)?;
    let (stem, ext) = match base.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (base.clone(), String::new()),
    };
    let mut dest = out.join(&base);
    let mut n = 1;
    while dest.exists() {
        dest = out.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)?;
    f.write_all(data)?;
    Ok(dest)
}

/// Where received files go by default: ~/Downloads if it exists, else here.
pub(crate) fn default_download_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Downloads"))
        .filter(|d| d.is_dir())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Pull the `AGE-SECRET-KEY-1...` line out of an identity file, skipping the
/// `# public key:` comment that `keygen` (and age's own tools) write.
fn read_identity(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.lines()
        .map(str::trim)
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .map(str::to_owned)
        .with_context(|| format!("no AGE-SECRET-KEY line in {}", path.display()))
}

/// The name comes from the sender; never let it escape the output directory.
fn safe_name(name: &str) -> Result<String> {
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim();
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    if clean.is_empty() || clean == "." || clean == ".." {
        return Ok("received.bin".into());
    }
    Ok(clean)
}

fn keygen(out: &Path) -> Result<()> {
    if out.exists() {
        bail!(
            "{} already exists; refusing to overwrite a key",
            out.display()
        );
    }
    let keys = generate_device_keys();
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(out)?;
    writeln!(f, "# public key: {}", keys.public)?;
    writeln!(f, "{}", keys.secret.expose_secret())?;
    eprintln!("secret identity written to {}", out.display());
    println!("{}", keys.public);
    Ok(())
}

fn passphrase(confirm: bool) -> Result<String> {
    if let Ok(p) = std::env::var(PASSPHRASE_ENV) {
        return Ok(p);
    }
    let p = rpassword::prompt_password("passphrase: ")?;
    if p.chars().count() < 12 {
        bail!("use a passphrase of at least 12 characters (5+ random words is better)");
    }
    if confirm && rpassword::prompt_password("confirm passphrase: ")? != p {
        bail!("passphrases didn't match");
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::safe_name;

    #[test]
    fn names_cannot_escape_the_output_dir() {
        assert_eq!(safe_name("notes.md").unwrap(), "notes.md");
        assert_eq!(safe_name("../../.bashrc").unwrap(), ".bashrc");
        assert_eq!(safe_name("/etc/passwd").unwrap(), "passwd");
        assert_eq!(safe_name("..").unwrap(), "received.bin");
        assert_eq!(safe_name("").unwrap(), "received.bin");
        assert_eq!(safe_name("a\u{7}b.txt").unwrap(), "ab.txt");
    }
}
