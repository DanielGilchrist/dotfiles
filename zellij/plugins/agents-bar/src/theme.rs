use zellij_tile::prelude::*;

// SGR helpers. Sections use these to compose styled cells without inlining
// raw escape codes.
pub const RESET: &str = "\u{1b}[0m";
pub const DIM: &str = "\u{1b}[2m";
pub const NDIM: &str = "\u{1b}[22m";
pub const BOLD: &str = "\u{1b}[1m";
pub const NBOLD: &str = "\u{1b}[22m";

/// SGR foreground escape for a palette color, or empty if None.
pub fn fg(c: Option<PaletteColor>) -> String {
    match c {
        Some(PaletteColor::Rgb((r, g, b))) => format!("\u{1b}[38;2;{};{};{}m", r, g, b),
        Some(PaletteColor::EightBit(n)) => format!("\u{1b}[38;5;{}m", n),
        None => String::new(),
    }
}

pub fn accented(c: Option<PaletteColor>, s: &str) -> String {
    format!("{}{}{}", fg(c), s, RESET)
}

pub fn dim(s: &str) -> String {
    format!("{}{}{}", DIM, s, NDIM)
}

pub fn bold(s: &str) -> String {
    format!("{}{}{}", BOLD, s, NBOLD)
}
