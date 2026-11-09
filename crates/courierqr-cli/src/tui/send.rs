//! Send tab: type (or paste) text, pick who it's for, and the terminal
//! itself becomes the transmitter: the text is encrypted and shown as a
//! live loop of QR codes, sized to fit the window. No separate window.

use std::time::{Duration, Instant};

use courierqr_core::{FrameConfig, FrameStream, Method, SealKey, Transmission};
use qrcode::{EcLevel, QrCode, Version, bits::Bits};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::theme::*;
use super::{App, panel};
use crate::pair;

/// Smallest and largest QR versions the terminal transmitter will use.
const MIN_VERSION: i16 = 5;
const MAX_VERSION: i16 = 15;

pub(crate) enum Focus {
    Text,
    Recipient,
    Passphrase,
    /// Nothing focused: the global keys (1-3, q, Tab) work.
    None,
}

pub(crate) struct Broadcast {
    stream: FrameStream<Transmission>,
    version: i16,
    code: Option<QrCode>,
    pub(crate) shown: u64,
    pub(crate) needed: usize,
    pub(crate) fps: u32,
    paused: bool,
    last: Instant,
    to: String,
    name: String,
    bytes: usize,
    scrub: String,
}

pub(crate) struct SendTab {
    pub(crate) text: String,
    /// 0..devices.len() = paired device, devices.len() = passphrase.
    pub(crate) recipient: usize,
    pub(crate) passphrase: String,
    pub(crate) focus: Focus,
    pub(crate) error: Option<String>,
    pub(crate) live: Option<Broadcast>,
    /// Codes per second for the next send (↑/↓ also adjust it live).
    pub(crate) fps: u32,
    /// Protection for a paired device (space toggles).
    pub(crate) method: Method,
    /// QR version that fits the body area at the last draw.
    fit_version: i16,
}

impl SendTab {
    pub(crate) fn new() -> Self {
        SendTab {
            text: String::new(),
            recipient: 0,
            passphrase: String::new(),
            focus: Focus::None,
            error: None,
            live: None,
            fps: 8,
            method: Method::PostQuantum,
            fit_version: 10,
        }
    }

    pub(crate) fn wants_text(&self) -> bool {
        self.live.is_some()
            || matches!(
                self.focus,
                Focus::Text | Focus::Recipient | Focus::Passphrase
            )
    }

    fn devices() -> Vec<(String, String)> {
        pair::load().unwrap_or_default()
    }

    /// Advance the transmitter: one new code every 1/fps seconds.
    pub(crate) fn tick(&mut self) {
        let Some(b) = self.live.as_mut() else { return };
        if b.paused
            || (b.code.is_some() && b.last.elapsed() < Duration::from_secs_f64(1.0 / b.fps as f64))
        {
            return;
        }
        b.last = Instant::now();
        let frame = b.stream.next().expect("frame stream never ends");
        let mut bits = Bits::new(Version::Normal(b.version));
        b.code = bits
            .push_byte_data(&frame)
            .and_then(|_| bits.push_terminator(EcLevel::L))
            .and_then(|_| QrCode::with_bits(bits, EcLevel::L))
            .ok();
        b.shown += 1;
    }
}

impl SendTab {
    /// Tests: show the next code now instead of waiting for the frame timer.
    #[cfg(test)]
    pub(crate) fn force_next(&mut self) {
        if let Some(b) = self.live.as_mut() {
            b.code = None;
        }
        self.tick();
    }
}

fn start(app: &mut App) {
    let s = &mut app.send;
    if s.text.trim().is_empty() {
        s.error = Some("type something to send first".into());
        return;
    }
    let devices = SendTab::devices();
    let (key, to) = match devices.get(s.recipient) {
        Some((n, k)) => match SealKey::for_device(k, s.method) {
            Ok(key) => (key, format!("{n} · {}", s.method.label())),
            Err(e) => {
                s.error = Some(format!("{e} (or press space for classic)"));
                s.focus = Focus::Recipient;
                return;
            }
        },
        None => {
            if s.passphrase.chars().count() < 12 {
                s.error = Some("use a passphrase of at least 12 characters".into());
                s.focus = Focus::Passphrase;
                return;
            }
            (
                SealKey::passphrase(s.passphrase.clone()),
                "passphrase".to_string(),
            )
        }
    };
    // No timestamp in the name: it would be metadata about when you wrote it.
    let name = "note.txt".to_string();
    let version = s.fit_version;
    let cfg = FrameConfig {
        max_frame_bytes: crate::render::byte_capacity(version),
    };
    match Transmission::new(&name, s.text.as_bytes(), &key, cfg) {
        Ok(tx) => {
            let needed = tx.source_frames();
            let scrub = tx.scrub_report().summary();
            s.live = Some(Broadcast {
                stream: tx.into_frames(),
                version,
                code: None,
                shown: 0,
                needed,
                fps: s.fps,
                paused: false,
                last: Instant::now(),
                to,
                name,
                bytes: s.text.len(),
                scrub,
            });
            s.error = None;
            s.passphrase.clear();
            s.tick();
        }
        Err(e) => s.error = Some(e.to_string()),
    }
}

