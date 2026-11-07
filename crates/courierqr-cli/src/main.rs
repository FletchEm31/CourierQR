//! `courierqr` — send files as animated QR codes, export frames, decode captures.

mod decode;
mod pair;
mod render;
#[cfg(feature = "window")]
mod window;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use courierqr_core::secrecy::ExposeSecret;
use courierqr_core::{
    Accepted, DEFAULT_SCRYPT_WORK_FACTOR, FrameConfig, OpenKey, Receiver, SealKey, Transmission,
    generate_device_keys,
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
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
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
    /// Reassemble a file from photos/screenshots/extracted video frames.
    Receive {
        /// Image files (PNG or JPEG), in any order.
        #[arg(required = true)]
        images: Vec<PathBuf>,
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
    /// Create a device keypair for paired mode. Prints the public key.
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
        /// The device's public key (age1…).
        public_key: String,
        /// Skip the fingerprint confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// List paired devices with their fingerprints.
    List,
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
}

#[derive(Args)]
struct OpenArgs {
    /// Secret identity file from `courierqr keygen` (otherwise: passphrase).
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
    match Cli::parse().cmd {
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
            key,
            out,
            force,
        } => receive(&images, &key, &out, force),
        Cmd::Pair { cmd } => match cmd {
            PairCmd::Add {
                name,
                public_key,
                yes,
            } => pair::add(&name, &public_key, yes),
            PairCmd::List => pair::list(),
            PairCmd::Remove { name } => pair::remove(&name),
        },
        Cmd::Keygen { out } => keygen(&out),
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
            let (pk, label) = pair::resolve(to)?;
            eprintln!("encrypting to {label}");
            SealKey::recipient(pk)
        }
        None => SealKey::Passphrase {
            passphrase: passphrase(true)?.into(),
            work_factor: args.scrypt_cost,
        },
    };
    let config = FrameConfig {
        max_frame_bytes: layout.frame_bytes(),
    };
    Ok(Transmission::new(name, &data, &key, config)?)
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

fn receive(images: &[PathBuf], args: &OpenArgs, out: &Path, force: bool) -> Result<()> {
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

    let p = rx.progress();
    eprintln!(
        "read {codes} codes from {} images ({rejected} rejected)",
        images.len()
    );
    if !p.complete {
        bail!(
            "not enough frames yet: have {} distinct, need about {} — capture more of the loop",
            p.received,
            p.needed
        );
    }

    let key = match &args.identity {
        Some(path) => OpenKey::identity(read_identity(path)?),
        None => OpenKey::passphrase(passphrase(false)?),
    };
    let file = rx.open(&key).expect("complete")?;

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
