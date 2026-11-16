use std::collections::BTreeMap;
use std::fmt;

use agent_core::config::Osc52;
use agent_host::Standing;

/// Whose terminal an allowed sequence is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    Everyone,
    Owner,
}

/// Which client's terminal a byte stream is for. A control client has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipient {
    Owner,
    Watcher,
}

impl Recipient {
    pub fn of(standing: Standing) -> Option<Self> {
        match standing {
            Standing::Owner => Some(Self::Owner),
            Standing::Watcher => Some(Self::Watcher),
            Standing::Control => None,
        }
    }
}

impl Audience {
    pub fn includes(self, recipient: Recipient) -> bool {
        match self {
            Self::Everyone => true,
            Self::Owner => recipient == Recipient::Owner,
        }
    }
}

/// What a host lets a program's output do, from what its starter passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub clipboard: Osc52,
}

/// A private mode a program may set or reset in the terminal of a client.
/// Anything else is dropped, whole sequence, so a mode the host doesn't know
/// is never turned on in a terminal it doesn't draw on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    ApplicationCursor,
    ReverseVideo,
    Origin,
    Autowrap,
    MouseX10,
    CursorBlink,
    CursorVisible,
    AlternateScreenLegacy,
    MouseNormal,
    MouseDrag,
    MouseAny,
    FocusReports,
    MouseUtf8,
    MouseSgr,
    MouseUrxvt,
    AlternateScreenClear,
    AlternateScreen,
    BracketedPaste,
    SynchronizedOutput,
}

impl Mode {
    pub const ALL: [Self; 19] = [
        Self::ApplicationCursor,
        Self::ReverseVideo,
        Self::Origin,
        Self::Autowrap,
        Self::MouseX10,
        Self::CursorBlink,
        Self::CursorVisible,
        Self::AlternateScreenLegacy,
        Self::MouseNormal,
        Self::MouseDrag,
        Self::MouseAny,
        Self::FocusReports,
        Self::MouseUtf8,
        Self::MouseSgr,
        Self::MouseUrxvt,
        Self::AlternateScreenClear,
        Self::AlternateScreen,
        Self::BracketedPaste,
        Self::SynchronizedOutput,
    ];

    pub const fn number(self) -> u16 {
        match self {
            Self::ApplicationCursor => 1,
            Self::ReverseVideo => 5,
            Self::Origin => 6,
            Self::Autowrap => 7,
            Self::MouseX10 => 9,
            Self::CursorBlink => 12,
            Self::CursorVisible => 25,
            Self::AlternateScreenLegacy => 47,
            Self::MouseNormal => 1000,
            Self::MouseDrag => 1002,
            Self::MouseAny => 1003,
            Self::FocusReports => 1004,
            Self::MouseUtf8 => 1005,
            Self::MouseSgr => 1006,
            Self::MouseUrxvt => 1015,
            Self::AlternateScreenClear => 1047,
            Self::AlternateScreen => 1049,
            Self::BracketedPaste => 2004,
            Self::SynchronizedOutput => 2026,
        }
    }

    pub fn from_number(number: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.number() == number)
    }

    /// Whether a terminal starts with this mode on.
    pub const fn starts_on(self) -> bool {
        matches!(self, Self::Autowrap | Self::CursorVisible)
    }
}

/// How a string sequence that ended its text, so a reply to a query ends the
/// same way the query did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    Bel,
    St,
}

impl Terminator {
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Bel => b"\x07",
            Self::St => b"\x1b\\",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    Foreground,
    Background,
}

/// The mode a status request asks about, which is not always one of the
/// private modes the host knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeQuery {
    Ansi(u16),
    Private(u16),
}

/// What a program asked its terminal. The host answers each one itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// `CSI c`, `CSI 0 c` and `ESC Z`.
    PrimaryAttributes,
    /// `CSI > c` and `CSI > 0 c`.
    SecondaryAttributes,
    /// `CSI 5 n`.
    DeviceStatus,
    /// `CSI 6 n`.
    CursorPosition,
    /// `CSI ? 6 n`.
    ExtendedCursorPosition,
    /// `CSI ? u`.
    KeyboardFlags,
    /// `CSI > q` and `CSI > 0 q`.
    Version,
    /// `CSI Pm $ p` and `CSI ? Pm $ p`.
    ModeStatus(ModeQuery),
    /// `OSC 10 ; ?` and `OSC 11 ; ?`.
    Colour {
        palette: Palette,
        terminator: Terminator,
    },
    /// `CSI 14 t`.
    PixelSize,
    /// `CSI 18 t`.
    CharSize,
}

