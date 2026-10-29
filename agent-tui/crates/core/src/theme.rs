//! The `[theme]` table: which colours the TUI uses for each role.
//!
//! ```toml
//! [theme]
//! accent = "#89b4fa"
//! dim = "faint"
//! warning = "yellow"
//! ```

use serde::Deserialize;

/// One of the terminal's 16 palette colours, so it follows the user's theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ansi {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

impl Ansi {
    const NAMES: [(&'static str, Self); 16] = [
        ("black", Self::Black),
        ("red", Self::Red),
        ("green", Self::Green),
        ("yellow", Self::Yellow),
        ("blue", Self::Blue),
        ("magenta", Self::Magenta),
        ("cyan", Self::Cyan),
        ("white", Self::White),
        ("bright-black", Self::BrightBlack),
        ("bright-red", Self::BrightRed),
        ("bright-green", Self::BrightGreen),
        ("bright-yellow", Self::BrightYellow),
        ("bright-blue", Self::BrightBlue),
        ("bright-magenta", Self::BrightMagenta),
        ("bright-cyan", Self::BrightCyan),
        ("bright-white", Self::BrightWhite),
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    /// `weight` of `self` and the rest of `other`, per channel.
    #[must_use]
    pub fn mix(self, other: Self, weight: f32) -> Self {
        let channel = |a: u8, b: u8| {
            let mixed = f32::from(a) * weight + f32::from(b) * (1.0 - weight);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0-255 first"
            )]
            let byte = mixed.round().clamp(0.0, 255.0) as u8;
            byte
        };
        Self {
            r: channel(self.r, other.r),
            g: channel(self.g, other.g),
            b: channel(self.b, other.b),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// The terminal's own foreground.
    Default,
    Ansi(Ansi),
    Rgb(Rgb),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} isn't a colour (use #rrggbb, default, or an ANSI name like cyan or bright-cyan)")]
pub struct InvalidColour(String);

impl std::str::FromStr for Colour {
    type Err = InvalidColour;

    fn from_str(s: &str) -> Result<Self, InvalidColour> {
        let invalid = || InvalidColour(s.to_string());
        let lower = s.trim().to_ascii_lowercase();
        if let Some(hex) = lower.strip_prefix('#') {
            let channel = |at: usize| {
                hex.get(at..at + 2)
                    .filter(|pair| pair.bytes().all(|b| b.is_ascii_hexdigit()))
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            };
            return match (hex.len(), channel(0), channel(2), channel(4)) {
                (6, Some(r), Some(g), Some(b)) => Ok(Self::Rgb(Rgb { r, g, b })),
                _ => Err(invalid()),
            };
        }
        if lower == "default" {
            return Ok(Self::Default);
        }
        Ansi::NAMES
            .iter()
            .find(|(name, _)| *name == lower)
            .map(|(_, ansi)| Self::Ansi(*ansi))
            .ok_or_else(invalid)
    }
}

/// How secondary text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dim {
    /// Blend the terminal's foreground into its background when the
    /// terminal reports them, else [`Dim::Faint`].
    #[default]
    Auto,
    /// The terminal's faint attribute (SGR 2).
    Faint,
    Colour(Colour),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Focused borders and titles, the selection gutter, badges.
    pub accent: Colour,
    pub dim: Dim,
    /// Asking you, shared terminals, files other agents changed.
    pub warning: Colour,
    /// Failures, errors, removed lines.
    pub error: Colour,
    /// Finished turns, added lines.
    pub success: Colour,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Colour::Ansi(Ansi::Cyan),
            dim: Dim::Auto,
            warning: Colour::Ansi(Ansi::Yellow),
            error: Colour::Ansi(Ansi::Red),
            success: Colour::Ansi(Ansi::Green),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThemeError {
    #[error("theme.{field}: {source}")]
    Colour {
        field: &'static str,
        source: InvalidColour,
    },
    #[error(
        "theme.dim: {0:?} isn't faint or a colour (use #rrggbb, default, or an ANSI name like cyan or bright-cyan)"
    )]
    Dim(String),
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawTheme {
    accent: Option<String>,
    dim: Option<String>,
    warning: Option<String>,
    error: Option<String>,
    success: Option<String>,
}

impl RawTheme {
    pub(crate) fn parse(self) -> Result<Theme, ThemeError> {
        let defaults = Theme::default();
        let colour = |field: &'static str, raw: Option<String>, default: Colour| match raw {
            Some(raw) => raw
                .parse()
                .map_err(|source| ThemeError::Colour { field, source }),
            None => Ok(default),
        };
        let dim = match self.dim {
            None => defaults.dim,
            Some(raw) if raw.trim().eq_ignore_ascii_case("faint") => Dim::Faint,
            Some(raw) => Dim::Colour(raw.parse().map_err(|_| ThemeError::Dim(raw))?),
        };
        Ok(Theme {
            accent: colour("accent", self.accent, defaults.accent)?,
            dim,
            warning: colour("warning", self.warning, defaults.warning)?,
            error: colour("error", self.error, defaults.error)?,
            success: colour("success", self.success, defaults.success)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Colour {
        Colour::Rgb(Rgb { r, g, b })
    }

    #[test]
    fn colours_parse_from_hex_ansi_names_and_default() {
        let parse = |s: &str| s.parse::<Colour>();
        assert_eq!(parse("#89b4fa"), Ok(rgb(0x89, 0xb4, 0xfa)));
        assert_eq!(parse("#89B4FA"), Ok(rgb(0x89, 0xb4, 0xfa)));
        assert_eq!(parse("cyan"), Ok(Colour::Ansi(Ansi::Cyan)));
        assert_eq!(parse("Bright-Black"), Ok(Colour::Ansi(Ansi::BrightBlack)));
        assert_eq!(parse("default"), Ok(Colour::Default));
        for bad in [
            "#89b4f",
            "#89b4fa0",
            "#gg0000",
            "#+1+1+1",
            "mauve",
            "",
            "brightcyan",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn unset_slots_keep_the_ansi_defaults() {
        let theme = RawTheme {
            accent: Some("#89b4fa".into()),
            ..RawTheme::default()
        }
        .parse()
        .expect("parses");
        assert_eq!(theme.accent, rgb(0x89, 0xb4, 0xfa));
        assert_eq!(theme.dim, Dim::Auto);
        assert_eq!(theme.warning, Colour::Ansi(Ansi::Yellow));
    }

    #[test]
    fn dim_is_faint_a_colour_or_auto() {
        let dim = |raw: &str| {
            RawTheme {
                dim: Some(raw.into()),
                ..RawTheme::default()
            }
            .parse()
            .map(|t| t.dim)
        };
        assert_eq!(dim("faint"), Ok(Dim::Faint));
        assert_eq!(dim("#585b70"), Ok(Dim::Colour(rgb(0x58, 0x5b, 0x70))));
        assert_eq!(
            dim("bright-black"),
            Ok(Dim::Colour(Colour::Ansi(Ansi::BrightBlack)))
        );
        let err = dim("grey").expect_err("fails");
        assert_eq!(err, ThemeError::Dim("grey".into()));
        assert!(
            err.to_string()
                .starts_with("theme.dim: \"grey\" isn't faint or a colour")
        );
    }

    #[test]
    fn mixing_weighs_the_first_colour() {
        let fg = Rgb {
            r: 0xf8,
            g: 0xf9,
            b: 0xe8,
        };
        let bg = Rgb {
            r: 0x1e,
            g: 0x25,
            b: 0x28,
        };
        assert_eq!(fg.mix(bg, 1.0), fg);
        assert_eq!(fg.mix(bg, 0.0), bg);
        assert_eq!(
            fg.mix(bg, 0.5),
            Rgb {
                r: 0x8b,
                g: 0x8f,
                b: 0x88
            }
        );
    }
}
