//! What a whole escape sequence is, by what it says and nothing else. The
//! parser decides where a sequence ends, this decides which [`Class`] it is,
//! and [`Class::verdict`] decides what happens to it.

use super::table::{Class, Mode, ModeQuery, Palette, Query, Refusal, Terminator};

/// The longest a CSI or an escape's intermediates may be, in bytes. One that
/// gets longer is read to its final byte and dropped.
pub(super) const MAX_CSI: usize = 256;

/// What the parser makes of a CSI it has read whole.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reading {
    Whole(Class),
    /// A mode list with some modes the host doesn't know: the sequence with
    /// only the listed ones, which is what is forwarded.
    Trimmed(Vec<u8>),
}

const TITLE_LIMIT: usize = 2048;
const HYPERLINK_LIMIT: usize = 4096;
const COLOUR_LIMIT: usize = 32;
const CLIPBOARD_LIMIT: usize = 256 * 1024;
const MAX_URI: usize = 2048;
const SELECTIONS: &[u8] = b"cpqs01234567";
const SCHEMES: [&str; 3] = ["http", "https", "mailto"];
const DECSCUSR_STYLES: u32 = 6;

fn refused(refusal: Refusal) -> Class {
    Class::Refused(refusal)
}

struct Shape {
    final_byte: u8,
    class: Class,
    max_params: usize,
}

const fn shape(final_byte: u8, class: Class, max_params: usize) -> Shape {
    Shape {
        final_byte,
        class,
        max_params,
    }
}

/// The CSI sequences with no marker and no intermediate that are forwarded,
/// by final byte and how many parameters each takes. `CSI Pl ; Pr s` and
/// `CSI Ps ; ... T` with more are other sequences in other terminals, so they
/// are not in these shapes.
const PLAIN: [Shape; 30] = [
    shape(b'@', Class::EditLines, 1),
    shape(b'A', Class::CursorMove, 1),
    shape(b'B', Class::CursorMove, 1),
    shape(b'C', Class::CursorMove, 1),
    shape(b'D', Class::CursorMove, 1),
    shape(b'E', Class::CursorMove, 1),
    shape(b'F', Class::CursorMove, 1),
    shape(b'G', Class::CursorMove, 1),
    shape(b'H', Class::CursorMove, 2),
    shape(b'I', Class::TabStops, 1),
    shape(b'J', Class::Erase, 1),
    shape(b'K', Class::Erase, 1),
    shape(b'L', Class::EditLines, 1),
    shape(b'M', Class::EditLines, 1),
    shape(b'P', Class::EditLines, 1),
    shape(b'S', Class::Scroll, 1),
    shape(b'T', Class::Scroll, 1),
    shape(b'X', Class::Erase, 1),
    shape(b'Z', Class::TabStops, 1),
    shape(b'`', Class::CursorMove, 1),
    shape(b'a', Class::CursorMove, 1),
    shape(b'b', Class::Repeat, 1),
    shape(b'd', Class::CursorMove, 1),
    shape(b'e', Class::CursorMove, 1),
    shape(b'f', Class::CursorMove, 2),
    shape(b'g', Class::TabStops, 1),
    shape(b'm', Class::Sgr, usize::MAX),
    shape(b'r', Class::ScrollRegion, 2),
    shape(b's', Class::CursorSave, 0),
    shape(b'u', Class::CursorSave, 0),
];

struct Csi<'a> {
    marker: Option<u8>,
    params: &'a [u8],
    intermediates: &'a [u8],
    final_byte: u8,
}

impl<'a> Csi<'a> {
    /// `ESC [`, what is between and the final byte. `None` when a parameter
    /// holds anything but digits, `;` and `:`, or a marker isn't first.
    fn split(held: &'a [u8]) -> Option<Self> {
        let (final_byte, body) = held.get(2..)?.split_last()?;
        let (marker, rest) = match body.split_first() {
            Some((marker @ (b'<' | b'=' | b'>' | b'?'), rest)) => (Some(*marker), rest),
            _ => (None, body),
        };
        let at = rest
            .iter()
            .position(|byte| (0x20..=0x2f).contains(byte))
            .unwrap_or(rest.len());
        let (params, intermediates) = rest.split_at(at);
        let plain = |byte: &u8| byte.is_ascii_digit() || *byte == b';' || *byte == b':';
        if !params.iter().all(plain) {
            return None;
        }
        Some(Self {
            marker,
            params,
            intermediates,
            final_byte: *final_byte,
        })
    }

