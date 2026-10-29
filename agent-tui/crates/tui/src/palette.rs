//! The configured theme resolved against what the terminal reports.

use std::io::{IsTerminal, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use agent_core::theme::{Ansi, Colour, Dim, Rgb, Theme};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use ratatui::style::{Color, Modifier, Style};

/// How long to wait for the terminal's colours. A terminal answers the
/// device attributes query sent after them, so one that ignores the colour
/// queries still ends the wait early, and locally that takes milliseconds.
/// The wait is long so a slow link's replies aren't read later as keys.
const PROBE_WAIT: Duration = Duration::from_secs(1);
/// Dim text gets the least of the foreground that reaches this contrast
/// with the background, starting from half. Halfway is plenty on most dark
/// themes but barely readable on light ones and Solarized.
const DIM_CONTRAST: f32 = 3.0;
const REPLY_MAX: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub accent: Color,
    pub dim: Secondary,
    pub warning: Color,
    pub error: Color,
    pub success: Color,
}

/// How secondary text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secondary {
    Faint,
    Colour(Color),
}

impl Secondary {
    pub fn style(self) -> Style {
        match self {
            Self::Faint => Style::new().add_modifier(Modifier::DIM),
            Self::Colour(colour) => Style::new().fg(colour),
        }
    }
}

impl Palette {
    pub const DEFAULT: Self = Self {
        accent: Color::Cyan,
        dim: Secondary::Faint,
        warning: Color::Yellow,
        error: Color::Red,
        success: Color::Green,
    };

    const MONOCHROME: Self = Self {
        accent: Color::Reset,
        dim: Secondary::Faint,
        warning: Color::Reset,
        error: Color::Reset,
        success: Color::Reset,
    };

    pub fn resolve(theme: &Theme, backdrop: Backdrop, colours: Colours) -> Self {
        let dim = match (colours, theme.dim, backdrop) {
            (Colours::Off, ..) => return Self::MONOCHROME,
            (_, Dim::Colour(colour), _) => Secondary::Colour(color(colour)),
            (Colours::TrueColour, Dim::Auto, Backdrop::Reported { fg, bg }) => {
                Secondary::Colour(color(Colour::Rgb(dim_over(fg, bg))))
            }
            (Colours::Ansi, Dim::Auto, _)
            | (_, Dim::Auto, Backdrop::Unknown)
            | (_, Dim::Faint, _) => Secondary::Faint,
        };
        Self {
            accent: color(theme.accent),
            dim,
            warning: color(theme.warning),
            error: color(theme.error),
            success: color(theme.success),
        }
    }
}

/// What colours the terminal takes: `NO_COLOR` turns them off, and only
/// `COLORTERM` says 24-bit colour won't be rounded to the nearest of 256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colours {
    Off,
    Ansi,
    TrueColour,
}

impl Colours {
    pub fn from_env() -> Self {
        let set = |name| std::env::var_os(name).filter(|value| !value.is_empty());
        match (set("NO_COLOR"), set("COLORTERM")) {
            (Some(_), _) => Self::Off,
            (None, Some(depth)) if depth == "truecolor" || depth == "24bit" => Self::TrueColour,
            (None, _) => Self::Ansi,
        }
    }
}

/// The terminal's default foreground and background, from `OSC 10` and
/// `OSC 11`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    Reported { fg: Rgb, bg: Rgb },
    Unknown,
}

fn dim_over(fg: Rgb, bg: Rgb) -> Rgb {
    (0u8..=10)
        .map(|step| fg.mix(bg, 0.5 + f32::from(step) * 0.05))
        .find(|dim| contrast(*dim, bg) >= DIM_CONTRAST)
        .unwrap_or(fg)
}

