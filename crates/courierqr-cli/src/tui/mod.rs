//! `courierqr` full-screen terminal UI (ratatui).
//!
//! Run `courierqr` with no arguments (or `courierqr ui`). Layout mirrors
//! the AtmosAlpha terminal: logo + status header, numbered tabs, a key-hint
//! footer and a `^p` command palette; mouse clicks work on tabs.

mod devices;
mod feed;
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
use courierqr_core::secrecy::ExposeSecret;

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
    /// Drop shadow under the header banner.
    pub const SHADOW: Color = Color::Rgb(52, 60, 72);
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
    pub(crate) gate: Option<Gate>,
    pub(crate) out: PathBuf,
}

/// Full-screen gate in front of the tabs: first-run account setup, or the
/// unlock prompt when the keys are encrypted at rest.
pub(crate) enum Gate {
    /// Keys are locked: type the account number (or a legacy recovery code).
    Unlock {
        input: String,
        error: Option<String>,
    },
    /// First run: pick new account / restore / no account.
    Welcome { selected: usize },
    /// Name this device (goes into key derivation).
    Label {
        input: String,
        restore: bool,
        error: Option<String>,
    },
    /// Restore: type the recovery seed.
    Seed {
        label: String,
        input: String,
        error: Option<String>,
    },
    /// Show the secrets once; type the account number back to commit.
    Confirm {
        pending: pair::PendingSetup,
        input: String,
        error: Option<String>,
    },
}

impl Gate {
    fn unlock() -> Self {
        Gate::Unlock {
            input: String::new(),
            error: None,
        }
    }
}

const WELCOME: [(&str, &str); 3] = [
    (
        "Create a new account",
        "New account number + recovery seed; keys derived for this device.",
    ),
    (
        "Restore from recovery seed",
        "Replacement device: same keys, paired devices still recognize you.",
    ),
    (
        "Continue without an account",
        "Random keys for this computer only. Not recoverable if lost.",
    ),
];

/// The system clipboard's text, via whichever helper is installed
/// (Wayland `wl-paste`, then X11 `xclip` / `xsel`). `None` if none works.
fn clipboard_text() -> Option<String> {
    use std::process::{Command, Stdio};
    const HELPERS: [&[&str]; 3] = [
        &["wl-paste", "--no-newline"],
        &["xclip", "-selection", "clipboard", "-o"],
        &["xsel", "--clipboard", "--output"],
    ];
    HELPERS.iter().find_map(|cmd| {
        let out = Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        (out.status.success() && !text.is_empty()).then_some(text)
    })
}

