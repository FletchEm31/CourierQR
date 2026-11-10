//! Webcam receiver: phone shows QR codes, this computer's camera reads them.
//!
//! Frames come from a [`Source`]: a V4L2 webcam (feature `camera`), or a
//! `.y4m` recording (`--camera-file`, used by tests and handy for replaying
//! a capture through the exact same pipeline).

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use courierqr_core::{Accepted, FrameError, Receiver};

use crate::decode;

/// One 8-bit grayscale camera frame.
pub struct Frame {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

pub trait Source {
    /// Next frame, or `None` when the source is exhausted (files only).
    fn next_frame(&mut self) -> Result<Option<Frame>>;
    fn describe(&self) -> String;
    /// What the camera is and how it's configured, for the UI.
    fn info(&self) -> CameraInfo;
}

/// Everything about the capture setup that affects how well codes read.
#[derive(Clone, Debug, Default)]
pub struct CameraInfo {
    pub device: String,
    pub card: String,
    pub driver: String,
    pub bus: String,
    /// Pixel format being captured (MJPG, YUYV, ...).
    pub format: String,
    pub width: u32,
    pub height: u32,
    /// Frame rate the camera was configured for, if it reports one.
    pub fps: Option<f64>,
    /// Largest resolution the camera offers in any format.
    pub max: Option<(u32, u32)>,
    /// Every pixel format on offer.
    pub formats: Vec<String>,
    /// Autofocus: Some(true) = has auto focus, Some(false) = manual focus
    /// only, None = fixed focus (no focus controls).
    pub autofocus: Option<bool>,
}

impl CameraInfo {
    pub fn megapixels(w: u32, h: u32) -> f64 {
        (w as f64 * h as f64) / 1e6
    }
}

// ---------------------------------------------------------------- .y4m file

/// Reads 8-bit YUV4MPEG2 (any 4:2:0/4:2:2/4:4:4/mono chroma), keeping luma.
pub struct Y4mSource {
    r: BufReader<File>,
    w: usize,
    h: usize,
    chroma: usize,
    name: String,
    fps: Option<f64>,
}

impl Y4mSource {
    pub fn open(path: &Path) -> Result<Self> {
        let mut r = BufReader::new(
            File::open(path).with_context(|| format!("opening {}", path.display()))?,
        );
        let mut header = String::new();
        r.read_line(&mut header)?;
        if !header.starts_with("YUV4MPEG2 ") {
            bail!("{} is not a .y4m file", path.display());
        }
        let field = |p: char| {
            header
                .split_whitespace()
                .find(|t| t.starts_with(p))
                .map(|t| &t[1..])
        };
        let w: usize = field('W').context("y4m: no width")?.parse()?;
        let h: usize = field('H').context("y4m: no height")?.parse()?;
        let c = field('C').unwrap_or("420");
        let chroma = if c.starts_with("mono") {
            0
        } else if c.starts_with("444") {
            2 * w * h
        } else if c.starts_with("422") {
            w * h
        } else {
            2 * w.div_ceil(2) * h.div_ceil(2)
        };
        Ok(Y4mSource {
            r,
            w,
            h,
            chroma,
            name: path.display().to_string(),
            fps: field('F').and_then(|f| {
                let (n, d) = f.split_once(':')?;
                let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
                (d > 0.0).then(|| n / d)
            }),
        })
    }
}

impl Source for Y4mSource {
    fn next_frame(&mut self) -> Result<Option<Frame>> {
        let mut line = String::new();
        if self.r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let mut pixels = vec![0u8; self.w * self.h];
        self.r.read_exact(&mut pixels)?;
        std::io::copy(
            &mut (&mut self.r).take(self.chroma as u64),
            &mut std::io::sink(),
        )?;
        Ok(Some(Frame {
            pixels,
            width: self.w,
            height: self.h,
        }))
    }