pub(crate) fn on_key(app: &mut App, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let s = &mut app.send;

    if let Some(b) = s.live.as_mut() {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                s.live = None;
                app.toast("stopped sending", false);
            }
            KeyCode::Char(' ') => b.paused = !b.paused,
            KeyCode::Up => b.fps = (b.fps + 1).min(20),
            KeyCode::Down => b.fps = b.fps.saturating_sub(1).max(1),
            _ => {}
        }
        return;
    }

    if ctrl && k.code == KeyCode::Char('s') {
        start(app);
        return;
    }
    match (&s.focus, k.code) {
        (Focus::None, KeyCode::Enter | KeyCode::Char('i')) => s.focus = Focus::Text,
        (_, KeyCode::Esc) => s.focus = Focus::None,
        (Focus::Text, KeyCode::Tab) => s.focus = Focus::Recipient,
        (Focus::Recipient, KeyCode::Tab) => {
            s.focus = if s.recipient >= SendTab::devices().len() {
                Focus::Passphrase
            } else {
                Focus::Text
            }
        }
        (Focus::Passphrase, KeyCode::Tab) => s.focus = Focus::Text,
        (Focus::Text, KeyCode::Enter) => s.text.push('\n'),
        (Focus::Text, KeyCode::Backspace) => {
            s.text.pop();
        }
        (Focus::Text, KeyCode::Char(c)) if !ctrl => {
            s.text.push(c);
            s.error = None;
        }
        (Focus::Recipient | Focus::None, KeyCode::Left) => s.fps = s.fps.saturating_sub(1).max(1),
        (Focus::Recipient | Focus::None, KeyCode::Right) => s.fps = (s.fps + 1).min(20),
        (Focus::Recipient, KeyCode::Char(' ')) => {
            s.method = match s.method {
                Method::PostQuantum => Method::Classic,
                Method::Classic => Method::PostQuantum,
            };
            s.error = None;
        }
        (Focus::Recipient, KeyCode::Up) => s.recipient = s.recipient.saturating_sub(1),
        (Focus::Recipient, KeyCode::Down) => {
            s.recipient = (s.recipient + 1).min(SendTab::devices().len())
        }
        (Focus::Recipient, KeyCode::Enter) => start(app),
        (Focus::Passphrase, KeyCode::Backspace) => {
            s.passphrase.pop();
        }
        (Focus::Passphrase, KeyCode::Char(c)) if !ctrl => s.passphrase.push(c),
        (Focus::Passphrase, KeyCode::Enter) => start(app),
        _ => {}
    }
}

/// Paste arrives as one string (bracketed paste).
pub(crate) fn on_paste(app: &mut App, text: &str) {
    let s = &mut app.send;
    match s.focus {
        Focus::Passphrase => s.passphrase.push_str(text.trim_end_matches(['\r', '\n'])),
        _ if s.live.is_none() => {
            s.text
                .push_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
            s.focus = Focus::Text;
        }
        _ => {}
    }
}

pub(crate) fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let s = &app.send;
    if s.live.is_some() {
        return vec![("space", "pause"), ("↑↓", "speed"), ("esc", "stop")];
    }
    match s.focus {
        Focus::None => vec![("enter", "type a message"), ("←→", "speed")],
        Focus::Recipient => vec![
            ("↑↓", "recipient"),
            ("space", "post-quantum/classic"),
            ("←→", "speed"),
            ("^s", "send"),
            ("tab", "next field"),
        ],
        _ => vec![
            ("^s", "send"),
            ("tab", "next field"),
            ("esc", "done typing"),
        ],
    }
}

