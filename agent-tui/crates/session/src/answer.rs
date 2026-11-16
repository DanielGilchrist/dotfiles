//! What the host says back when a program asks its terminal something. The
//! host is the program's terminal whether or not a client is attached, so
//! an app started detached isn't left waiting on a query nobody answers.
//!
//! The identity it claims is a VT220 that draws in colour: `CSI ? 62 ; 22 c`.
//! That is as much as `vt100`, the emulator behind the host, does. It
//! doesn't claim sixel (4), selective erase (6), rectangular editing (28),
//! 132 columns (1), a printer (2) or any graphics protocol, and `TERM` is the
//! `xterm-256color` the program was started with. A program that wants
//! more finds out from these replies that it isn't there.

use std::collections::BTreeSet;

use agent_core::size::TermSize;
use agent_host::Colours;

use crate::egress::{Mode, ModeQuery, Palette, Query};

const PRIMARY_ATTRIBUTES: &[u8] = b"\x1b[?62;22c";
/// VT220, firmware 10, no ROM cartridge.
const SECONDARY_ATTRIBUTES: &[u8] = b"\x1b[>1;10;0c";
const DEVICE_OK: &[u8] = b"\x1b[0n";

/// The host draws nothing, so it has no cell size. A program that asks gets
/// the size of a common terminal's cell, which is enough to size an image
/// the host can't show.
const CELL_WIDTH: u32 = 8;
const CELL_HEIGHT: u32 = 16;

/// A cursor position as a terminal reports it, counted from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportedCursor {
    pub row: u16,
    pub col: u16,
}

/// What the host knows that a reply is made of.
pub struct Facts<'a> {
    pub cursor: ReportedCursor,
    pub size: TermSize,
    pub keyboard_flags: u32,
    /// The private modes that differ from a terminal's defaults.
    pub modes: &'a BTreeSet<Mode>,
    /// The owner's, which a host with no owner or an owner that didn't say
    /// doesn't answer for.
    pub colours: Option<Colours>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeState {
    Unknown,
    Set,
    Reset,
}

impl ModeState {
    const fn code(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Set => 1,
            Self::Reset => 2,
        }
    }
}

fn mode_state(query: ModeQuery, modes: &BTreeSet<Mode>) -> ModeState {
    let ModeQuery::Private(number) = query else {
        return ModeState::Unknown;
    };
    let Some(mode) = Mode::from_number(number) else {
        return ModeState::Unknown;
    };
    if modes.contains(&mode) == mode.starts_on() {
        ModeState::Reset
    } else {
        ModeState::Set
    }
}

fn colour_component(value: u8) -> String {
    format!("{value:02x}{value:02x}")
}

pub fn reply(query: Query, facts: &Facts<'_>) -> Option<Vec<u8>> {
    let ReportedCursor { row, col } = facts.cursor;
    let (rows, cols) = (facts.size.rows.get(), facts.size.cols.get());
    Some(match query {
        Query::PrimaryAttributes => PRIMARY_ATTRIBUTES.to_vec(),
        Query::SecondaryAttributes => SECONDARY_ATTRIBUTES.to_vec(),
        Query::DeviceStatus => DEVICE_OK.to_vec(),
        Query::CursorPosition => format!("\x1b[{row};{col}R").into_bytes(),
        Query::ExtendedCursorPosition => format!("\x1b[?{row};{col};1R").into_bytes(),
        Query::KeyboardFlags => format!("\x1b[?{}u", facts.keyboard_flags).into_bytes(),
        Query::Version => {
            format!("\x1bP>|agent-host({})\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes()
        }
        Query::ModeStatus(query) => {
            let state = mode_state(query, facts.modes).code();
            let (marker, number) = match query {
                ModeQuery::Ansi(number) => ("", number),
                ModeQuery::Private(number) => ("?", number),
            };
            format!("\x1b[{marker}{number};{state}$y").into_bytes()
        }
        Query::Colour {
            palette,
            terminator,
        } => {
            let colours = facts.colours?;
            let (number, rgb) = match palette {
                Palette::Foreground => (10, colours.foreground),
                Palette::Background => (11, colours.background),
            };
            let mut reply = format!(
                "\x1b]{number};rgb:{}/{}/{}",
                colour_component(rgb.r),
                colour_component(rgb.g),
                colour_component(rgb.b)
            )
            .into_bytes();
            reply.extend_from_slice(terminator.bytes());
            reply
        }
        Query::PixelSize => format!(
            "\x1b[4;{};{}t",
            u32::from(rows) * CELL_HEIGHT,
            u32::from(cols) * CELL_WIDTH
        )
        .into_bytes(),
        Query::CharSize => format!("\x1b[8;{rows};{cols}t").into_bytes(),
    })
}
