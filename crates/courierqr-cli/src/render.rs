//! Turn core frames into QR code pixels.
//!
//! Each frame is encoded as a single **byte-mode** segment at a fixed QR
//! version and error-correction level L. Forcing byte mode matters: an
//! "optimal" encoder may split data into numeric/alphanumeric segments,
//! and some decoders only hand back raw bytes for byte-mode segments.

use anyhow::{Context, Result, bail};
use qrcode::bits::Bits;
use qrcode::{Color, EcLevel, QrCode, Version};

/// Quiet zone around each code, in modules (the QR spec asks for 4).
const QUIET_MODULES: usize = 4;

/// Largest payload (bytes) that fits a byte-mode QR code of `version` at ECC L.
pub fn byte_capacity(version: i16) -> usize {
    let fits = |n: usize| {
        let mut bits = Bits::new(Version::Normal(version));
        bits.push_byte_data(&vec![0u8; n]).is_ok() && bits.push_terminator(EcLevel::L).is_ok()
    };
    // Capacity is monotonic in n; binary search (V40-L tops out at 2953).
    let (mut lo, mut hi) = (0usize, 4096usize);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) { lo = mid } else { hi = mid - 1 }
    }
    lo
}

/// Encode one frame as a QR code with exactly this version, in byte mode.
pub fn encode(data: &[u8], version: i16) -> Result<QrCode> {
    let mut bits = Bits::new(Version::Normal(version));
    bits.push_byte_data(data)
        .and_then(|_| bits.push_terminator(EcLevel::L))
        .with_context(|| format!("{} bytes don't fit QR version {version}-L", data.len()))?;
    Ok(QrCode::with_bits(bits, EcLevel::L)?)
}

/// Encode `data` in byte mode at the smallest version that fits, at `ec`.
pub fn encode_smallest(data: &[u8], ec: EcLevel) -> Result<QrCode> {
    (1..=40)
        .find_map(|v| {
            let mut bits = Bits::new(Version::Normal(v));
            bits.push_byte_data(data).ok()?;
            bits.push_terminator(ec).ok()?;
            QrCode::with_bits(bits, ec).ok()
        })
        .context("data too large for a QR code")
}

/// Render a QR code for a terminal using half-block characters (two module
/// rows per text line), forcing black-on-white so it scans on dark themes.
pub fn terminal_qr(code: &QrCode) -> String {
    let n = code.width();
    let colors = code.to_colors();
    let q = QUIET_MODULES as isize;
    let dark = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < n
            && (y as usize) < n
            && colors[y as usize * n + x as usize] == Color::Dark
    };
    let mut out = String::new();
    let mut y = -q;
    while y < n as isize + q {
        for x in -q..n as isize + q {
            let fg = if dark(x, y) { 16 } else { 231 }; // top half
            let bg = if dark(x, y + 1) { 16 } else { 231 }; // bottom half
            out.push_str(&format!("\x1b[38;5;{fg}m\x1b[48;5;{bg}m\u{2580}"));
        }
        out.push_str("\x1b[0m\n");
        y += 2;
    }
    out
}

/// Plain grayscale image of a QR code (quiet zone included), `scale` px per module.
pub fn code_image(code: &QrCode, scale: usize) -> Luma {
    let n = code.width();
    let side = (n + 2 * QUIET_MODULES) * scale;
    let mut pixels = vec![255u8; side * side];
    let colors = code.to_colors();
    for y in 0..n * scale {
        for x in 0..n * scale {
            if colors[(y / scale) * n + x / scale] == Color::Dark {
                pixels[(y + QUIET_MODULES * scale) * side + x + QUIET_MODULES * scale] = 0;
            }
        }
    }
    Luma {
        width: side,
        height: side,
        pixels,
    }
}

/// How many codes to show at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
}

impl Grid {
    pub fn from_count(n: usize) -> Result<Self> {
        Ok(match n {
            1 => Grid { cols: 1, rows: 1 },
            2 => Grid { cols: 2, rows: 1 },
            4 => Grid { cols: 2, rows: 2 },
            _ => bail!("grid must be 1, 2 or 4 codes, got {n}"),
        })
    }

