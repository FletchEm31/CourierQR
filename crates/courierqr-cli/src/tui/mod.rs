//! `courierqr` full-screen terminal UI (ratatui).
//!
//! Run `courierqr` with no arguments (or `courierqr ui`). Layout mirrors
//! the AtmosAlpha terminal: logo + status header, numbered tabs, a key-hint
//! footer and a `^p` command palette; mouse clicks work on tabs.

mod devices;
mod logo;
mod receive;
mod send;

use std::io::stdout;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::pair;

// ---------------------------------------------------------------- theme (matches the phone app)

pub(crate) mod theme {
    use ratatui::style::Color;
    pub const BG: Color = Color::Rgb(13, 15, 18);
    pub const PANEL: Color = Color::Rgb(22, 26, 32);
    pub const LINE: Color = Color::Rgb(38, 44, 53);
    pub const TEXT: Color = Color::Rgb(232, 235, 239);
    pub const MUTED: Color = Color::Rgb(139, 148, 161);
    pub const ACCENT: Color = Color::Rgb(61, 220, 151);
    pub const WARN: Color = Color::Rgb(255, 184, 77);
    pub const ERROR: Color = Color::Rgb(255, 107, 107);
}
use theme::*;

pub(crate) fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(LINE))
        .title(Span::styled(format!(" {title} "), Style::new().fg(MUTED)))
        .style(Style::new().bg(BG).fg(TEXT))
}

// ---------------------------------------------------------------- app

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tab {
    Receive,
    Devices,
    Send,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Send, Tab::Receive, Tab::Devices];
    fn label(self) -> &'static str {
        match self {
            Tab::Receive => "Receive",
            Tab::Devices => "Devices",
            Tab::Send => "Send",
        }
    }
}

/// Where camera frames come from.
#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "camera"), allow(dead_code))]
pub enum SourceSpec {
    Camera(String),
    File(PathBuf),
}

pub struct Opts {
    /// Camera device, if forced (`--camera /dev/video2`).
    #[cfg_attr(not(feature = "camera"), allow(dead_code))]
    pub camera: Option<String>,
    /// Replay a .y4m recording instead of a camera (testing).
    pub camera_file: Option<PathBuf>,
    /// Where received files are saved.
    pub out: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    GoReceive,
    GoDevices,
    GoSend,
    StartStop,
    NextCamera,
    AddDevice,
    Quit,
}

const COMMANDS: &[(Command, &str)] = &[
    (Command::GoSend, "Go to Send"),
    (Command::GoReceive, "Go to Receive"),
    (Command::GoDevices, "Go to Devices"),
    (Command::StartStop, "Start / stop the camera"),
    (Command::NextCamera, "Switch camera"),
    (Command::AddDevice, "Pair a new device"),
    (Command::Quit, "Quit"),
];

struct Palette {
    query: String,
    selected: usize,
}

impl Palette {
    fn matches(&self) -> Vec<(Command, &'static str)> {
        let q = self.query.to_lowercase();
        COMMANDS
            .iter()
            .copied()
            .filter(|(_, label)| label.to_lowercase().contains(&q))
            .collect()
    }
}

pub(crate) struct App {
    tab: Tab,
    palette: Option<Palette>,
    pub(crate) receive: receive::ReceiveTab,
    pub(crate) devices: devices::DevicesTab,
    pub(crate) send: send::SendTab,
    /// Transient message in the footer (text, is_error, since).
    toast: Option<(String, bool, Instant)>,
    quit: bool,
    /// Tab label hit-boxes from the last draw, for mouse clicks.
    tab_hits: Vec<(Rect, Tab)>,
    /// This computer's public key + fingerprint (created on first run).
    pub(crate) own_key: Option<(String, String)>,
    pub(crate) unlock: Option<Unlock>,
    pub(crate) out: PathBuf,
}

