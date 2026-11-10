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
    /// Rows forming the boxed tagline banner, left of `accent_from`: frame
    /// muted, text bold and bright, `█`/`▀` drawn as a dark half-block drop shadow.
    pub banner_rows: &'static [usize],
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

/// The banner under COURIER: a double-line box (53 columns) with a drop
/// shadow, like the letters above it. Other taglines that fit (≤51):
/// "ENCRYPTED OVER LIGHT", "AIR-GAPPED · END-TO-END ENCRYPTED",
/// "YOUR KEYS STAY HOME · ONLY LIGHT CROSSES",
/// "POST-QUANTUM ENCRYPTED · CARRIED BY LIGHT".
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
            r#"╔═══════════════════════════════════════════════════╗  ████║       ████║ ████╔═════████╗"#,
            r#"║   NO NETWORK · ANONYMOUS ACCOUNTS · NO METADATA   ║█ ████║ ████╗ ████║ ████║     ████║"#,
            r#"╚═══════════════════════════════════════════════════╝█ ╚═████████████╔═╝ ████║     ████║"#,
            r#" ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀   ████████████║   ████║     ████║"#,
            r#"                                                         ╚═════████══╝   ╚═══╝     ╚═══╝"#,
            r#"                                                               ╚═══╝"#,
        ],
        accent_from: 55,
        accent_until: 0,
        muted_rows: &[],
        banner_rows: &[6, 7, 8, 9],
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
        banner_rows: &[],
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
        banner_rows: &[],
    },
    Logo {
        name: "Minimal",
        about: "one quiet line, for small windows",
        lines: &[r#"▣ courier·qr  ─  files over light"#],
        accent_from: 9999,
        muted_rows: &[],
        accent_until: 1,
        banner_rows: &[],
    },
];

/// Smallest logo that fits; used when the chosen one is too big.
/// The big Courier QR mark is the logo; Shadow, Finder and Minimal are
/// fallbacks for narrower terminals.
pub const COMPACT: usize = 3;