    pub fn cells(&self) -> usize {
        self.cols * self.rows
    }
}

/// An 8-bit grayscale image: 0 = black, 255 = white.
pub struct Luma {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// Lays out QR codes of one version into a grid at a fixed pixel scale.
pub struct Layout {
    pub version: i16,
    pub grid: Grid,
    /// Pixels per QR module.
    pub scale: usize,
}

impl Layout {
    /// Pick the largest integer module scale so the whole grid fits in
    /// `max_w` × `max_h` pixels. Integer scaling keeps every module the
    /// same size, which cameras like far better than smoothed scaling.
    pub fn fit(version: i16, grid: Grid, max_w: usize, max_h: usize) -> Result<Self> {
        let cell = Self::cell_modules(version);
        let scale = (max_w / (cell * grid.cols)).min(max_h / (cell * grid.rows));
        if scale < 2 {
            bail!(
                "a {}x{} grid of version-{version} codes needs at least {}x{} px",
                grid.cols,
                grid.rows,
                2 * cell * grid.cols,
                2 * cell * grid.rows
            );
        }
        Ok(Layout {
            version,
            grid,
            scale,
        })
    }

    fn cell_modules(version: i16) -> usize {
        (17 + 4 * version as usize) + 2 * QUIET_MODULES
    }

    pub fn width(&self) -> usize {
        Self::cell_modules(self.version) * self.scale * self.grid.cols
    }

    pub fn height(&self) -> usize {
        Self::cell_modules(self.version) * self.scale * self.grid.rows
    }

    /// Bytes per frame the core should target.
    pub fn frame_bytes(&self) -> usize {
        byte_capacity(self.version)
    }

    /// Render up to `grid.cells()` frames, row-major. Missing cells stay white.
    pub fn render(&self, frames: &[Vec<u8>]) -> Result<Luma> {
        let (w, h) = (self.width(), self.height());
        let mut pixels = vec![255u8; w * h];
        let cell_px = Self::cell_modules(self.version) * self.scale;

        for (i, frame) in frames.iter().take(self.grid.cells()).enumerate() {
            let code = encode(frame, self.version)?;
            let n = code.width();
            let colors = code.to_colors();
            let ox = (i % self.grid.cols) * cell_px + QUIET_MODULES * self.scale;
            let oy = (i / self.grid.cols) * cell_px + QUIET_MODULES * self.scale;
            for my in 0..n {
                for mx in 0..n {
                    if colors[my * n + mx] != Color::Dark {
                        continue;
                    }
                    for dy in 0..self.scale {
                        let row = (oy + my * self.scale + dy) * w + ox + mx * self.scale;
                        pixels[row..row + self.scale].fill(0);
                    }
                }
            }
        }
        Ok(Luma {
            width: w,
            height: h,
            pixels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use courierqr_core::qr_capacity;

    #[test]
    fn capacities_match_spec_table() {
        assert_eq!(byte_capacity(15), qr_capacity::V15_L);
        assert_eq!(byte_capacity(20), qr_capacity::V20_L);
        assert_eq!(byte_capacity(25), qr_capacity::V25_L);
        assert_eq!(byte_capacity(30), qr_capacity::V30_L);
        assert_eq!(byte_capacity(40), qr_capacity::V40_L);
    }

    #[test]
    fn full_frame_fits_and_one_more_byte_does_not() {
        let cap = byte_capacity(25);
        assert!(encode(&vec![0xAB; cap], 25).is_ok());
        assert!(encode(&vec![0xAB; cap + 1], 25).is_err());
    }

    #[test]
    fn layout_fits_and_scales_by_integers() {
        let l = Layout::fit(25, Grid::from_count(4).unwrap(), 1920, 1080).unwrap();
        assert!(l.width() <= 1920 && l.height() <= 1080);
        assert_eq!(l.width() % l.scale, 0);
        assert!(Layout::fit(40, Grid::from_count(4).unwrap(), 300, 300).is_err());
    }
}