    /// The parameters as numbers, each `None` when left out. `None` when one
    /// has sub-parameters, which only SGR uses.
    fn numbers(&self) -> Option<Vec<Option<u32>>> {
        if self.params.is_empty() {
            return Some(Vec::new());
        }
        self.params
            .split(|byte| *byte == b';')
            .map(|digits| {
                if digits.is_empty() {
                    return Some(None);
                }
                if digits.contains(&b':') {
                    return None;
                }
                let value = digits.iter().fold(0u32, |value, digit| {
                    value
                        .saturating_mul(10)
                        .saturating_add(u32::from(digit - b'0'))
                });
                Some(Some(value))
            })
            .collect()
    }
}

/// An escape: `ESC`, intermediates, a final byte.
pub(super) fn escape(held: &[u8]) -> Class {
    let Some((final_byte, between)) = held.get(1..).and_then(<[u8]>::split_last) else {
        return refused(Refusal::UnlistedEscape);
    };
    match (between, *final_byte) {
        ([], b'7' | b'8') => Class::CursorSave,
        ([], b'=' | b'>') => Class::KeypadMode,
        ([], b'D' | b'E' | b'M') => Class::Index,
        ([], b'H') => Class::TabStops,
        ([], b'c') => Class::Reset,
        ([], b'Z') => Class::Answered(Query::PrimaryAttributes),
        ([b'(' | b')'], b'0' | b'A' | b'B') => Class::CharacterSet,
        _ => refused(Refusal::UnlistedEscape),
    }
}

pub(super) fn csi(held: &[u8]) -> Reading {
    let Some(csi) = Csi::split(held) else {
        return Reading::Whole(refused(Refusal::MalformedCsi));
    };
    let Some(numbers) = csi.numbers() else {
        return Reading::Whole(
            if csi.marker.is_none() && csi.intermediates.is_empty() && csi.final_byte == b'm' {
                Class::Sgr
            } else {
                refused(Refusal::MalformedCsi)
            },
        );
    };
    if csi.marker == Some(b'?')
        && csi.intermediates.is_empty()
        && matches!(csi.final_byte, b'h' | b'l')
    {
        return modes(csi.final_byte, &numbers);
    }
    Reading::Whole(other_csi(&csi, &numbers))
}

fn other_csi(csi: &Csi<'_>, numbers: &[Option<u32>]) -> Class {
    match (csi.marker, csi.intermediates) {
        (None, []) => plain(csi.final_byte, numbers),
        (Some(b'?'), []) => private(csi.final_byte, numbers),
        (Some(b'>'), []) => secondary(csi.final_byte, numbers),
        (Some(b'<'), []) => match (csi.final_byte, numbers.len()) {
            (b'u', 0 | 1) => Class::KittyKeyboard,
            _ => refused(Refusal::UnlistedCsi),
        },
        (Some(b'='), []) => match (csi.final_byte, numbers.len()) {
            (b'u', 1 | 2) => Class::KittyKeyboard,
            _ => refused(Refusal::UnlistedCsi),
        },
        (None, [b' ']) => cursor_style(csi.final_byte, numbers),
        (None, [b'!']) => match (csi.final_byte, numbers.len()) {
            (b'p', 0) => Class::Reset,
            _ => refused(Refusal::UnlistedCsi),
        },
        (marker @ (None | Some(b'?')), [b'$']) => mode_status(csi.final_byte, marker, numbers),
        _ => refused(Refusal::UnlistedCsi),
    }
}