/// WCAG 2's contrast ratio.
fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn luminance(Rgb { r, g, b }: Rgb) -> f32 {
    let linear = |byte: u8| {
        let c = f32::from(byte) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn color(colour: Colour) -> Color {
    match colour {
        Colour::Default => Color::Reset,
        Colour::Rgb(Rgb { r, g, b }) => Color::Rgb(r, g, b),
        Colour::Ansi(ansi) => match ansi {
            Ansi::Black => Color::Black,
            Ansi::Red => Color::Red,
            Ansi::Green => Color::Green,
            Ansi::Yellow => Color::Yellow,
            Ansi::Blue => Color::Blue,
            Ansi::Magenta => Color::Magenta,
            Ansi::Cyan => Color::Cyan,
            Ansi::White => Color::Gray,
            Ansi::BrightBlack => Color::DarkGray,
            Ansi::BrightRed => Color::LightRed,
            Ansi::BrightGreen => Color::LightGreen,
            Ansi::BrightYellow => Color::LightYellow,
            Ansi::BrightBlue => Color::LightBlue,
            Ansi::BrightMagenta => Color::LightMagenta,
            Ansi::BrightCyan => Color::LightCyan,
            Ansi::BrightWhite => Color::White,
        },
    }
}

/// What the terminal says its colours are, asked only when the theme
/// would use them. Runs before the screen is set up, so no mouse or focus
/// reports mix with the replies, and before anything else reads the tty,
/// or the replies would arrive as keys.
pub fn backdrop(theme: &Theme, colours: Colours) -> Backdrop {
    match (theme.dim, colours) {
        (Dim::Auto, Colours::TrueColour) => {
            if crossterm::terminal::enable_raw_mode().is_err() {
                return Backdrop::Unknown;
            }
            let backdrop = probe();
            let _ = crossterm::terminal::disable_raw_mode();
            backdrop
        }
        _ => Backdrop::Unknown,
    }
}

fn probe() -> Backdrop {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() || !std::io::stdout().is_terminal() {
        return Backdrop::Unknown;
    }
    let mut out = std::io::stdout();
    if out
        .write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c")
        .and_then(|()| out.flush())
        .is_err()
    {
        return Backdrop::Unknown;
    }
    let deadline = Instant::now() + PROBE_WAIT;
    let mut reply = Vec::new();
    let mut byte = [0u8; 1];
    while reply.len() < REPLY_MAX && !ends_with_attributes(&reply) {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(wait) = PollTimeout::try_from(left) else {
            break;
        };
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, wait) {
            Ok(n) if n > 0 => {}
            _ => break,
        }
        // One byte at a time, so nothing typed after the replies is lost.
        match nix::unistd::read(stdin.as_fd(), &mut byte) {
            Ok(1) => reply.push(byte[0]),
            _ => break,
        }
    }
    parse_backdrop(&reply)
}

/// Whether `reply` ends with the answer to `CSI c`: `CSI ? … c`.
fn ends_with_attributes(reply: &[u8]) -> bool {
    let Some(start) = reply.windows(3).rposition(|w| w == b"\x1b[?") else {
        return false;
    };
    match reply[start + 3..].split_last() {
        Some((b'c', params)) => params.iter().all(|b| b.is_ascii_digit() || *b == b';'),
        _ => false,
    }
}

fn parse_backdrop(reply: &[u8]) -> Backdrop {
    match (answer(reply, b"10"), answer(reply, b"11")) {
        (Some(fg), Some(bg)) => Backdrop::Reported { fg, bg },
        _ => Backdrop::Unknown,
    }
}

/// The colour in `OSC <code> ; rgb:R/G/B`, ended by BEL or ST.
fn answer(reply: &[u8], code: &[u8]) -> Option<Rgb> {
    let prefix = [b"\x1b]".as_slice(), code, b";rgb:"].concat();
    let start = reply.windows(prefix.len()).position(|w| w == prefix)? + prefix.len();
    let rest = &reply[start..];
    let end = rest.iter().position(|b| matches!(b, b'\x07' | b'\x1b'))?;
    let spec = std::str::from_utf8(&rest[..end]).ok()?;
    let mut channels = spec.split('/').map(channel);
    match (
        channels.next(),
        channels.next(),
        channels.next(),
        channels.next(),
    ) {
        (Some(Some(r)), Some(Some(g)), Some(Some(b)), None) => Some(Rgb { r, g, b }),
        _ => None,
    }
}

/// One to four hex digits, scaled to a byte (`ffff` and `ff` are both 255).
fn channel(hex: &str) -> Option<u8> {
    if !(1..=4).contains(&hex.len()) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let max = (1u32 << (4 * hex.len())) - 1;
    u8::try_from((value * 255 + max / 2) / max).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: Rgb = Rgb {
        r: 0xf8,
        g: 0xf9,
        b: 0xe8,
    };
    const BG: Rgb = Rgb {
        r: 0x1e,
        g: 0x25,
        b: 0x28,
    };

    #[test]
    fn replies_terminated_by_st_or_bel_are_read() {
        let st = b"\x1b]10;rgb:f8f8/f9f9/e8e8\x1b\\\x1b]11;rgb:1e1e/2525/2828\x1b\\\x1b[?62;22c";
        assert_eq!(parse_backdrop(st), Backdrop::Reported { fg: FG, bg: BG });
        let bel = b"\x1b]11;rgb:1e/25/28\x07\x1b]10;rgb:f8/f9/e8\x07\x1b[?1;2c";
        assert_eq!(parse_backdrop(bel), Backdrop::Reported { fg: FG, bg: BG });
    }

    #[test]
    fn a_terminal_that_ignores_the_colour_queries_is_unknown() {
        assert_eq!(parse_backdrop(b"\x1b[?1;2c"), Backdrop::Unknown);
        assert_eq!(
            parse_backdrop(b"\x1b]11;rgb:1e1e/2525/2828\x1b\\\x1b[?1;2c"),
            Backdrop::Unknown
        );
        assert_eq!(
            parse_backdrop(b"\x1b]10;rgb:zz/00/00\x07\x1b]11;rgb:00/00/00\x07"),
            Backdrop::Unknown
        );
        assert_eq!(
            parse_backdrop(b"\x1b]10;rgb:ff/ff/ff/ff\x07\x1b]11;rgb:00/00/00\x07"),
            Backdrop::Unknown
        );
    }

    #[test]
    fn channels_scale_from_any_width() {
        assert_eq!(channel("f"), Some(255));
        assert_eq!(channel("8"), Some(136));
        assert_eq!(channel("80"), Some(128));
        assert_eq!(channel("ffff"), Some(255));
        assert_eq!(channel("0000"), Some(0));
        assert_eq!(channel("1e1e"), Some(30));
        assert_eq!(channel(""), None);
        assert_eq!(channel("fffff"), None);
        assert_eq!(channel("+f"), None);
    }

    #[test]
    fn the_wait_ends_on_the_attributes_reply() {
        assert!(!ends_with_attributes(b""));
        assert!(!ends_with_attributes(b"\x1b]10;rgb:f8/f9/e8\x07"));
        assert!(!ends_with_attributes(b"\x1b[?62;2"));
        assert!(ends_with_attributes(
            b"\x1b]10;rgb:f8/f9/e8\x07\x1b[?62;22c"
        ));
        assert!(ends_with_attributes(b"\x1b[?6c"));
    }

    #[test]
    fn dim_blends_the_reported_colours_unless_configured() {
        let reported = Backdrop::Reported { fg: FG, bg: BG };
        let auto = Palette::resolve(&Theme::default(), reported, Colours::TrueColour);
        assert_eq!(auto.dim, Secondary::Colour(Color::Rgb(0x8b, 0x8f, 0x88)));
        assert_eq!(auto.accent, Color::Cyan);

        let unknown = Palette::resolve(&Theme::default(), Backdrop::Unknown, Colours::TrueColour);
        assert_eq!(unknown, Palette::DEFAULT);

        let faint = Theme {
            dim: Dim::Faint,
            ..Theme::default()
        };
        assert_eq!(
            Palette::resolve(&faint, reported, Colours::TrueColour).dim,
            Palette::DEFAULT.dim
        );

        let fixed = Theme {
            dim: Dim::Colour(Colour::Ansi(Ansi::BrightBlack)),
            ..Theme::default()
        };
        assert_eq!(
            Palette::resolve(&fixed, reported, Colours::TrueColour).dim,
            Secondary::Colour(Color::DarkGray)
        );
    }

    #[test]
    fn dim_stays_readable_on_light_and_low_contrast_themes() {
        let hex = |hex: &str| match hex.parse() {
            Ok(Colour::Rgb(rgb)) => rgb,
            other => panic!("{hex} parsed as {other:?}"),
        };
        for (theme, fg, bg) in [
            ("owner", "#f8f9e8", "#1e2528"),
            ("mocha", "#cdd6f4", "#1e1e2e"),
            ("latte", "#4c4f69", "#eff1f5"),
            ("solarized dark", "#839496", "#002b36"),
            ("solarized light", "#657b83", "#fdf6e3"),
            ("gruvbox light", "#3c3836", "#fbf1c7"),
            ("black on white", "#000000", "#ffffff"),
        ] {
            let (fg, bg) = (hex(fg), hex(bg));
            let dim = dim_over(fg, bg);
            assert!(
                contrast(dim, bg) >= DIM_CONTRAST || dim == fg,
                "{theme}: {:.2}",
                contrast(dim, bg)
            );
            assert!(contrast(fg, dim) > 1.0, "{theme}: dim is the foreground");
        }
        let (fg, bg) = (hex("#f8f9e8"), hex("#1e2528"));
        assert_eq!(
            dim_over(fg, bg),
            fg.mix(bg, 0.5),
            "dark themes keep the halfway mix"
        );
    }

    #[test]
    fn without_truecolour_dim_stays_faint() {
        let reported = Backdrop::Reported { fg: FG, bg: BG };
        let palette = Palette::resolve(&Theme::default(), reported, Colours::Ansi);
        assert_eq!(palette.dim, Palette::DEFAULT.dim);
        let fixed = Theme {
            dim: Dim::Colour(Colour::Rgb(BG)),
            ..Theme::default()
        };
        assert_eq!(
            Palette::resolve(&fixed, reported, Colours::Ansi).dim,
            Secondary::Colour(Color::Rgb(0x1e, 0x25, 0x28)),
            "a configured colour is the user's call"
        );
    }

    #[test]
    fn no_color_drops_every_colour() {
        let theme = Theme {
            accent: Colour::Rgb(FG),
            ..Theme::default()
        };
        let palette = Palette::resolve(&theme, Backdrop::Reported { fg: FG, bg: BG }, Colours::Off);
        assert_eq!(palette, Palette::MONOCHROME);
    }
}
