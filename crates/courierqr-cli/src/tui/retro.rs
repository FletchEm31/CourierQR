//! Retro (Win9x-style) chrome for the terminal UI: colour themes, bevelled
//! group boxes, the title bar, bevelled tabs and the status bar.
//!
//! Everything reads its colours from [`t()`], the active [`Palette`], so a
//! theme switch repaints the whole UI on the next frame. The palettes match
//! the design mockup (Midnight, Classic, Amber, Phosphor, Synthwave).
//!
//! A terminal can't draw 1-px bevels, so a bevel is a box-drawing frame in
//! two tones: light on the top/left and dark on the bottom/right for a
//! raised surface, the other way round for a sunken one.

use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// One colour theme.
pub struct Palette {
    pub name: &'static str,
    /// Window face (most of the screen).
    pub face: Color,
    /// Bevel highlight (top/left of raised things).
    pub hi: Color,
    /// Slightly lighter than the face: inactive tabs, the key-hint strip.
    pub light: Color,
    /// Bevel shadow (bottom/right of raised things).
    pub shadow: Color,
    pub text: Color,
    pub dim: Color,
    /// Sunken fields: the logo well, status-bar cells.
    pub field: Color,
    /// Title bar gradient, left to right, and its text.
    pub t1: Color,
    pub t2: Color,
    pub t_text: Color,
    /// Highlight colour (the "QR" in the logo, focus rings, selections).
    pub accent: Color,
    /// The logo's drop shadow.
    pub logo_shadow: Color,
    pub warn: Color,
    pub error: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const THEMES: [Palette; 5] = [
    Palette {
        name: "Midnight",
        face: rgb(0x2a2c31),
        hi: rgb(0x5a5e68),
        light: rgb(0x3a3d44),
        shadow: rgb(0x141518),
        text: rgb(0xececec),
        dim: rgb(0xb4b8bf),
        field: rgb(0x111214),
        // The mockup's "Mint" title bar: deep green into jade, white text.
        t1: rgb(0x053a2a),
        t2: rgb(0x127a55),
        t_text: rgb(0xffffff),
        accent: rgb(0x3ddc97),
        logo_shadow: rgb(0x343c48),
        warn: rgb(0xffb84d),
        error: rgb(0xff6b6b),
    },
    Palette {
        name: "Classic",
        face: rgb(0xc0c0c0),
        hi: rgb(0xffffff),
        light: rgb(0xdfdfdf),
        shadow: rgb(0x808080),
        text: rgb(0x000000),
        dim: rgb(0x3d3d3d),
        field: rgb(0xffffff),
        t1: rgb(0x000080),
        t2: rgb(0x1084d0),
        t_text: rgb(0xffffff),
        accent: rgb(0x000080),
        logo_shadow: rgb(0x9aa7b8),
        warn: rgb(0x806000),
        error: rgb(0xc00000),
    },
    Palette {
        name: "Amber",
        face: rgb(0x1c1407),
        hi: rgb(0x6b4a12),
        light: rgb(0x3a2a0c),
        shadow: rgb(0x000000),
        text: rgb(0xffb000),
        dim: rgb(0xd99a2b),
        field: rgb(0x0d0903),
        t1: rgb(0x3a2200),
        t2: rgb(0x8a5a00),
        t_text: rgb(0xffd27a),
        accent: rgb(0xffd27a),
        logo_shadow: rgb(0x5a3300),
        warn: rgb(0xff7a3a),
        error: rgb(0xff4d3a),
    },
    Palette {
        name: "Phosphor",
        face: rgb(0x0c160c),
        hi: rgb(0x2c5a2c),
        light: rgb(0x173017),
        shadow: rgb(0x000000),
        text: rgb(0x4dff7a),
        dim: rgb(0x3fcf66),
        field: rgb(0x050a05),
        t1: rgb(0x003a10),
        t2: rgb(0x00872a),
        t_text: rgb(0xd6ffe0),
        accent: rgb(0xb6ffc9),
        logo_shadow: rgb(0x003a10),
        warn: rgb(0xe6ff4d),
        error: rgb(0xff5a5a),
    },
    Palette {
        name: "Synthwave",
        face: rgb(0x1d1233),
        hi: rgb(0x5a3f8f),
        light: rgb(0x2d1d4f),
        shadow: rgb(0x0a0614),
        text: rgb(0xf4e9ff),
        dim: rgb(0xc9b6ee),
        field: rgb(0x110a20),
        t1: rgb(0xff2bd6),
        t2: rgb(0x3a2bff),
        t_text: rgb(0xffffff),
        accent: rgb(0x00e5ff),
        logo_shadow: rgb(0xff2bd6),
        warn: rgb(0xffd84d),
        error: rgb(0xff5a7a),
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The active theme.
pub fn t() -> &'static Palette {
    &THEMES[CURRENT.load(Ordering::Relaxed) % THEMES.len()]
}

/// Switch to the next theme and remember it. Returns its name.
pub fn cycle() -> &'static str {
    let next = (CURRENT.load(Ordering::Relaxed) + 1) % THEMES.len();
    CURRENT.store(next, Ordering::Relaxed);
    save();
    THEMES[next].name
}

fn theme_file() -> Option<std::path::PathBuf> {
    crate::pair::config_dir().ok().map(|d| d.join("ui-theme"))
}

/// Load the saved theme (by name); unknown or missing keeps the default.
pub fn load() {
    let Some(name) = theme_file().and_then(|f| std::fs::read_to_string(f).ok()) else {
        return;
    };
    if let Some(i) = THEMES
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case(name.trim()))
    {
        CURRENT.store(i, Ordering::Relaxed);
    }
}