fn plain(final_byte: u8, numbers: &[Option<u32>]) -> Class {
    match final_byte {
        b'c' => {
            return match numbers {
                [] | [None | Some(0)] => Class::Answered(Query::PrimaryAttributes),
                _ => refused(Refusal::UnlistedCsi),
            };
        }
        b'n' => {
            return match numbers {
                [Some(5)] => Class::Answered(Query::DeviceStatus),
                [Some(6)] => Class::Answered(Query::CursorPosition),
                _ => refused(Refusal::UnlistedCsi),
            };
        }
        b't' => return window_operation(numbers),
        b'h' | b'l' => return refused(Refusal::UnlistedMode),
        _ => {}
    }
    match PLAIN.iter().find(|shape| shape.final_byte == final_byte) {
        Some(shape) if numbers.len() <= shape.max_params => shape.class,
        _ => refused(Refusal::UnlistedCsi),
    }
}

/// `CSI Ps t`: the title stack and the two size reports. Resizing, moving,
/// minimising and reporting the title are other `Ps` and are dropped.
fn window_operation(numbers: &[Option<u32>]) -> Class {
    match numbers {
        [Some(22 | 23), ..] => Class::TitleStack,
        [Some(14)] => Class::Answered(Query::PixelSize),
        [Some(18)] => Class::Answered(Query::CharSize),
        _ => refused(Refusal::UnlistedCsi),
    }
}

fn private(final_byte: u8, numbers: &[Option<u32>]) -> Class {
    match final_byte {
        b'u' if numbers.is_empty() => Class::Answered(Query::KeyboardFlags),
        b'n' if numbers == [Some(6)] => Class::Answered(Query::ExtendedCursorPosition),
        _ => refused(Refusal::UnlistedCsi),
    }
}

/// `CSI ? Pm h` and `CSI ? Pm l`. The modes the host knows are forwarded and
/// the others are left out of the sequence, since the screen and a client's
/// terminal are both fed what is forwarded and so both apply the same list.
fn modes(final_byte: u8, numbers: &[Option<u32>]) -> Reading {
    let listed: Vec<Mode> = numbers
        .iter()
        .filter_map(|number| {
            number
                .and_then(|number| u16::try_from(number).ok())
                .and_then(Mode::from_number)
        })
        .collect();
    if listed.is_empty() {
        return Reading::Whole(refused(Refusal::UnlistedMode));
    }
    if listed.len() == numbers.len() {
        return Reading::Whole(Class::Modes);
    }
    let list: Vec<String> = listed
        .iter()
        .map(|mode| mode.number().to_string())
        .collect();
    let mut kept = format!("\x1b[?{}", list.join(";")).into_bytes();
    kept.push(final_byte);
    Reading::Trimmed(kept)
}

fn secondary(final_byte: u8, numbers: &[Option<u32>]) -> Class {
    match (final_byte, numbers) {
        (b'c', [] | [None | Some(0)]) => Class::Answered(Query::SecondaryAttributes),
        (b'q', [] | [None | Some(0)]) => Class::Answered(Query::Version),
        (b'u', [] | [_]) => Class::KittyKeyboard,
        (b'm', [Some(4)] | [Some(4), None | Some(0..=2)]) | (b'n', [Some(4)]) => {
            Class::ModifyOtherKeys
        }
        _ => refused(Refusal::UnlistedCsi),
    }
}

fn cursor_style(final_byte: u8, numbers: &[Option<u32>]) -> Class {
    match (final_byte, numbers) {
        (b'q', [] | [None]) => Class::CursorStyle,
        (b'q', [Some(style)]) if *style <= DECSCUSR_STYLES => Class::CursorStyle,
        _ => refused(Refusal::UnlistedCsi),
    }
}

fn mode_status(final_byte: u8, marker: Option<u8>, numbers: &[Option<u32>]) -> Class {
    match (final_byte, numbers) {
        (b'p', [Some(mode)]) => match u16::try_from(*mode) {
            Ok(mode) => Class::Answered(Query::ModeStatus(if marker.is_some() {
                ModeQuery::Private(mode)
            } else {
                ModeQuery::Ansi(mode)
            })),
            Err(_) => refused(Refusal::UnlistedCsi),
        },
        _ => refused(Refusal::UnlistedCsi),
    }
}