/// Startup gate shown when the key vault is encrypted at rest and no code
/// has been supplied yet (`courierqr vault on`).
#[derive(Default)]
pub(crate) struct Unlock {
    input: String,
    error: Option<String>,
}

/// This computer's public key + fingerprint, unlocking the vault if a code
/// is already available (env var).
fn load_own_key() -> Option<(String, String)> {
    pair::own_public_key_quiet()
        .ok()
        .and_then(|(pk, _)| courierqr_core::fingerprint(&pk).ok().map(|fp| (pk, fp)))
}

impl App {
    pub(crate) fn new(opts: Opts) -> Self {
        let locked = pair::vault_is_locked().unwrap_or(false);
        let own_key = load_own_key();
        // Locked, and we couldn't open it from the environment: prompt.
        let unlock = (locked && own_key.is_none()).then(Unlock::default);
        let mut sources: Vec<(SourceSpec, String)> = Vec::new();
        if let Some(f) = &opts.camera_file {
            sources.push((
                SourceSpec::File(f.clone()),
                format!("recording {}", f.display()),
            ));
        }
        #[cfg(feature = "camera")]
        if opts.camera_file.is_none() {
            for (path, name) in crate::camera::cameras() {
                if opts.camera.as_deref().is_none_or(|c| c == path) {
                    sources.push((SourceSpec::Camera(path.clone()), format!("{name} ({path})")));
                }
            }
            if sources.is_empty()
                && let Some(c) = &opts.camera
            {
                sources.push((SourceSpec::Camera(c.clone()), c.clone()));
            }
        }
        App {
            tab: Tab::Send,
            palette: None,
            receive: receive::ReceiveTab::new(sources),
            devices: devices::DevicesTab::new(),
            send: send::SendTab::new(),
            toast: None,
            quit: false,
            tab_hits: Vec::new(),
            own_key,
            unlock,
            out: opts.out,
        }
    }

    /// Try the typed recovery code. On success the keys load for the rest of
    /// the session; on failure the prompt stays up with an error.
    fn try_unlock(&mut self, code: &str) {
        // Thread the code to pair::own_identity (and later auto-unlocks) for
        // this process only. Startup is single-threaded here; no worker yet.
        unsafe { std::env::set_var("COURIERQR_RECOVERY_CODE", code.trim()) };
        match load_own_key() {
            Some(k) => {
                self.own_key = Some(k);
                self.unlock = None;
                self.toast("keys unlocked", false);
            }
            None => {
                unsafe { std::env::remove_var("COURIERQR_RECOVERY_CODE") };
                if let Some(u) = self.unlock.as_mut() {
                    u.error = Some("Wrong recovery code — try again.".into());
                    u.input.clear();
                }
            }
        }
    }