    fn describe(&self) -> String {
        format!("{} ({}x{} recording)", self.name, self.w, self.h)
    }

    fn info(&self) -> CameraInfo {
        CameraInfo {
            device: self.name.clone(),
            card: "recording".into(),
            driver: "y4m file".into(),
            bus: "-".into(),
            format: "Y4M".into(),
            width: self.w as u32,
            height: self.h as u32,
            fps: self.fps,
            max: Some((self.w as u32, self.h as u32)),
            formats: vec!["Y4M".into()],
            autofocus: None,
        }
    }
}

// ---------------------------------------------------------------- V4L2 webcam

#[cfg(feature = "camera")]
pub use v4l_source::{V4lSource, cameras, list_cameras};

#[cfg(feature = "camera")]
mod v4l_source {
    use super::*;
    use v4l::buffer::Type;
    use v4l::io::traits::CaptureStream;
    use v4l::prelude::*;
    use v4l::video::Capture;
    use v4l::video::capture::Parameters;
    use v4l::{Format, FourCC};

    pub struct V4lSource {
        stream: MmapStream<'static>,
        fmt: Format,
        name: String,
        bad_jpegs: u32,
        info: CameraInfo,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Pixels {
        Mjpg,
        Yuyv,
        Grey,
    }

    impl Pixels {
        fn fourcc(self) -> FourCC {
            FourCC::new(match self {
                Pixels::Mjpg => b"MJPG",
                Pixels::Yuyv => b"YUYV",
                Pixels::Grey => b"GREY",
            })
        }
    }

    impl V4lSource {
        /// Open a webcam. `prefer` forces a pixel format; otherwise MJPEG
        /// (full resolution at full frame rate over USB 2), then YUYV, then GREY.
        pub fn open(path: &str, prefer: Option<Pixels>) -> Result<Self> {
            let dev = Device::with_path(path).with_context(|| format!("opening camera {path}"))?;
            let caps = dev.query_caps()?;
            let offered: Vec<FourCC> = dev.enum_formats()?.into_iter().map(|f| f.fourcc).collect();
            let order = match prefer {
                Some(p) => vec![p],
                None => vec![Pixels::Mjpg, Pixels::Yuyv, Pixels::Grey],
            };
            let pix = order
                .into_iter()
                .find(|p| offered.contains(&p.fourcc()))
                .with_context(|| format!("{path} offers no usable format (have: {offered:?})"))?;

            // Largest size up to 1920x1080, preferring 1280x720 when offered:
            // plenty of pixels per QR module without slow decoding.
            let sizes: Vec<(u32, u32)> = dev
                .enum_framesizes(pix.fourcc())?
                .into_iter()
                .flat_map(|s| s.size.to_discrete())
                .map(|d| (d.width, d.height))
                .filter(|&(w, h)| w <= 1920 && h <= 1080)
                .collect();
            let (w, h) = if sizes.contains(&(1280, 720)) {
                (1280, 720)
            } else {
                sizes
                    .iter()
                    .copied()
                    .max_by_key(|&(w, h)| w * h)
                    .unwrap_or((640, 480))
            };
            let fmt = dev.set_format(&Format::new(w, h, pix.fourcc()))?;
            let _ = dev.set_params(&Parameters::with_fps(30));

            let fps = dev.params().ok().and_then(|p| {
                let i = p.interval;
                (i.numerator > 0).then(|| i.denominator as f64 / i.numerator as f64)
            });
            let mut formats: Vec<String> = offered.iter().map(|f| f.to_string()).collect();
            formats.dedup();
            let max = offered
                .iter()
                .flat_map(|fc| dev.enum_framesizes(*fc).unwrap_or_default())
                .flat_map(|s| s.size.to_discrete())
                .map(|d| (d.width, d.height))
                .max_by_key(|&(w, h)| w as u64 * h as u64);
            let names: Vec<String> = dev
                .query_controls()
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.name.to_lowercase())
                .collect();
            let autofocus = if names.iter().any(|n| {
                n.contains("focus, auto")
                    || n.contains("focus automatic")
                    || n.contains("autofocus")
            }) {
                Some(true)
            } else if names.iter().any(|n| n.contains("focus")) {
                Some(false)
            } else {
                None
            };
            let info = CameraInfo {
                device: path.to_string(),
                card: caps.card.clone(),
                driver: caps.driver.clone(),
                bus: caps.bus.clone(),
                format: fmt.fourcc.to_string(),
                width: fmt.width,
                height: fmt.height,
                fps,
                max,
                formats,
                autofocus,
            };