/// Why a sequence was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refusal {
    Control,
    StrayC1,
    InvalidUtf8,
    UnlistedEscape,
    UnlistedCsi,
    UnlistedMode,
    MalformedCsi,
    Apc,
    Dcs,
    Pm,
    Sos,
    UnlistedOsc,
    MalformedOsc,
    HyperlinkTarget,
    HyperlinkParameters,
    ClipboardRead,
    ClipboardOff,
    /// A sequence or a string that was being kept was longer than it may be,
    /// and was read to its end and dropped.
    Overlong,
    /// Another sequence or a cancel began before it ended.
    Aborted,
}

impl Refusal {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Control => "control character",
            Self::StrayC1 => "8-bit control",
            Self::InvalidUtf8 => "invalid UTF-8",
            Self::UnlistedEscape => "unlisted escape",
            Self::UnlistedCsi => "unlisted CSI",
            Self::UnlistedMode => "unlisted mode",
            Self::MalformedCsi => "malformed CSI",
            Self::Apc => "APC",
            Self::Dcs => "DCS",
            Self::Pm => "PM",
            Self::Sos => "SOS",
            Self::UnlistedOsc => "unlisted OSC",
            Self::MalformedOsc => "malformed OSC",
            Self::HyperlinkTarget => "hyperlink target",
            Self::HyperlinkParameters => "hyperlink parameters",
            Self::ClipboardRead => "clipboard read",
            Self::ClipboardOff => "clipboard write",
            Self::Overlong => "overlong sequence",
            Self::Aborted => "aborted sequence",
        }
    }
}

/// What a sequence is once the parser has read it whole. Every class that is
/// has its verdict in [`Class::verdict`], an exhaustive match, so the
/// allowlist is this table and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Text,
    Control,
    Sgr,
    CursorMove,
    Erase,
    EditLines,
    Scroll,
    ScrollRegion,
    TabStops,
    CursorSave,
    Repeat,
    Index,
    CharacterSet,
    KeypadMode,
    Reset,
    Modes,
    KittyKeyboard,
    ModifyOtherKeys,
    CursorStyle,
    TitleStack,
    Title,
    Hyperlink,
    Clipboard,
    Answered(Query),
    Refused(Refusal),
}

/// What the host does with a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Forward(Audience),
    Answer(Query),
    Drop(Refusal),
}

impl Class {
    /// A write to the clipboard is the owner's only when the host was told it
    /// may be, and the title, which is the owner's terminal's window, is the
    /// owner's. Everything else changes what is drawn and so is for every
    /// client.
    pub fn verdict(self, policy: Policy) -> Verdict {
        match self {
            Self::Text
            | Self::Control
            | Self::Sgr
            | Self::CursorMove
            | Self::Erase
            | Self::EditLines
            | Self::Scroll
            | Self::ScrollRegion
            | Self::TabStops
            | Self::CursorSave
            | Self::Repeat
            | Self::Index
            | Self::CharacterSet
            | Self::KeypadMode
            | Self::Reset
            | Self::Modes
            | Self::KittyKeyboard
            | Self::ModifyOtherKeys
            | Self::CursorStyle
            | Self::Hyperlink => Verdict::Forward(Audience::Everyone),
            Self::TitleStack | Self::Title => Verdict::Forward(Audience::Owner),
            Self::Clipboard => match policy.clipboard {
                Osc52::Off => Verdict::Drop(Refusal::ClipboardOff),
                Osc52::Owner => Verdict::Forward(Audience::Owner),
            },
            Self::Answered(query) => Verdict::Answer(query),
            Self::Refused(refusal) => Verdict::Drop(refusal),
        }
    }
}

/// Dropped sequences by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally(BTreeMap<Refusal, u64>);

impl Tally {
    pub fn add(&mut self, refusal: Refusal) {
        let count = self.0.entry(refusal).or_insert(0);
        *count = count.saturating_add(1);
    }

    pub fn merge(&mut self, other: Self) {
        for (refusal, count) in other.0 {
            let total = self.0.entry(refusal).or_insert(0);
            *total = total.saturating_add(count);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn total(&self) -> u64 {
        self.0
            .values()
            .fold(0, |sum, count| sum.saturating_add(*count))
    }
}

impl fmt::Display for Tally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (at, (refusal, count)) in self.0.iter().enumerate() {
            if at > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{} x{count}", refusal.name())?;
        }
        Ok(())
    }
}
