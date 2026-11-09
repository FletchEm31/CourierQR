//! The terminal header's wordmark, plus smaller fallbacks for narrow windows.
//!
//! The big "QR" is the ANSI Shadow letterforms scaled 2× (from the shadowless
//! ANSI Regular glyphs, re-shadowed with the box-drawing rule learned from the
//! Shadow font itself), so its outline matches COURIER exactly.
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
    /// Rows drawn muted (a tagline), left of `accent_from`.
    pub muted_rows: &'static [usize],
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

/// The line under COURIER (heavy rule). Alternatives that fit (53 columns):
/// "encrypted over light", "air-gapped · end-to-end encrypted",
/// "your keys stay home · only light crosses",
/// "post-quantum encrypted · carried by light".
pub const LOGOS: &[Logo] = &[
    Logo {
        name: "Courier QR",
        about: "Shadow COURIER with a double-height QR hanging below, and a tagline",
        lines: &[
            r#" ██████╗ ██████╗ ██╗   ██╗██████╗ ██╗███████╗██████╗     ████████████╗   ████████████╗"#,
            r#"██╔════╝██╔═══██╗██║   ██║██╔══██╗██║██╔════╝██╔══██╗    ████████████║   ████████████║"#,
            r#"██║     ██║   ██║██║   ██║██████╔╝██║█████╗  ██████╔╝  ████╔═══════████╗ ████╔═════████╗"#,
            r#"██║     ██║   ██║██║   ██║██╔══██╗██║██╔══╝  ██╔══██╗  ████║       ████║ ████║     ████║"#,
            r#"╚██████╗╚██████╔╝╚██████╔╝██║  ██║██║███████╗██║  ██║  ████║       ████║ ████████████╔═╝"#,
            r#" ╚═════╝ ╚═════╝  ╚═════╝ ╚═╝  ╚═╝╚═╝╚══════╝╚═╝  ╚═╝  ████║       ████║ ████████████║"#,
            r#"                                                       ████║       ████║ ████╔═════████╗"#,
            r#"━━━━━━ no network · no accounts · no metadata ━━━━━━━  ████║ ████╗ ████║ ████║     ████║"#,
            r#"                                                       ╚═████████████╔═╝ ████║     ████║"#,
            r#"                                                         ████████████║   ████║     ████║"#,
            r#"                                                         ╚═════████══╝   ╚═══╝     ╚═══╝"#,
            r#"                                                               ╚═══╝"#,
        ],
        accent_from: 55,
        accent_until: 0,
        muted_rows: &[7],
    },
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
        muted_rows: &[],
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
        muted_rows: &[3],
    },
    Logo {
        name: "Minimal",
        about: "one quiet line, for small windows",
        lines: &[r#"▣ courier·qr  ─  files over light"#],
        accent_from: 9999,
        muted_rows: &[],
        accent_until: 1,
    },
];

/// Smallest logo that fits; used when the chosen one is too big.
/// The big Courier QR mark is the logo; Shadow, Finder and Minimal are
/// fallbacks for narrower terminals.
pub const COMPACT: usize = 3;