            let stream = MmapStream::with_buffers(&dev, Type::VideoCapture, 4)?;
            Ok(V4lSource {
                info,
                stream,
                name: format!(
                    "{} ({path}, {}x{} {})",
                    caps.card, fmt.width, fmt.height, fmt.fourcc
                ),
                fmt,
                bad_jpegs: 0,
            })
        }
    }

    impl Source for V4lSource {
        fn next_frame(&mut self) -> Result<Option<Frame>> {
            let (buf, meta) = self.stream.next()?;
            let data = &buf[..(meta.bytesused as usize).min(buf.len()).max(1)];
            let (w, h) = (self.fmt.width as usize, self.fmt.height as usize);
            let stride = (self.fmt.stride as usize).max(1);
            let pixels = match &self.fmt.fourcc.repr {
                b"MJPG" => {
                    match image::load_from_memory_with_format(data, image::ImageFormat::Jpeg) {
                        Ok(img) => {
                            self.bad_jpegs = 0;
                            let g = img.into_luma8();
                            return Ok(Some(Frame {
                                width: g.width() as usize,
                                height: g.height() as usize,
                                pixels: g.into_raw(),
                            }));
                        }
                        Err(e) => {
                            self.bad_jpegs += 1;
                            if self.bad_jpegs > 15 {
                                bail!(
                                    "this camera's MJPEG frames don't decode ({e}); retry with --camera-format yuyv"
                                );
                            }
                            return self.next_frame();
                        }
                    }
                }
                // Y0 U Y1 V: luma is every other byte.
                b"YUYV" => (0..h)
                    .flat_map(|y| data[y * stride..].iter().step_by(2).take(w).copied())
                    .collect(),
                _ => (0..h)
                    .flat_map(|y| data[y * stride..y * stride + w].iter().copied())
                    .collect(),
            };
            Ok(Some(Frame {
                pixels,
                width: w,
                height: h,
            }))
        }

        fn describe(&self) -> String {
            self.name.clone()
        }

        fn info(&self) -> CameraInfo {
            self.info.clone()
        }
    }

    /// (device path, card name) for every capture-capable camera.
    pub fn cameras() -> Vec<(String, String)> {
        let mut out = Vec::new();
        for n in v4l::context::enum_devices() {
            let path = n.path().display().to_string();
            let Ok(dev) = Device::with_path(&path) else {
                continue;
            };
            let Ok(caps) = dev.query_caps() else { continue };
            if dev.enum_formats().map(|f| f.is_empty()).unwrap_or(true) {
                continue;
            }
            out.push((path, caps.card));
        }
        out
    }

    pub fn list_cameras() -> Result<()> {
        let nodes = v4l::context::enum_devices();
        if nodes.is_empty() {
            eprintln!("no cameras found (is it plugged in? are you in the 'video' group?)");
        }
        for n in nodes {
            let path = n.path().display().to_string();
            let Ok(dev) = Device::with_path(&path) else {
                continue;
            };
            let Ok(caps) = dev.query_caps() else { continue };
            let formats: Vec<String> = dev
                .enum_formats()
                .map(|fs| fs.iter().map(|f| f.fourcc.to_string()).collect())
                .unwrap_or_default();
            if formats.is_empty() {
                continue; // metadata nodes
            }
            println!("{path:<12} {:<32} {}", caps.card, formats.join(" "));
        }
        Ok(())
    }

