//! Public feed slideshow on the Receive tab.
//!
//! Plays every message in this gateway's feed store ([`crate::feed`]) as one
//! interleaved loop of QR codes, so any offline messenger pointed at this
//! screen can collect the messages meant for it. Display only: the gateway
//! never decrypts these (it usually can't).

use std::cell::Cell;
use std::time::{Duration, Instant};

use courierqr_core::FrameConfig;
use courierqr_core::feed::Playlist;
use qrcode::{EcLevel, QrCode, Version, bits::Bits};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::theme::*;

/// QR versions the slideshow will use (sized to the panel).
const MIN_VERSION: i16 = 5;
const MAX_VERSION: i16 = 15;
/// How often to re-read the feed store for new messages.
const SYNC_EVERY: Duration = Duration::from_secs(3);

pub(crate) struct FeedView {
    playlist: Playlist,
    /// Version the current playlist was built for.
    version: i16,
    /// Largest version that fit the panel at the last draw.
    fit: Cell<i16>,
    ids: Vec<String>,
    last_sync: Option<Instant>,
    last_frame: Instant,
    pub(crate) code: Option<QrCode>,
    pub(crate) fps: u32,
    pub(crate) shown: u64,
    pub(crate) skipped: usize,
}

fn config_for(version: i16) -> FrameConfig {
    FrameConfig {
        max_frame_bytes: crate::render::byte_capacity(version),
    }
}

impl FeedView {
    pub(crate) fn new() -> Self {
        let version = 10;
        FeedView {
            playlist: Playlist::new(config_for(version)),
            version,
            fit: Cell::new(version),
            ids: Vec::new(),
            last_sync: None,
            last_frame: Instant::now() - Duration::from_secs(1),
            code: None,
            fps: 8,
            shown: 0,
            skipped: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.playlist.len()
    }

    /// Tests: re-read the store on the next tick instead of waiting.
    #[cfg(test)]
    pub(crate) fn force_sync(&mut self) {
        self.last_sync = None;
    }

    /// Re-read the store now and then; show the next code every 1/fps s.
    pub(crate) fn tick(&mut self) {
        let fit = self.fit.get();
        if (MIN_VERSION..=MAX_VERSION).contains(&fit) && fit != self.version {
            // The panel changed size: rebuild for the new code size.
            self.version = fit;
            self.playlist = Playlist::new(config_for(fit));
            self.ids.clear();
            self.last_sync = None;
        }
        if self.last_sync.is_none_or(|t| t.elapsed() >= SYNC_EVERY) {
            self.last_sync = Some(Instant::now());
            if let Ok(envs) = crate::feed::load() {
                let ids: Vec<String> = envs.iter().map(|e| e.id_hex()).collect();
                if ids != self.ids || self.playlist.len() != ids.len() {
                    self.skipped = self.playlist.sync(&envs);
                    self.ids = ids;
                }
            }
        }
        if self.playlist.is_empty() {
            self.code = None;
            return;
        }
        if self.code.is_some()
            && self.last_frame.elapsed() < Duration::from_secs_f64(1.0 / self.fps as f64)
        {
            return;
        }
        self.last_frame = Instant::now();
        let Some(frame) = self.playlist.next_frame() else {
            return;
        };
        let mut bits = Bits::new(Version::Normal(self.version));
        self.code = bits
            .push_byte_data(&frame)
            .and_then(|_| bits.push_terminator(EcLevel::L))
            .and_then(|_| QrCode::with_bits(bits, EcLevel::L))
            .ok();
        self.shown += 1;
    }
}

/// Largest QR version whose half-block rendering (+ quiet zone) fits.
fn fitting_version(area: Rect) -> i16 {
    (MIN_VERSION..=MAX_VERSION)
        .rev()
        .find(|v| {
            let total = (17 + 4 * *v as u16) + 8;
            total <= area.width && total.div_ceil(2) <= area.height
        })
        .unwrap_or(MIN_VERSION - 1)
}

/// Draw the slideshow into `area` (inside the camera panel).
pub(crate) fn draw(view: &FeedView, f: &mut Frame, area: Rect) {
    let [status, qr] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    view.fit.set(fitting_version(qr));
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" PUBLIC FEED ", Style::new().fg(BG()).bg(ACCENT())),
                Span::styled(
                    format!(
                        "  {} message{} · {} per second · QR v{} · shown {}",
                        view.len(),
                        if view.len() == 1 { "" } else { "s" },
                        view.fps,
                        view.version,
                        view.shown
                    ),
                    Style::new().fg(TEXT()),
                ),
            ]),
            Line::from(Span::styled(
                "  Offline messengers: point the camera here to collect messages meant for you. Enter: scan your phone instead.",
                Style::new().fg(MUTED()),
            )),
        ]),
        status,
    );
    if view.fit.get() < MIN_VERSION {
        f.render_widget(
            Paragraph::new(Span::styled(
                "  make the terminal bigger to show the feed",
                Style::new().fg(WARN()),
            )),
            qr,
        );
        return;
    }
    if let Some(code) = &view.code {
        // White backdrop sized to the code, so the quiet zone is clear.
        let total = code.width() as u16 + 8;
        let (w, h) = (total.min(qr.width), total.div_ceil(2).min(qr.height));
        let backdrop = Rect {
            x: qr.x + qr.width.saturating_sub(w) / 2,
            y: qr.y + qr.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(
            Block::default().style(Style::new().bg(Color::Rgb(255, 255, 255))),
            backdrop,
        );
        super::send::draw_code(f, qr, code);
    }
}
