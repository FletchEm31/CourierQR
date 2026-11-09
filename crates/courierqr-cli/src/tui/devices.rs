//! Devices tab: paired phones (with fingerprints), pairing a new one, and
//! this computer's own key as a scannable QR code.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::theme::*;
use super::{App, panel};
use crate::pair;

pub(crate) enum Mode {
    Browse,
    /// Typing a new device: which field has focus (0 = name, 1 = key).
    Add {
        name: String,
        key: String,
        field: usize,
        error: Option<String>,
    },
    /// Key parsed: confirm the fingerprint matches the phone.
    Confirm {
        name: String,
        key: String,
        fp: String,
    },
}

pub(crate) struct DevicesTab {
    pub(crate) list: Vec<(String, String, String)>, // name, key, fingerprint
    pub(crate) selected: usize,
    pub(crate) mode: Mode,
    /// Second press of `d` within this window deletes.
    delete_armed: Option<std::time::Instant>,
}

impl DevicesTab {
    pub(crate) fn new() -> Self {
        let mut t = DevicesTab {
            list: Vec::new(),
            selected: 0,
            mode: Mode::Browse,
            delete_armed: None,
        };
        t.reload();
        t
    }

    pub(crate) fn reload(&mut self) {
        self.list = pair::load()
            .unwrap_or_default()
            .into_iter()
            .map(|(n, k)| {
                let fp = courierqr_core::fingerprint(&k).unwrap_or_else(|_| "INVALID".into());
                (n, k, fp)
            })
            .collect();
        self.selected = self.selected.min(self.list.len().saturating_sub(1));
    }

    pub(crate) fn wants_text(&self) -> bool {
        !matches!(self.mode, Mode::Browse)
    }

    pub(crate) fn begin_add(&mut self) {
        let name = if self.list.iter().any(|d| d.0 == "phone") {
            String::new()
        } else {
            "phone".into()
        };
        let field = if name.is_empty() { 0 } else { 1 };
        self.mode = Mode::Add {
            name,
            key: String::new(),
            field,
            error: None,
        };
    }
}

