//! Logo options for the terminal header. Press `l` to cycle; the choice is
//! remembered in the config directory.
//!
//! `accent_from` is the column where the "QR" part starts, drawn in the
//! accent colour (the Finder mark is accent too).

#[allow(dead_code)] // name/about document each design; only the art is drawn
pub struct Logo {
    pub name: &'static str,
    pub about: &'static str,
    pub lines: &'static [&'static str],
    /// Columns at or after this are drawn in the accent colour.
    pub accent_from: usize,
    /// Columns before this are drawn in the accent colour too (the Finder mark).
    pub accent_until: usize,
}

impl Logo {
    pub fn width(&self) -> usize {
        self.lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
    }
    pub fn height(&self) -> usize {
        self.lines.len()
    }
}

pub const LOGOS: &[Logo] = &[
    Logo {
        name: "Shadow",
        about: "bold block letters with a drop shadow",
        lines: &[
            r#" ██████╗ ██████╗ ██╗   ██╗██████╗ ██╗███████╗██████╗  ██████╗ ██████╗"#,
            r#"██╔════╝██╔═══██╗██║   ██║██╔══██╗██║██╔════╝██╔══██╗██╔═══██╗██╔══██╗"#,
            r#"██║     ██║   ██║██║   ██║██████╔╝██║█████╗  ██████╔╝██║   ██║██████╔╝"#,
            r#"██║     ██║   ██║██║   ██║██╔══██╗██║██╔══╝  ██╔══██╗██║▄▄ ██║██╔══██╗"#,
            r#"╚██████╗╚██████╔╝╚██████╔╝██║  ██║██║███████╗██║  ██║╚██████╔╝██║  ██║"#,
            r#" ╚═════╝ ╚═════╝  ╚═════╝ ╚═╝  ╚═╝╚═╝╚══════╝╚═╝  ╚═╝ ╚══▀▀═╝ ╚═╝  ╚═╝"#,
        ],
        accent_from: 52,
        accent_until: 0,
    },
    Logo {
        name: "Finder",
        about: "a QR finder-pattern mark beside a compact wordmark",
        lines: &[
            r#"█▀▀▀▀▀█  ╔═╗┌─┐┬ ┬┬─┐┬┌─┐┬─┐╔═╗ ╦═╗"#,
            r#"█ ███ █  ║  │ ││ │├┬┘│├┤ ├┬┘║═╬╗╠╦╝"#,
            r#"█ ▀▀▀ █  ╚═╝└─┘└─┘┴└─┴└─┘┴└─╚═╝╚╩╚═"#,
            r#"▀▀▀▀▀▀▀  files over light"#,
        ],
        accent_from: 27,
        accent_until: 7,
    },
    Logo {
        name: "Minimal",
        about: "one quiet line, for small windows",
        lines: &[r#"▣ courier·qr  ─  files over light"#],
        accent_from: 9999,
        accent_until: 1,
    },
];

/// Smallest logo that fits; used when the chosen one is too big.
/// Shadow is the logo; Finder and Minimal only appear on narrow terminals.
pub const COMPACT: usize = 2;