pub(crate) fn draw(app: &mut App, f: &mut Frame, area: Rect) {
    // Largest QR version whose code (plus quiet zone) fits the body:
    // one module per column, two per row (half blocks).
    let room = (area.width.saturating_sub(2) as i32).min(area.height.saturating_sub(5) as i32 * 2);
    app.send.fit_version = (MIN_VERSION..=MAX_VERSION)
        .rev()
        .find(|v| 17 + 4 * *v as i32 + 8 <= room)
        .unwrap_or(MIN_VERSION);

    if app.send.live.is_some() {
        return draw_live(app, f, area);
    }
    let s = &app.send;
    let devices = SendTab::devices();
    let [left, right] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(64)]).areas(area);

    // Compose
    let focused = matches!(s.focus, Focus::Text);
    let block = panel("message").border_style(Style::new().fg(if focused { ACCENT } else { LINE }));
    let inner = block.inner(left);
    f.render_widget(block, left);
    let mut lines: Vec<Line> = s
        .text
        .split('\n')
        .map(|l| Line::from(Span::styled(l.to_owned(), Style::new().fg(TEXT))))
        .collect();
    if focused && let Some(last) = lines.last_mut() {
        last.spans.push(Span::styled("▏", Style::new().fg(ACCENT)));
    }
    if s.text.is_empty() && !focused {
        lines = vec![Line::from(Span::styled(
            "Press Enter and type (or paste) what you want to send.",
            Style::new().fg(MUTED),
        ))];
    }
    // Keep the end of long text in view.
    let wrapped_h = lines.len() as u16;
    let scroll = wrapped_h.saturating_sub(inner.height);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        inner,
    );

    // Recipient + summary
    let block = panel("send to · encryption");
    let rin = block.inner(right);
    f.render_widget(block, right);
    let rfocus = matches!(s.focus, Focus::Recipient);
    let mut lines = Vec::new();
    for (i, (n, k)) in devices.iter().enumerate() {
        let sel = i == s.recipient;
        lines.push(Line::from(vec![
            Span::styled(
                if sel { "● " } else { "○ " },
                Style::new().fg(if sel { ACCENT } else { MUTED }),
            ),
            Span::styled(
                format!("{n:<10}"),
                Style::new().fg(TEXT).add_modifier(if sel && rfocus {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
            Span::styled(
                courierqr_core::fingerprint(k).unwrap_or_default(),
                Style::new().fg(MUTED),
            ),
        ]));
    }
    let sel = s.recipient >= devices.len();
    lines.push(Line::from(vec![
        Span::styled(
            if sel { "● " } else { "○ " },
            Style::new().fg(if sel { ACCENT } else { MUTED }),
        ),
        Span::styled("passphrase", Style::new().fg(TEXT)),
    ]));
    if sel {
        let pf = matches!(s.focus, Focus::Passphrase);
        lines.push(Line::from(vec![
            Span::styled("  ", Style::new()),
            Span::styled(
                if s.passphrase.is_empty() && !pf {
                    "(tab here to type it)".to_string()
                } else {
                    "•".repeat(s.passphrase.chars().count())
                },
                Style::new().fg(if s.passphrase.is_empty() { MUTED } else { TEXT }),
            ),
            Span::styled(if pf { "▏" } else { "" }, Style::new().fg(ACCENT)),
        ]));
    }
    if devices.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "Pair a phone in Devices to skip passphrases.",
            Style::new().fg(MUTED),
        )));
    }
    // Encryption: what protects it, and who can open it.
    let k = |t: &str| Span::styled(format!("{t:<9}"), Style::new().fg(MUTED));
    let v = |t: String| Span::styled(t, Style::new().fg(TEXT));
    let ok = |t: String| Span::styled(t, Style::new().fg(ACCENT));
    let head = |t: &'static str| {
        Line::from(Span::styled(
            t,
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
    };
    lines.push(Line::raw(""));
    lines.push(head("encryption"));
    match devices.get(s.recipient) {
        Some((n, key)) => {
            let has_pq = key.contains("age1pq1");
            let radio = |on: bool, label: &str, what: &str, enabled: bool| {
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        if on { "● " } else { "○ " },
                        Style::new().fg(if on { ACCENT } else { MUTED }),
                    ),
                    Span::styled(
                        format!("{label:<13}"),
                        Style::new().fg(if enabled { TEXT } else { MUTED }),
                    ),
                    Span::styled(what.to_string(), Style::new().fg(MUTED)),
                ])
            };
            let pq_on = s.method == Method::PostQuantum;
            lines.push(radio(
                pq_on,
                "post-quantum",
                if has_pq {
                    "ML-KEM-768 + X25519 hybrid, +1.4 KB"
                } else {
                    "re-pair this phone to enable"
                },
                has_pq,
            ));
            lines.push(radio(!pq_on, "classic", "X25519, smallest", true));
            lines.push(Line::from(vec![
                k("cipher"),
                v("age: ChaCha20-Poly1305 (256-bit key)".into()),
            ]));
            lines.push(Line::from(vec![
                k("opens on"),
                ok(format!("{n} only")),
                v(" (secret key never leaves it)".into()),
            ]));
        }
        None => {
            lines.push(Line::from(vec![
                k("method"),
                v("age: scrypt passphrase + ChaCha20-Poly1305".into()),
            ]));
            lines.push(Line::from(vec![
                k("quantum"),
                v("no public-key maths to break; a strong passphrase holds up".into()),
            ]));
            lines.push(Line::from(vec![
                k("opens on"),
                v("anything given the passphrase (4+ random words is best)".into()),
            ]));
        }
    }
    lines.push(Line::from(vec![
        k("metadata"),
        ok("stripped automatically".into()),
        v(" before encryption".into()),
    ]));
    lines.push(Line::from(vec![
        k("visible"),
        v("codes look random; name and contents are encrypted (length is not)".into()),
    ]));
    lines.push(Line::raw(""));
    lines.push(head("transmission"));
    let cap = crate::render::byte_capacity(s.fit_version).saturating_sub(28);
    let overhead = match devices.get(s.recipient) {
        Some(_) if s.method == Method::PostQuantum => 1700,
        _ => 250,
    };
    let est = (s.text.len() + overhead).div_ceil(cap.max(1));
    lines.push(Line::from(vec![
        k("speed"),
        ok(format!("{} codes/s", s.fps)),
        v("  (←/→ to change)".into()),
    ]));
    lines.push(Line::from(vec![
        k("QR size"),
        v(format!(
            "v{}, the largest that fits this window",
            s.fit_version
        )),
    ]));
    lines.push(Line::from(vec![
        k("estimate"),
        v(format!(
            "{} bytes · ~{} codes · ≈{:.0} s",
            s.text.len(),
            est,
            (est as f64 / s.fps as f64).ceil()
        )),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "Ctrl+S",
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " to encrypt and show it as QR codes here",
            Style::new().fg(TEXT),
        ),
    ]));
    if let Some(e) = &s.error {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(e.clone(), Style::new().fg(ERROR))));
    }
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            ratatui::widgets::Block::default().border_style(Style::new().fg(if rfocus {
                ACCENT
            } else {
                LINE
            })),
        ),
        rin,
    );
}