/// Group alphanumerics in fours with dashes, uppercased: `A3F9-C2E1-…`.
fn group4(input: &str) -> String {
    let chars: Vec<char> = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    chars
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// Same shape as [`group4`], but bullets.
fn masked4(input: &str) -> String {
    group4(input)
        .chars()
        .map(|c| if c == '-' { '-' } else { '•' })
        .collect()
}

/// Edit a gate text field: alphanumerics (plus `allow`) append up to `max`,
/// backspace deletes. Returns true if the key was consumed.
fn edit_field(field: &mut String, k: &KeyEvent, max: usize, allow: &[char]) -> bool {
    match k.code {
        KeyCode::Backspace => {
            field.pop();
            true
        }
        KeyCode::Char(c)
            if (c.is_ascii_alphanumeric() || allow.contains(&c)) && field.chars().count() < max =>
        {
            field.push(c);
            true
        }
        // Separators the user types out of habit are ignored (we regroup).
        KeyCode::Char('-' | ' ') => true,
        _ => false,
    }
}

/// This computer's public key + fingerprint, unlocking the vault if a code
/// is already available (env var).
fn load_own_key() -> Option<(String, String)> {
    // Don't let the CLI's stdin prompt fire from inside the TUI (the terminal
    // is already in raw mode): if the vault is locked and no code is in the
    // environment, return None so the in-app unlock gate handles it instead.
    if pair::vault_is_locked().unwrap_or(false) && pair::unlock_from_env().is_none() {
        return None;
    }
    pair::own_public_key_quiet()
        .ok()
        .and_then(|(pk, _)| courierqr_core::fingerprint(&pk).ok().map(|fp| (pk, fp)))
}

impl App {
    pub(crate) fn new(opts: Opts) -> Self {
        // First run (no identity yet): offer account setup rather than
        // silently generating standalone keys. On an I/O error, fall back
        // to the old behaviour.
        let first_run = !pair::has_identity().unwrap_or(true);
        let (own_key, gate) = if first_run {
            (None, Some(Gate::Welcome { selected: 0 }))
        } else {
            let locked = pair::vault_is_locked().unwrap_or(false);
            let own_key = load_own_key();
            // Locked, and we couldn't open it from the environment: prompt.
            let gate = (locked && own_key.is_none()).then(Gate::unlock);
            (own_key, gate)
        };
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
            gate,
            out: opts.out,
        }
    }

    /// Try a typed account number (or legacy recovery code). On success the
    /// keys load for the rest of the session; on failure, an error message.
    fn try_unlock(&mut self, typed: &str) -> std::result::Result<(), String> {
        // Thread the secret to pair::own_identity (and later auto-unlocks)
        // for this process only. Startup is single-threaded; no worker yet.
        unsafe { std::env::set_var(pair::UNLOCK_ENV[0], pair::unlock_passphrase(typed)) };
        match load_own_key() {
            Some(k) => {
                self.own_key = Some(k);
                Ok(())
            }
            None => {
                unsafe { std::env::remove_var(pair::UNLOCK_ENV[0]) };
                Err("Wrong account number — try again.".into())
            }
        }
    }

    /// Pasted text goes to whichever text field is active.
    pub(crate) fn paste(&mut self, text: &str) {
        if self.gate.is_some() {
            self.gate_paste(text);
        } else if self.tab == Tab::Send {
            send::on_paste(self, text);
        } else if self.tab == Tab::Devices && self.devices.wants_text() {
            for c in text.chars().filter(|c| !c.is_control()) {
                devices::on_key(self, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
        }
    }

    /// Ctrl+V. Most Linux terminals don't paste on Ctrl+V themselves (they
    /// pass the key through), so read the clipboard directly.
    fn paste_clipboard(&mut self) {
        match clipboard_text() {
            Some(t) => self.paste(&t),
            None => self.toast(
                "couldn't read the clipboard: use Ctrl+Shift+V or right-click → Paste \
                 (or install wl-clipboard / xclip)",
                true,
            ),
        }
    }

    /// Paste into a gate field, keeping only the characters it accepts
    /// (so a copied "A3F9-C2E1-…" or a trailing newline is fine).
    fn gate_paste(&mut self, text: &str) {
        const PLAIN: &[char] = &[];
        const LABEL: &[char] = &['-', '_', ' '];
        let Some(gate) = self.gate.as_mut() else {
            return;
        };
        let (field, max, allow): (&mut String, usize, &[char]) = match gate {
            Gate::Unlock { input, error } => {
                *error = None;
                (input, 25, PLAIN)
            }
            Gate::Label { input, error, .. } => {
                *error = None;
                (input, 32, LABEL)
            }
            Gate::Seed { input, error, .. } => {
                *error = None;
                (input, 32, PLAIN)
            }
            Gate::Confirm { input, error, .. } => {
                *error = None;
                (input, 16, PLAIN)
            }
            Gate::Welcome { .. } => return,
        };
        for c in text
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || allow.contains(c))
        {
            if field.chars().count() >= max {
                break;
            }
            field.push(c);
        }
    }

    fn gate_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if ctrl && k.code == KeyCode::Char('v') {
            self.paste_clipboard();
            return;
        }
        let Some(gate) = self.gate.take() else { return };
        self.gate = self.step_gate(gate, k);
    }

    /// One key through the gate state machine; returns the next state
    /// (`None` = gate done, show the tabs).
    fn step_gate(&mut self, gate: Gate, k: KeyEvent) -> Option<Gate> {
        match gate {
            Gate::Unlock {
                mut input,
                mut error,
            } => {
                match k.code {
                    KeyCode::Esc => self.quit = true,
                    KeyCode::Enter if !input.trim().is_empty() => match self.try_unlock(&input) {
                        Ok(()) => {
                            self.toast("keys unlocked", false);
                            return None;
                        }
                        Err(e) => {
                            error = Some(e);
                            input.clear();
                        }
                    },
                    _ => {
                        if edit_field(&mut input, &k, 25, &[]) {
                            error = None;
                        }
                    }
                }
                Some(Gate::Unlock { input, error })
            }
            Gate::Welcome { mut selected } => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
                    KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(WELCOME.len() - 1)
                    }
                    KeyCode::Char(c @ '1'..='3') => selected = (c as u8 - b'1') as usize,
                    KeyCode::Enter => {
                        return match selected {
                            0 | 1 => Some(Gate::Label {
                                input: "desktop".into(),
                                restore: selected == 1,
                                error: None,
                            }),
                            _ => match pair::own_public_key_quiet() {
                                Ok(_) => {
                                    self.own_key = load_own_key();
                                    self.toast(
                                        "standalone keys created — run `courierqr vault on` to encrypt them",
                                        false,
                                    );
                                    None
                                }
                                Err(e) => {
                                    self.toast(format!("couldn't create keys: {e:#}"), true);
                                    Some(Gate::Welcome { selected })
                                }
                            },
                        };
                    }
                    _ => {}
                }
                Some(Gate::Welcome { selected })
            }
            Gate::Label {
                mut input,
                restore,
                mut error,
            } => {
                match k.code {
                    KeyCode::Esc => {
                        return Some(Gate::Welcome {
                            selected: restore as usize,
                        });
                    }
                    KeyCode::Enter => {
                        if restore {
                            // Validate the label now so the error lands here.
                            match pair::check_label(&input) {
                                Ok(label) => {
                                    return Some(Gate::Seed {
                                        label,
                                        input: String::new(),
                                        error: None,
                                    });
                                }
                                Err(e) => error = Some(format!("{e:#}")),
                            }
                        } else {
                            match pair::PendingSetup::new(&input) {
                                Ok(pending) => {
                                    return Some(Gate::Confirm {
                                        pending,
                                        input: String::new(),
                                        error: None,
                                    });
                                }
                                Err(e) => error = Some(format!("{e:#}")),
                            }
                        }
                    }
                    _ => {
                        if edit_field(&mut input, &k, 32, &['-', '_', ' ']) {
                            error = None;
                        }
                    }
                }
                Some(Gate::Label {
                    input,
                    restore,
                    error,
                })
            }
            Gate::Seed {
                label,
                mut input,
                mut error,
            } => {
                match k.code {
                    KeyCode::Esc => {
                        return Some(Gate::Label {
                            input: label,
                            restore: true,
                            error: None,
                        });
                    }
                    KeyCode::Enter => match pair::PendingSetup::restore(&input, &label) {
                        Ok(pending) => {
                            return Some(Gate::Confirm {
                                pending,
                                input: String::new(),
                                error: None,
                            });
                        }
                        Err(e) => error = Some(format!("{e:#}")),
                    },
                    _ => {
                        if edit_field(&mut input, &k, 32, &[]) {
                            error = None;
                        }
                    }
                }
                Some(Gate::Seed {
                    label,
                    input,
                    error,
                })
            }
            Gate::Confirm {
                pending,
                mut input,
                mut error,
            } => {
                match k.code {
                    KeyCode::Esc => {
                        // Nothing has been written yet; going back discards
                        // these secrets.
                        let restore = pending.recovery_seed().is_none();
                        return Some(Gate::Label {
                            input: pending.label().to_owned(),
                            restore,
                            error: None,
                        });
                    }
                    KeyCode::Enter => {
                        if !pending.confirms(&input) {
                            error = Some("That doesn't match. Check what you wrote down.".into());
                            input.clear();
                        } else {
                            let pass = pending.passphrase();
                            let label = pending.label().to_owned();
                            match pending.commit(false) {
                                Ok(()) => {
                                    unsafe { std::env::set_var(pair::UNLOCK_ENV[0], &pass) };
                                    self.own_key = load_own_key();
                                    self.toast(
                                        format!("account ready — device \"{label}\" unlocked"),
                                        false,
                                    );
                                    return None;
                                }
                                Err(e) => {
                                    self.toast(format!("setup failed: {e:#}"), true);
                                    return Some(Gate::Welcome { selected: 0 });
                                }
                            }
                        }
                    }
                    _ => {
                        if edit_field(&mut input, &k, 16, &[]) {
                            error = None;
                        }
                    }
                }
                Some(Gate::Confirm {
                    pending,
                    input,
                    error,
                })
            }
        }
    }

    fn draw_gate(&self, f: &mut Frame, area: Rect) {
        let Some(gate) = self.gate.as_ref() else {
            return;
        };
        let muted = |s: String| Line::from(Span::styled(s, Style::new().fg(MUTED)));
        let text = |s: String| Line::from(Span::styled(s, Style::new().fg(TEXT)));
        let field = |label: &str, value: String| {
            Line::from(vec![
                Span::styled(format!("  {label:<8}"), Style::new().fg(MUTED)),
                Span::styled(value, Style::new().fg(ACCENT)),
                Span::styled("▏", Style::new().fg(ACCENT)),
            ])
        };
        let err_lines = |e: &Option<String>| -> Vec<Line<'static>> {
            e.as_ref()
                .map(|e| {
                    vec![
                        Line::raw(""),
                        Line::from(Span::styled(e.clone(), Style::new().fg(ERROR))),
                    ]
                })
                .unwrap_or_default()
        };
        let hint = |s: &str| {
            vec![
                Line::raw(""),
                Line::from(Span::styled(s.to_owned(), Style::new().fg(MUTED))),
            ]
        };

        let (title, width, mut lines): (&str, u16, Vec<Line>) = match gate {
            Gate::Unlock { input, error } => {
                let mut l = vec![
                    text("This computer's keys are encrypted at rest.".into()),
                    muted("Enter your account number to unlock them.".into()),
                    Line::raw(""),
                    field("account", masked4(input)),
                ];
                l.extend(err_lines(error));
                l.extend(hint(
                    "enter unlock · esc quit   (older vaults: your recovery code)",
                ));
                ("keys locked", 64, l)
            }
            Gate::Welcome { selected } => {
                let mut l = vec![
                    text("No keys on this computer yet. How do you want to start?".into()),
                    Line::raw(""),
                ];
                for (i, (name, desc)) in WELCOME.iter().enumerate() {
                    let on = i == *selected;
                    l.push(Line::from(vec![
                        Span::styled(if on { " ▸ " } else { "   " }, Style::new().fg(ACCENT)),
                        Span::styled(
                            format!("{}  {name}", i + 1),
                            if on {
                                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                            } else {
                                Style::new().fg(TEXT)
                            },
                        ),
                    ]));
                    l.push(muted(format!("      {desc}")));
                }
                l.extend(hint("↑↓ / 1-3 choose · enter continue · esc quit"));
                ("welcome", 76, l)
            }
            Gate::Label {
                input,
                restore,
                error,
            } => {
                let mut l = vec![
                    text("Name this device.".into()),
                    muted(if *restore {
                        "Use the same name as the device you're restoring, or the keys won't match."
                            .into()
                    } else {
                        "Each device gets its own keys from your seed + this name.".into()
                    }),
                    Line::raw(""),
                    field("device", input.clone()),
                ];
                l.extend(err_lines(error));
                l.extend(hint("enter continue · esc back"));
                (if *restore { "restore" } else { "new account" }, 80, l)
            }
            Gate::Seed {
                label,
                input,
                error,
            } => {
                let mut l = vec![
                    text(format!("Restoring device \"{label}\".")),
                    muted("Type your recovery seed (32 characters; dashes optional).".into()),
                    Line::raw(""),
                    field("seed", group4(input)),
                ];
                l.extend(err_lines(error));
                l.extend(hint("enter continue · esc back"));
                ("restore", 76, l)
            }
            Gate::Confirm {
                pending,
                input,
                error,
            } => {
                let key = |s: &str| Span::styled(format!("  {s:<16}"), Style::new().fg(MUTED));
                let val = |s: String| {
                    Span::styled(s, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
                };
                let mut l = vec![
                    Line::from(Span::styled(
                        "Write these down now. They are shown once.",
                        Style::new().fg(WARN).add_modifier(Modifier::BOLD),
                    )),
                    Line::raw(""),
                    Line::from(vec![key("account number"), val(pending.account_number())]),
                    muted("                    unlocks CourierQR on this computer".into()),
                ];
                if let Some(seed) = pending.recovery_seed() {
                    l.push(Line::from(vec![
                        key("recovery seed"),
                        val(seed.expose_secret().to_string()),
                    ]));
                    l.push(muted(
                        "                    the ONLY way to rebuild your keys on a new device"
                            .into(),
                    ));
                    l.push(Line::raw(""));
                    l.push(muted(
                        "Keep them apart, and off this computer. Anyone with the seed has your keys."
                            .into(),
                    ));
                } else {
                    l.push(Line::raw(""));
                    l.push(muted(format!(
                        "Device \"{}\" will get the same keys as before. This account number is new.",
                        pending.label()
                    )));
                }
                l.push(Line::raw(""));
                l.push(text(
                    "Type the account number to confirm you wrote it down:".into(),
                ));
                l.push(field("account", group4(input)));
                l.extend(err_lines(error));
                l.extend(hint("enter finish · esc back (discards these)"));
                ("save these", 84, l)
            }
        };
        lines.insert(0, Line::raw(""));
        let height = lines.len() as u16 + 2;
        let [_, mid, _] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .areas(area);
        let [_, center, _] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(width.min(area.width)),
            Constraint::Min(0),
        ])
        .areas(mid);
        let block = panel(title);
        let inner = block.inner(center);
        f.render_widget(Clear, center);
        f.render_widget(block, center);
        let pad = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(2),
            ..inner
        };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), pad);
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
                    // Right-click → Paste, Ctrl+Shift+V, middle-click: the
                    // terminal sends the text as one bracketed paste.
                    Event::Paste(text) => self.paste(&text),
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
        self.receive.feed.tick();
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
        // The setup/unlock gate swallows everything until it's done.
        if self.gate.is_some() {
            return self.gate_key(k);
        }
        if k.code == KeyCode::Char('v') && k.modifiers.contains(KeyModifiers::CONTROL) {
            self.paste_clipboard();
            return;
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

        if self.gate.is_some() {
            let logo = self.fitting_logo(area.width);
            let header_h = (logo.height().max(4) + 2) as u16;
            let [header, body] =
                Layout::vertical([Constraint::Length(header_h), Constraint::Min(4)]).areas(area);
            self.draw_header(f, header, logo);
            self.draw_gate(f, body);
            return;
        }

        // While transmitting, the codes need every row: shrink the header.
        let logo = if (self.tab == Tab::Send && self.send.live.is_some())
            || (self.tab == Tab::Receive && self.receive.showing_feed())
        {
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
            let banner = logo.banner_rows.contains(&row);
            let spans: Vec<Span> = l
                .chars()
                .enumerate()
                .map(|(i, ch)| {
                    let accent = i >= logo.accent_from || i < logo.accent_until;
                    let style = if accent {
                        Style::new().fg(ACCENT)
                    } else if banner {
                        match ch {
                            '▒' => Style::new().fg(SHADOW),
                            '═' | '║' | '╔' | '╗' | '╚' | '╝' => {
                                Style::new().fg(ACCENT)
                            }
                            _ => Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                        }
                    } else if muted {
                        Style::new().fg(MUTED)
                    } else {
                        Style::new().fg(TEXT)
                    };
                    Span::styled(ch.to_string(), style)
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
                Span::styled(format!("{k:<12}"), Style::new().fg(MUTED)),
                v,
            ])
        };
        let status = vec![
            kv(
                "this device",
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
        // Top-right corner, flush right: size the block to its widest line
        // so labels and values stay in columns.
        let w = (status.iter().map(Line::width).max().unwrap_or(0) as u16 + 1).min(right.width);
        let corner = Rect {
            x: right.x + right.width - w,
            y: right.y,
            width: w,
            height: (status.len() as u16).min(right.height),
        };
        f.render_widget(Paragraph::new(status).style(Style::new().bg(BG)), corner);
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