    fn unlock_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Enter => {
                let code = self
                    .unlock
                    .as_ref()
                    .map(|u| u.input.clone())
                    .unwrap_or_default();
                if !code.trim().is_empty() {
                    self.try_unlock(&code);
                }
            }
            KeyCode::Backspace => {
                if let Some(u) = self.unlock.as_mut() {
                    u.input.pop();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(u) = self.unlock.as_mut() {
                    u.input.push(c);
                    u.error = None;
                }
            }
            _ => {}
        }
    }

    fn draw_unlock(&self, f: &mut Frame, area: Rect) {
        let u = self.unlock.as_ref().unwrap();
        let [_, mid, _] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(9),
            Constraint::Min(1),
        ])
        .areas(area);
        let [_, center, _] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(56),
            Constraint::Min(1),
        ])
        .areas(mid);
        let block = panel("keys locked");
        let inner = block.inner(center);
        f.render_widget(block, center);
        let masked: String = "•".repeat(u.input.chars().count());
        let mut lines = vec![
            Line::from(Span::styled(
                "This computer's keys are encrypted at rest.",
                Style::new().fg(TEXT),
            )),
            Line::from(Span::styled(
                "Enter your recovery code to unlock them.",
                Style::new().fg(MUTED),
            )),
            Line::raw(""),
            Line::from(vec![
                Span::styled("  code  ", Style::new().fg(MUTED)),
                Span::styled(masked, Style::new().fg(ACCENT)),
                Span::styled("▏", Style::new().fg(ACCENT)),
            ]),
        ];
        if let Some(e) = &u.error {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(e.clone(), Style::new().fg(ERROR))));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }

    pub(crate) fn toast(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error, Instant::now()));
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.tick();
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(33))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
                    Event::Paste(text) if self.tab == Tab::Send => send::on_paste(self, &text),
                    Event::Mouse(m) => {
                        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                            let hit = self.tab_hits.iter().find(|(r, _)| {
                                m.column >= r.x && m.column < r.x + r.width && m.row == r.y
                            });
                            if let Some((_, t)) = hit {
                                self.tab = *t;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.receive.stop();
        Ok(())
    }

    /// Background work: drain the camera worker, run unlocks.
    pub(crate) fn tick(&mut self) {
        let events = self.receive.poll();
        for e in events {
            let receive::Event::Message(m, err) = e;
            self.toast(m, err);
        }
        self.send.tick();
        if let Some(rx) = self.receive.take_completed() {
            receive::unlock_with_own_key(self, rx);
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|t| t.2.elapsed() > Duration::from_secs(6))
        {
            self.toast = None;
        }
    }

    fn run_command(&mut self, c: Command) {
        match c {
            Command::GoReceive => self.tab = Tab::Receive,
            Command::GoDevices => self.tab = Tab::Devices,
            Command::GoSend => self.tab = Tab::Send,
            Command::StartStop => {
                self.tab = Tab::Receive;
                receive::start_stop(self);
            }
            Command::NextCamera => {
                self.tab = Tab::Receive;
                let msg = self.receive.next_camera();
                self.toast(msg, false);
            }
            Command::AddDevice => {
                self.tab = Tab::Devices;
                self.devices.begin_add();
            }
            Command::Quit => self.quit = true,
        }
    }

    pub(crate) fn on_key(&mut self, k: KeyEvent) {
        // The unlock gate swallows everything until the vault is open.
        if self.unlock.is_some() {
            return self.unlock_key(k);
        }
        // Command palette has focus when open.
        if let Some(p) = self.palette.as_mut() {
            match k.code {
                KeyCode::Esc => self.palette = None,
                KeyCode::Enter => {
                    let pick = p.matches().get(p.selected).map(|m| m.0);
                    self.palette = None;
                    if let Some(c) = pick {
                        self.run_command(c);
                    }
                }
                KeyCode::Up => p.selected = p.selected.saturating_sub(1),
                KeyCode::Down => {
                    p.selected = (p.selected + 1).min(p.matches().len().saturating_sub(1))
                }
                KeyCode::Backspace => {
                    p.query.pop();
                    p.selected = 0;
                }
                KeyCode::Char(ch) => {
                    p.query.push(ch);
                    p.selected = 0;
                }
                _ => {}
            }
            return;
        }
        if k.code == KeyCode::Char('p') && k.modifiers.contains(KeyModifiers::CONTROL) {
            self.palette = Some(Palette {
                query: String::new(),
                selected: 0,
            });
            return;
        }
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        // A tab with a text field open gets every key.
        if self.tab == Tab::Receive && self.receive.wants_text() {
            receive::on_key(self, k);
            return;
        }
        if self.tab == Tab::Send && self.send.wants_text() {
            send::on_key(self, k);
            return;
        }
        if self.tab == Tab::Devices && self.devices.wants_text() {
            devices::on_key(self, k);
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('1') => self.tab = Tab::Send,
            KeyCode::Char('2') => self.tab = Tab::Receive,
            KeyCode::Char('3') => self.tab = Tab::Devices,
            KeyCode::Tab => {
                let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.tab = Tab::ALL[(i + 1) % Tab::ALL.len()];
            }
            _ => match self.tab {
                Tab::Receive => receive::on_key(self, k),
                Tab::Devices => devices::on_key(self, k),
                Tab::Send => send::on_key(self, k),
            },
        }
    }

    // ---------------------------------------------------------------- drawing

    pub(crate) fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Block::default().style(Style::new().bg(BG)), area);

        if self.unlock.is_some() {
            let logo = self.fitting_logo(area.width);
            let header_h = (logo.height().max(4) + 2) as u16;
            let [header, body] =
                Layout::vertical([Constraint::Length(header_h), Constraint::Min(4)]).areas(area);
            self.draw_header(f, header, logo);
            self.draw_unlock(f, body);
            return;
        }

        // While transmitting, the codes need every row: shrink the header.
        let logo = if self.tab == Tab::Send && self.send.live.is_some() {
            &logo::LOGOS[logo::COMPACT]
        } else {
            self.fitting_logo(area.width)
        };
        let header_h = (logo.height().max(4) + 2) as u16;
        // The Send tab sizes its codes for the live screen, which uses the
        // compact header: tell it how many rows that frees up.
        let compact_h = (logo::LOGOS[logo::COMPACT].height().max(4) + 2) as u16;
        self.send.extra_rows = header_h.saturating_sub(compact_h);
        let [header, tabs, body, footer] = Layout::vertical([
            Constraint::Length(header_h),
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .areas(area);

        self.draw_header(f, header, logo);
        self.draw_tabs(f, tabs);
        match self.tab {
            Tab::Receive => receive::draw(self, f, body),
            Tab::Devices => devices::draw(self, f, body),
            Tab::Send => send::draw(self, f, body),
        }
        self.draw_footer(f, footer);
        if self.palette.is_some() {
            self.draw_palette(f, area);
        }
    }

    fn fitting_logo(&self, width: u16) -> &'static logo::Logo {
        let room = width.saturating_sub(64) as usize; // leave the status column room
        let chosen = &logo::LOGOS[0];
        if chosen.width() <= room {
            return chosen;
        }
        // Fall back through ever smaller designs.
        logo::LOGOS
            .iter()
            .filter(|l| l.width() <= room)
            .max_by_key(|l| l.width())
            .unwrap_or(&logo::LOGOS[logo::COMPACT])
    }

    fn draw_header(&self, f: &mut Frame, area: Rect, logo: &logo::Logo) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(LINE))
            .style(Style::new().bg(BG));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [left, right] = Layout::horizontal([
            Constraint::Length(logo.width() as u16 + 2),
            Constraint::Min(30),
        ])
        .areas(inner);

        let top_pad = (inner.height as usize).saturating_sub(logo.height()) / 2;
        let mut lines: Vec<Line> = vec![Line::raw(""); top_pad];
        for (row, l) in logo.lines.iter().enumerate() {
            let muted = logo.muted_rows.contains(&row);
            let spans: Vec<Span> = l
                .chars()
                .enumerate()
                .map(|(i, ch)| {
                    let accent = i >= logo.accent_from || i < logo.accent_until;
                    let fg = if accent {
                        ACCENT
                    } else if muted {
                        MUTED
                    } else {
                        TEXT
                    };
                    Span::styled(ch.to_string(), Style::new().fg(fg))
                })
                .collect();
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines).style(Style::new().bg(BG)), left);

        let paired = pair::load().map(|d| d.len()).unwrap_or(0);
        let fp = self
            .own_key
            .as_ref()
            .map(|k| k.1.as_str())
            .unwrap_or("not set up");
        let (cam_dot, cam) = self.receive.source_label();
        let kv = |k: &str, v: Span<'static>| {
            Line::from(vec![
                Span::styled(format!("{k:<15}"), Style::new().fg(MUTED)),
                v,
            ])
        };
        let status = vec![
            kv(
                "this computer",
                Span::styled(
                    fp.to_owned(),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
            ),
            kv(
                "camera",
                Span::styled(format!("{cam_dot} {cam}"), Style::new().fg(TEXT)),
            ),
            kv(
                "paired",
                Span::styled(
                    format!("{paired} device{}", if paired == 1 { "" } else { "s" }),
                    Style::new().fg(TEXT),
                ),
            ),
            kv(
                "saving to",
                Span::styled(shorten_home(&self.out), Style::new().fg(TEXT)),
            ),
        ];
        let pad = (right.height as usize).saturating_sub(status.len()) / 2;
        let mut out = vec![Line::raw(""); pad];
        out.extend(status);
        f.render_widget(Paragraph::new(out).style(Style::new().bg(BG)), right);
    }

    fn draw_tabs(&mut self, f: &mut Frame, area: Rect) {
        self.tab_hits.clear();
        let mut spans = vec![Span::raw(" ")];
        let mut x = area.x + 1;
        for (i, t) in Tab::ALL.iter().enumerate() {
            let label = format!(" {} {} ", i + 1, t.label());
            let w = label.chars().count() as u16;
            let style = if *t == self.tab {
                Style::new().fg(BG).bg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(MUTED)
            };
            self.tab_hits.push((Rect::new(x, area.y, w, 1), *t));
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
            x += w + 1;
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::new().bg(BG)),
            area,
        );
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let [msg, keys] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        if let Some((t, err, _)) = &self.toast {
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {t}"),
                    Style::new().fg(if *err { ERROR } else { ACCENT }),
                ))
                .style(Style::new().bg(BG)),
                msg,
            );
        }
        let mut hints: Vec<(&str, &str)> = match self.tab {
            Tab::Receive => receive::hints(self),
            Tab::Devices => devices::hints(self),
            Tab::Send => send::hints(self),
        };
        hints.extend([("1-3", "tabs"), ("^p", "palette"), ("q", "quit")]);
        let mut spans = vec![Span::raw(" ")];
        for (k, what) in hints {
            spans.push(Span::styled(
                k.to_owned(),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(format!(" {what}   "), Style::new().fg(MUTED)));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::new().bg(PANEL)),
            keys,
        );
    }

    fn draw_palette(&self, f: &mut Frame, area: Rect) {
        let p = self.palette.as_ref().unwrap();
        let w = 52.min(area.width.saturating_sub(4));
        let h = (COMMANDS.len() as u16 + 4).min(area.height.saturating_sub(2));
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + 3, w, h);
        f.render_widget(Clear, r);
        let block = panel("command palette").border_style(Style::new().fg(ACCENT));
        let inner = block.inner(r);
        f.render_widget(block, r);
        let [q, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("› ", Style::new().fg(ACCENT)),
                    Span::styled(p.query.clone(), Style::new().fg(TEXT)),
                    Span::styled("▏", Style::new().fg(ACCENT)),
                ]),
                Line::raw(""),
            ]),
            q,
        );
        let items: Vec<ListItem> = p
            .matches()
            .iter()
            .enumerate()
            .map(|(i, (_, label))| {
                let style = if i == p.selected {
                    Style::new().fg(BG).bg(ACCENT)
                } else {
                    Style::new().fg(TEXT)
                };
                ListItem::new(Span::styled(format!(" {label}"), style))
            })
            .collect();
        f.render_widget(List::new(items), list);
    }
}

pub(crate) fn shorten_home(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

/// Enter the full-screen UI. Restores the terminal on exit and on panic.
pub fn run(opts: Opts) -> Result<()> {
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableMouseCapture, EnableBracketedPaste);
    let result = App::new(opts).run(&mut terminal);
    let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

#[allow(dead_code)]
pub(crate) fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        _ => (128, 128, 128),
    }
}

#[cfg(test)]
mod snapshot;