fn draw_live(app: &mut App, f: &mut Frame, area: Rect) {
    let b = app.send.live.as_ref().unwrap();
    let [status, qr] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    let state = if b.paused { "PAUSED" } else { "SENDING" };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!(" {state} "), Style::new().fg(BG).bg(if b.paused { WARN } else { ACCENT }).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {} · {} bytes → {}", b.name, b.bytes, b.to), Style::new().fg(TEXT)),
            ]),
            Line::from(Span::styled(
                format!(
                    "  shown {} · receiver needs ≈{} · {} per second · QR v{}   Point the phone at this screen (Receive tab).",
                    b.shown, b.needed, b.fps, b.version
                ),
                Style::new().fg(MUTED),
            )),
            Line::from(Span::styled(
                format!("  metadata · {}", b.scrub),
                Style::new().fg(ACCENT),
            )),
        ])
        .style(Style::new().bg(BG)),
        status,
    );
    // White backdrop so the quiet zone is unmistakable on dark themes.
    f.render_widget(
        ratatui::widgets::Block::default().style(Style::new().bg(Color::Rgb(255, 255, 255))),
        qr,
    );
    if let Some(code) = &b.code {
        draw_code(f, qr, code);
    }
}

/// Half-block QR (two modules per cell), quiet zone 4, centred.
pub(crate) fn draw_code(f: &mut Frame, area: Rect, code: &QrCode) {
    let n = code.width();
    let colors = code.to_colors();
    let q = 4isize;
    let total = n as isize + 2 * q;
    let (w, h) = (total as u16, (total as u16).div_ceil(2));
    let ox = area.x + area.width.saturating_sub(w) / 2;
    let oy = area.y + area.height.saturating_sub(h) / 2;
    let dark = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < n
            && (y as usize) < n
            && colors[y as usize * n + x as usize] == qrcode::Color::Dark
    };
    let buf = f.buffer_mut();
    for cy in 0..h.min(area.height) {
        for cx in 0..w.min(area.width) {
            let (x, y) = (cx as isize - q, cy as isize * 2 - q);
            let top = if dark(x, y) {
                Color::Rgb(0, 0, 0)
            } else {
                Color::Rgb(255, 255, 255)
            };
            let bot = if dark(x, y + 1) {
                Color::Rgb(0, 0, 0)
            } else {
                Color::Rgb(255, 255, 255)
            };
            if let Some(cell) = buf.cell_mut((ox + cx, oy + cy)) {
                cell.set_symbol("▀").set_fg(top).set_bg(bot);
            }
        }
    }
}
