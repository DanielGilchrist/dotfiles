mod input;
mod pty;
mod vt;

pub use input::{CellPos, encode_key, encode_mouse};
pub use pty::{Liveness, Terminal};
pub use vt::Vt100;

use std::num::NonZeroU16;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// A terminal size with no zero dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub rows: NonZeroU16,
    pub cols: NonZeroU16,
}

impl TermSize {
    pub fn of(area: Rect) -> Option<Self> {
        Some(Self { rows: NonZeroU16::new(area.height)?, cols: NonZeroU16::new(area.width)? })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseReporting {
    #[default]
    Off,
    /// The program asked for mouse events in SGR encoding.
    Sgr,
}

/// The terminal modes that change how input must be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modes {
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse: MouseReporting,
    pub cursor_visible: bool,
}

/// Parses program output into a screen. Implementations need not be `Send`:
/// a [`Terminal`] feeds its emulator on the thread that owns it.
pub trait Emulator {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, size: TermSize);
    fn modes(&self) -> Modes;
    fn render(&self, area: Rect, buf: &mut Buffer, show_cursor: bool);
}

impl<E: Emulator + ?Sized> Emulator for Box<E> {
    fn feed(&mut self, bytes: &[u8]) {
        (**self).feed(bytes);
    }

    fn resize(&mut self, size: TermSize) {
        (**self).resize(size);
    }

    fn modes(&self) -> Modes {
        (**self).modes()
    }

    fn render(&self, area: Rect, buf: &mut Buffer, show_cursor: bool) {
        (**self).render(area, buf, show_cursor);
    }
}

/// Builds an emulator for a new terminal; lets the TUI swap emulators.
pub type EmulatorFactory = fn(TermSize) -> Box<dyn Emulator>;

pub fn vt100(size: TermSize) -> Box<dyn Emulator> {
    Box::new(Vt100::new(size))
}