    pub fn parse_format(s: &str) -> Result<Pixels> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "mjpg" | "mjpeg" => Pixels::Mjpg,
            "yuyv" | "yuv" => Pixels::Yuyv,
            "grey" | "gray" => Pixels::Grey,
            _ => bail!("camera format must be mjpg, yuyv or grey"),
        })
    }
}

#[cfg(feature = "camera")]
pub use v4l_source::parse_format;

// ---------------------------------------------------------------- receive loop

/// Scan frames until the transfer completes (or the source ends / user quits).
pub fn scan_until_complete(src: &mut dyn Source, preview: bool) -> Result<Receiver> {
    eprintln!("camera: {}", src.describe());
    eprintln!("point the camera at the phone's CourierQR codes…");
    let mut rx = Receiver::new();
    let mut view = if preview { Preview::open() } else { None };
    let start = Instant::now();
    let mut last_status = Instant::now() - Duration::from_secs(1);
    let (mut frames, mut codes) = (0u64, 0u64);
    let mut flash = 0u8;
    let mut why = Rejections::default();

    while let Some(f) = src.next_frame()? {
        frames += 1;
        let mut new = 0;
        for payload in decode::decode_all(&f.pixels, f.width, f.height) {
            codes += 1;
            match rx.push(&payload) {
                Ok(Accepted::Progress | Accepted::Complete) => new += 1,
                Ok(_) => {}
                Err(e) => why.note(e, &payload),
            }
        }
        if new > 0 {
            flash = 4;
        }
        let p = rx.progress();
        if last_status.elapsed() >= Duration::from_millis(250) || p.complete {
            last_status = Instant::now();
            let status = if p.needed == 0 && codes == 0 {
                format!("{frames} camera frames, no QR code in view yet")
            } else if p.needed == 0 {
                format!(
                    "{frames} camera frames, {codes} codes read but none are a transfer: {}",
                    why.summary()
                )
            } else {
                format!(
                    "{}/~{} pieces · {:.0}s",
                    p.received,
                    p.needed,
                    start.elapsed().as_secs_f64()
                )
            };
            eprint!("\r\x1b[2K{status}");
            let _ = std::io::stderr().flush();
            if let Some(v) = view.as_mut() {
                v.title(&status);
            }
        }
        if let Some(v) = view.as_mut()
            && !v.show(&f, flash > 0, p.received as f32 / p.needed.max(1) as f32)
        {
            eprintln!();
            bail!("cancelled");
        }
        flash = flash.saturating_sub(1);
        if p.complete {
            eprintln!(
                "\nreceived everything in {:.1}s",
                start.elapsed().as_secs_f64()
            );
            return Ok(rx);
        }
    }
    eprintln!();
    Ok(rx)
}

/// Why codes were turned away, so "nothing happens" is never a mystery.
#[derive(Default, Clone)]
pub struct Rejections {
    pub not_courierqr: u64,
    pub damaged: u64,
    pub other: u64,
    pub example: Option<String>,
}

impl Rejections {
    pub fn note(&mut self, e: FrameError, payload: &[u8]) {
        match e {
            FrameError::NotCourierQr => {
                self.not_courierqr += 1;
                if self.example.is_none() {
                    let text: String = String::from_utf8_lossy(payload).chars().take(24).collect();
                    self.example = Some(text.replace(|c: char| c.is_control(), "."));
                }
            }
            FrameError::Checksum | FrameError::Truncated => self.damaged += 1,
            _ => self.other += 1,
        }
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.not_courierqr > 0 {
            let what = match self.example.as_deref() {
                Some(t) if t.to_ascii_lowercase().starts_with("age1") => {
                    "a pairing key (is the phone on the Keys screen? use Send)".to_string()
                }
                Some(t) => format!("some other QR code (\"{t}…\")"),
                None => "other QR codes".to_string(),
            };
            parts.push(format!("{} were {what}", self.not_courierqr));
        }
        if self.damaged > 0 {
            parts.push(format!("{} damaged (checksum failed)", self.damaged));
        }
        if self.other > 0 {
            parts.push(format!("{} unsupported", self.other));
        }
        parts.join(", ")
    }
}