pub(crate) fn on_key(app: &mut App, k: KeyEvent) {
    let d = &mut app.devices;
    match &mut d.mode {
        Mode::Browse => match k.code {
            KeyCode::Up => d.selected = d.selected.saturating_sub(1),
            KeyCode::Down => d.selected = (d.selected + 1).min(d.list.len().saturating_sub(1)),
            KeyCode::Char('a') => d.begin_add(),
            KeyCode::Char('r') => {
                d.reload();
                app.toast("reloaded paired devices", false);
            }
            KeyCode::Char('d') => {
                let Some((name, ..)) = d.list.get(d.selected).cloned() else {
                    return;
                };
                let armed = d.delete_armed.is_some_and(|t| t.elapsed().as_secs() < 4);
                if armed {
                    d.delete_armed = None;
                    match pair::remove_quiet(&name) {
                        Ok(()) => {
                            d.reload();
                            app.toast(format!("forgot {name}"), false);
                        }
                        Err(e) => app.toast(e.to_string(), true),
                    }
                } else {
                    d.delete_armed = Some(std::time::Instant::now());
                    app.toast(format!("press d again to forget {name}"), false);
                }
            }
            _ => {}
        },
        Mode::Add {
            name,
            key,
            field,
            error,
        } => match k.code {
            KeyCode::Esc => d.mode = Mode::Browse,
            KeyCode::Tab | KeyCode::Up | KeyCode::Down => *field = 1 - *field,
            KeyCode::Backspace => {
                if *field == 0 {
                    name.pop()
                } else {
                    key.pop()
                };
            }
            KeyCode::Char(c) => {
                *error = None;
                if *field == 0 {
                    name.push(c)
                } else {
                    key.push(c)
                }
            }
            KeyCode::Enter => {
                if *field == 0 && key.is_empty() {
                    *field = 1;
                    return;
                }
                match pair::check(name.trim(), key.trim()) {
                    Ok((pk, fp)) => {
                        d.mode = Mode::Confirm {
                            name: name.trim().to_owned(),
                            key: pk,
                            fp,
                        }
                    }
                    Err(e) => *error = Some(format!("{e:#}")),
                }
            }
            _ => {}
        },
        Mode::Confirm { name, key, .. } => {
            match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    let (name, key) = (name.clone(), key.clone());
                    d.mode = Mode::Browse;
                    match pair::add_quiet(&name, &key) {
                        Ok(fp) => {
                            d.reload();
                            d.selected = d.list.iter().position(|x| x.0 == name).unwrap_or(0);
                            app.toast(format!("paired {name} ({fp}). Send with: courierqr send <file> --to {name}"), false);
                        }
                        Err(e) => app.toast(e.to_string(), true),
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    d.mode = Mode::Browse;
                    app.toast("not paired. If the fingerprints differ, the key was changed on the way here", true);
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    match app.devices.mode {
        Mode::Browse => vec![("a", "pair a phone"), ("d d", "forget"), ("↑↓", "select")],
        Mode::Add { .. } => vec![
            ("tab", "next field"),
            ("enter", "check key"),
            ("esc", "cancel"),
        ],
        Mode::Confirm { .. } => vec![("y", "it matches"), ("n", "it doesn't")],
    }
}

pub(crate) fn draw(app: &App, f: &mut Frame, area: Rect) {
    let d = &app.devices;
    let [left, right] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(50)]).areas(area);

    // Paired devices (computer -> phone)
    let block = panel("phones this computer sends to");
    let inner = block.inner(left);
    f.render_widget(block, left);
    let [list_area, help] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(5)]).areas(inner);
    if d.list.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::from(Span::styled("No phones paired yet.", Style::new().fg(TEXT))),
                Line::from(Span::styled(
                    "Press a, then paste the key from the phone's Keys tab.",
                    Style::new().fg(MUTED),
                )),
            ]),
            list_area,
        );
    } else {
        let items: Vec<ListItem> = d
            .list
            .iter()
            .map(|(n, k, fp)| {
                let short = format!(
                    "{}…{}",
                    &k[..12.min(k.len())],
                    &k[k.len().saturating_sub(6)..]
                );
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {n:<14}"),
                        Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{fp}  "), Style::new().fg(ACCENT)),
                    Span::styled(short, Style::new().fg(MUTED)),
                ]))
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(d.selected));
        f.render_stateful_widget(
            List::new(items)
                .highlight_style(Style::new().bg(PANEL))
                .highlight_symbol("›"),
            list_area,
            &mut state,
        );
    }
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Computer → phone. On the phone: Keys → This phone's key",
                Style::new().fg(MUTED),
            )),
            Line::from(Span::styled(
                "shows a key and fingerprint. Add it here (a), and only",
                Style::new().fg(MUTED),
            )),
            Line::from(Span::styled(
                "confirm if both screens show the same fingerprint.",
                Style::new().fg(MUTED),
            )),
        ])
        .wrap(Wrap { trim: true }),
        help,
    );

    // This computer (phone -> computer)
    let block = panel("this computer · phone → computer");
    let rin = block.inner(right);
    f.render_widget(block, right);
    match &app.own_key {
        Some((pk, fp)) => {
            let qr = crate::render::encode_smallest(pk.as_bytes(), qrcode::EcLevel::M).ok();
            let qr_h = qr
                .as_ref()
                .map(|c| (c.width() + 8).div_ceil(2) as u16)
                .unwrap_or(0);
            let [q, t] =
                Layout::vertical([Constraint::Length(qr_h), Constraint::Min(4)]).areas(rin);
            if let Some(code) = qr {
                draw_qr(f, q, &code);
            }
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled("fingerprint  ", Style::new().fg(MUTED)),
                        Span::styled(
                            fp.clone(),
                            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(Span::styled(pk.clone(), Style::new().fg(MUTED))),
                    Line::raw(""),
                    Line::from(Span::styled(
                        "On the phone: Keys → Scan computer's key, and check the fingerprint.",
                        Style::new().fg(TEXT),
                    )),
                ])
                .wrap(Wrap { trim: true }),
                t,
            );
        }
        None => f.render_widget(
            Paragraph::new(Span::styled(
                "couldn't load or create this computer's key",
                Style::new().fg(ERROR),
            )),
            rin,
        ),
    }

    // Add / confirm dialog
    let dialog = |f: &mut Frame, h: u16, title: &str| {
        let w = 70.min(area.width.saturating_sub(4));
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + 2, w, h);
        f.render_widget(Clear, r);
        let b = panel(title).border_style(Style::new().fg(ACCENT));
        let i = b.inner(r);
        f.render_widget(b, r);
        i
    };
    match &d.mode {
        Mode::Add {
            name,
            key,
            field,
            error,
        } => {
            let i = dialog(f, 10, "pair a phone");
            let fld = |label: &str, v: &str, active: bool| {
                Line::from(vec![
                    Span::styled(format!("{label:<6}"), Style::new().fg(MUTED)),
                    Span::styled(v.to_owned(), Style::new().fg(TEXT)),
                    Span::styled(if active { "▏" } else { "" }, Style::new().fg(ACCENT)),
                ])
            };
            let mut lines = vec![
                Line::from(Span::styled(
                    "Paste the age1… key from the phone (Keys tab).",
                    Style::new().fg(MUTED),
                )),
                Line::raw(""),
                fld("name", name, *field == 0),
                fld("key", key, *field == 1),
            ];
            if let Some(e) = error {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(e.clone(), Style::new().fg(ERROR))));
            }
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), i);
        }
        Mode::Confirm { name, fp, .. } => {
            let i = dialog(f, 9, "confirm fingerprint");
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!("Does {name} show exactly this fingerprint?"),
                        Style::new().fg(TEXT),
                    )),
                    Line::raw(""),
                    Line::from(Span::styled(
                        fp.clone(),
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ))
                    .centered(),
                    Line::raw(""),
                    Line::from(Span::styled(
                        "y  yes, pair it      n  no, cancel",
                        Style::new().fg(MUTED),
                    ))
                    .centered(),
                ]),
                i,
            );
        }
        Mode::Browse => {}
    }
}

/// QR code with a 4-module quiet zone, two modules per cell, black on white.
fn draw_qr(f: &mut Frame, area: Rect, code: &qrcode::QrCode) {
    let n = code.width();
    let colors = code.to_colors();
    let q = 4isize;
    let total = n as isize + 2 * q;
    let (w, h) = (total as u16, (total as u16).div_ceil(2));
    let ox = area.x + area.width.saturating_sub(w) / 2;
    let oy = area.y;
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
