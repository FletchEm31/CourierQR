//! On-screen sender: loops the frame stream at a fixed code rate.
//!
//! Keys: Space pause · ↑/↓ faster/slower · Esc or Q quit.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use courierqr_core::Transmission;
use minifb::{Key, KeyRepeat, ScaleMode, Window, WindowOptions};

use crate::render::Layout;

const MIN_FPS: f64 = 1.0;
const MAX_FPS: f64 = 30.0;

pub fn run(tx: &Transmission, layout: &Layout, fps: f64) -> Result<()> {
    let (w, h) = (layout.width(), layout.height());
    let mut window = Window::new(
        "CourierQR",
        w,
        h,
        WindowOptions {
            resize: true,
            // Never resample the codes: keep pixels 1:1 and pad with white.
            scale_mode: ScaleMode::Center,
            ..WindowOptions::default()
        },
    )
    .map_err(|e| anyhow!("can't open a window (is a display available?): {e}"))?;
    // Redraw at the monitor-ish rate; codes only *change* at `fps`, so each
    // one is held for several refreshes and is never caught half-drawn.
    window.set_target_fps(60);

    let cells = layout.grid.cells();
    let mut stream = tx.frames();
    let mut fps = fps.clamp(MIN_FPS, MAX_FPS);
    let mut paused = false;
    let mut shown = 0usize;
    let mut buffer = vec![0u32; w * h];
    let mut last = Instant::now() - Duration::from_secs(1);
    let goal = tx.source_frames();
    let mut last_status = Instant::now() - Duration::from_secs(1);

    while window.is_open() && !window.is_key_down(Key::Escape) && !window.is_key_down(Key::Q) {
        if window.is_key_pressed(Key::Space, KeyRepeat::No) {
            paused = !paused;
        }
        if window.is_key_pressed(Key::Up, KeyRepeat::Yes) {
            fps = (fps + 1.0).min(MAX_FPS);
        }
        if window.is_key_pressed(Key::Down, KeyRepeat::Yes) {
            fps = (fps - 1.0).max(MIN_FPS);
        }

        if !paused && last.elapsed().as_secs_f64() >= 1.0 / fps {
            last = Instant::now();
            let frames: Vec<Vec<u8>> = stream.by_ref().take(cells).collect();
            shown += frames.len();
            let img = layout.render(&frames)?;
            for (dst, &p) in buffer.iter_mut().zip(&img.pixels) {
                let v = p as u32;
                *dst = (v << 16) | (v << 8) | v;
            }
        }
        // Progress goes to the title bar *and* the terminal: GNOME on Wayland
        // doesn't draw title bars for minifb windows, so the terminal is the
        // only place you'd see it there.
        if last_status.elapsed() >= Duration::from_millis(250) {
            last_status = Instant::now();
            let status = format!(
                "{shown} frames sent (receiver needs ≈{goal}) · {fps:.0} fps{}",
                if paused { " · PAUSED" } else { "" }
            );
            window.set_title(&format!("CourierQR — {status}"));
            eprint!("\r\x1b[2K{status}  [space pause · ↑/↓ speed · esc quit]");
            let _ = std::io::stderr().flush();
        }
        window.update_with_buffer(&buffer, w, h)?;
    }
    eprintln!();
    Ok(())
}