// ---------------------------------------------------------------- preview window

#[cfg(feature = "window")]
pub(crate) struct Preview {
    win: minifb::Window,
    buf: Vec<u32>,
    w: usize,
    h: usize,
}

#[cfg(feature = "window")]
impl Preview {
    const MAX_W: usize = 960;

    fn open() -> Option<Self> {
        Self::open_quiet()
            .map_err(|e| eprintln!("(no preview window: {e})"))
            .ok()
    }

    /// Like `open`, but reports failure instead of printing (for the TUI,
    /// where stderr would scribble over the screen).
    pub(crate) fn open_quiet() -> Result<Self, String> {
        Ok(Preview {
            win: Self::window(640, 360)?,
            buf: Vec::new(),
            w: 640,
            h: 360,
        })
    }

    fn window(w: usize, h: usize) -> Result<minifb::Window, String> {
        let mut win = minifb::Window::new(
            "CourierQR: receiving",
            w,
            h,
            minifb::WindowOptions {
                resize: true,
                scale_mode: minifb::ScaleMode::AspectRatioStretch,
                ..Default::default()
            },
        )
        .map_err(|e| format!("can't open a window (is a display available?): {e}"))?;
        win.set_target_fps(60);
        Ok(win)
    }

    pub(crate) fn title(&mut self, s: &str) {
        self.win.set_title(&format!("CourierQR: {s}"));
    }

    /// Draw the frame (mirrored, like a selfie view) with a progress bar and a
    /// green border flash on new pieces. Returns false if the user closed it.
    pub(crate) fn show(&mut self, f: &Frame, hit: bool, progress: f32) -> bool {
        if !self.win.is_open() || self.win.is_key_down(minifb::Key::Escape) {
            return false;
        }
        let scale = (f.width.div_ceil(Self::MAX_W)).max(1);
        let (w, h) = (f.width / scale, f.height / scale);
        if (w, h) != (self.w, self.h) {
            self.w = w;
            self.h = h;
        }
        self.buf.resize(w * h, 0);
        for y in 0..h {
            for x in 0..w {
                let v = f.pixels[(y * scale) * f.width + (f.width - 1 - x * scale)] as u32;
                self.buf[y * w + x] = (v << 16) | (v << 8) | v;
            }
        }
        let accent = 0x3ddc97;
        if hit {
            for t in 0..6.min(h / 2) {
                for x in 0..w {
                    self.buf[t * w + x] = accent;
                    self.buf[(h - 1 - t) * w + x] = accent;
                }
                for y in 0..h {
                    self.buf[y * w + t] = accent;
                    self.buf[y * w + w - 1 - t] = accent;
                }
            }
        }
        let filled = ((progress.clamp(0.0, 1.0)) * w as f32) as usize;
        for y in h.saturating_sub(8)..h {
            for x in 0..w {
                self.buf[y * w + x] = if x < filled { accent } else { 0x262c35 };
            }
        }
        self.win.update_with_buffer(&self.buf, w, h).is_ok()
    }
}

#[cfg(not(feature = "window"))]
pub(crate) struct Preview;

#[cfg(not(feature = "window"))]
impl Preview {
    fn open() -> Option<Self> {
        None
    }
    pub(crate) fn open_quiet() -> Result<Self, String> {
        Err("this build has no window support".into())
    }
    pub(crate) fn title(&mut self, _: &str) {}
    pub(crate) fn show(&mut self, _: &Frame, _: bool, _: f32) -> bool {
        true
    }
}