/// What the parser does with an OSC once its number is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Route {
    /// Keep the bytes, up to this many in all, to classify at the end.
    Collect { limit: usize },
    /// Read to the end and drop, keeping nothing.
    Skip(Refusal),
}

/// How many bytes an OSC with this number may be, or `None` when its number
/// isn't one of the listed ones.
fn osc_limit(number: u32) -> Option<usize> {
    match number {
        0..=2 => Some(TITLE_LIMIT),
        8 => Some(HYPERLINK_LIMIT),
        10 | 11 => Some(COLOUR_LIMIT),
        52 => Some(CLIPBOARD_LIMIT),
        _ => None,
    }
}

pub(super) fn route(number: &[u8]) -> Route {
    let limit = std::str::from_utf8(number)
        .ok()
        .filter(|digits| !digits.is_empty() && digits.len() <= 5)
        .and_then(|digits| digits.parse::<u32>().ok())
        .and_then(osc_limit);
    match limit {
        Some(limit) => Route::Collect { limit },
        None => Route::Skip(Refusal::UnlistedOsc),
    }
}

/// An OSC read whole, `ESC ]` first and its terminator last.
pub(super) fn osc(held: &[u8], terminator: Terminator) -> Class {
    let end = held.len().saturating_sub(terminator.bytes().len());
    let Some(text) = held
        .get(2..end)
        .and_then(|body| std::str::from_utf8(body).ok())
        .filter(|text| !text.chars().any(char::is_control))
    else {
        return refused(Refusal::MalformedOsc);
    };
    let Some((number, rest)) = text.split_once(';') else {
        return match text.parse::<u32>().ok().and_then(osc_limit) {
            Some(_) => refused(Refusal::MalformedOsc),
            None => refused(Refusal::UnlistedOsc),
        };
    };
    match number.parse::<u16>() {
        Ok(0..=2) => Class::Title,
        Ok(8) => hyperlink(rest),
        Ok(52) => clipboard(rest),
        Ok(10) if rest == "?" => Class::Answered(Query::Colour {
            palette: Palette::Foreground,
            terminator,
        }),
        Ok(11) if rest == "?" => Class::Answered(Query::Colour {
            palette: Palette::Background,
            terminator,
        }),
        _ => refused(Refusal::UnlistedOsc),
    }
}

/// `params ; URI`. Parameters, which is where `id=` lives, are refused, so a
/// link never joins another's cells or is highlighted with them. An empty URI
/// ends a link and is always passed.
fn hyperlink(rest: &str) -> Class {
    let Some((params, uri)) = rest.split_once(';') else {
        return refused(Refusal::MalformedOsc);
    };
    if !params.is_empty() {
        return refused(Refusal::HyperlinkParameters);
    }
    if hyperlink_target(uri) {
        Class::Hyperlink
    } else {
        refused(Refusal::HyperlinkTarget)
    }
}

fn hyperlink_target(uri: &str) -> bool {
    if uri.is_empty() {
        return true;
    }
    if uri.len() > MAX_URI || !uri.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return false;
    }
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    if !SCHEMES.contains(&scheme.as_str()) {
        return false;
    }
    match scheme.as_str() {
        "mailto" => !rest.is_empty(),
        _ => rest
            .strip_prefix("//")
            .is_some_and(|authority| !authority.is_empty()),
    }
}

/// `Pc ; Pd`. Only a write of base64 data is a write: `?` asks the terminal
/// for its clipboard, which would come back as input the program didn't get
/// from the user.
fn clipboard(rest: &str) -> Class {
    let Some((selection, data)) = rest.split_once(';') else {
        return refused(Refusal::MalformedOsc);
    };
    if !selection.bytes().all(|byte| SELECTIONS.contains(&byte)) {
        return refused(Refusal::MalformedOsc);
    }
    if data == "?" {
        return refused(Refusal::ClipboardRead);
    }
    let base64 =
        |byte: u8| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'=';
    if data.is_empty() || !data.bytes().all(base64) {
        return refused(Refusal::MalformedOsc);
    }
    Class::Clipboard
}