fn save() {
    if let Some(f) = theme_file() {
        let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f));
        let _ = std::fs::write(f, t().name);
    }
}

// ---------------------------------------------------------------- bevels

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Etched group box (Win9x "frame"): dark top/left, light bottom/right.
    Etched,
    /// Raised: light top/left, dark bottom/right.
    #[allow(dead_code)] // for buttons/dialogs in the next phases
    Raised,
    /// Sunken well: dark top/left, light bottom/right, field-coloured inside.
    Sunken,
}

/// Draw a two-tone box frame around `area` (single-line, square corners).
pub fn frame(buf: &mut Buffer, area: Rect, edge: Edge, focus: Option<Color>) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let p = t();
    let (tl, br) = match (focus, edge) {
        (Some(c), _) => (c, c),
        (None, Edge::Raised) => (p.hi, p.shadow),
        (None, Edge::Etched | Edge::Sunken) => (p.shadow, p.hi),
    };
    let (x0, y0) = (area.x, area.y);
    let (x1, y1) = (area.right() - 1, area.bottom() - 1);
    let mut put = |x: u16, y: u16, s: &str, c: Color| {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(s).set_fg(c);
        }
    };
    for x in x0 + 1..x1 {
        put(x, y0, "─", tl);
        put(x, y1, "─", br);
    }
    for y in y0 + 1..y1 {
        put(x0, y, "│", tl);
        put(x1, y, "│", br);
    }
    put(x0, y0, "┌", tl);
    put(x1, y0, "┐", tl);
    put(x0, y1, "└", tl);
    put(x1, y1, "┘", br);
}

/// Fill `area` with a background colour (and the theme text colour).
pub fn fill(buf: &mut Buffer, area: Rect, bg: Color) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_bg(bg).set_fg(t().text);
            }
        }
    }
}

/// Write `s` at (x, y) with `style`, clipped to `max` columns. Returns the
/// column after the last character written.
pub fn put_str(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, max: u16) -> u16 {
    let (end, _) = buf.set_stringn(x, y, s, max as usize, style);
    end
}

/// A titled panel: Win9x group box by default. Drop-in for the old rounded
/// `Block` (`inner`, `border_style`, render with `render_widget`).
pub struct Panel<'a> {
    title: &'a str,
    edge: Edge,
    focus: Option<Color>,
}

impl<'a> Panel<'a> {
    pub fn new(title: &'a str) -> Self {
        Panel {
            title,
            edge: Edge::Etched,
            focus: None,
        }
    }
    pub fn edge(mut self, edge: Edge) -> Self {
        self.edge = edge;
        self
    }
    /// Old API: a border colour. The theme's plain border colours mean "no
    /// focus"; anything else draws a focus ring in that colour.
    pub fn border_style(mut self, s: Style) -> Self {
        let p = t();
        self.focus = s.fg.filter(|c| *c != p.hi && *c != p.shadow);
        self
    }
    pub fn inner(&self, area: Rect) -> Rect {
        Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        }
    }
}

impl Widget for Panel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let p = t();
        let bg = if self.edge == Edge::Sunken {
            p.field
        } else {
            p.face
        };
        fill(buf, area, bg);
        frame(buf, area, self.edge, self.focus);
        if !self.title.is_empty() && area.width > 6 {
            let style = Style::new()
                .fg(self.focus.unwrap_or(p.text))
                .bg(p.face)
                .add_modifier(Modifier::BOLD);
            put_str(
                buf,
                area.x + 2,
                area.y,
                &format!(" {} ", self.title),
                style,
                area.width - 4,
            );
        }
    }
}

/// A bevelled push-button label drawn on one row: `▐ label ▌` style with
/// the face lighter than the window so it reads as raised.
pub fn button(buf: &mut Buffer, x: u16, y: u16, label: &str, active: bool) -> Rect {
    let p = t();
    let style = if active {
        Style::new()
            .fg(p.face)
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(p.text).bg(p.light)
    };
    let text = format!(" {label} ");
    let w = text.chars().count() as u16;
    put_str(buf, x, y, &text, style, w);
    // A one-cell dark edge on the right reads as the bevel's shadow side.
    put_str(buf, x + w, y, "▌", Style::new().fg(p.shadow).bg(p.face), 1);
    Rect::new(x, y, w + 1, 1)
}

/// Linear blend between two RGB colours (`f` from 0.0 to 1.0).
pub fn blend(a: Color, b: Color, f: f32) -> Color {
    let (ar, ag, ab) = super::rgb(a);
    let (br, bg, bb) = super::rgb(b);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f.clamp(0.0, 1.0)).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// The title bar background: the theme's gradient, stepped per column.
pub fn title_gradient(buf: &mut Buffer, area: Rect) {
    let p = t();
    let w = area.width.max(2) as f32 - 1.0;
    for x in area.left()..area.right() {
        let c = blend(p.t1, p.t2, (x - area.x) as f32 / w);
        for y in area.top()..area.bottom() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_bg(c).set_fg(p.t_text);
            }
        }
    }
}
