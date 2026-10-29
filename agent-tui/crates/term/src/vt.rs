use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use tui_term::widget::{Cursor, PseudoTerminal};

use crate::{Emulator, Modes, MouseReporting, TermSize};

pub struct Vt100(vt100::Parser);

impl Vt100 {
    pub fn new(size: TermSize) -> Self {
        Self(vt100::Parser::new(size.rows.get(), size.cols.get(), 0))
    }
}

impl Emulator for Vt100 {
    fn feed(&mut self, bytes: &[u8]) {
        self.0.process(bytes);
    }

    fn resize(&mut self, size: TermSize) {
        self.0
            .screen_mut()
            .set_size(size.rows.get(), size.cols.get());
    }

    fn modes(&self) -> Modes {
        let screen = self.0.screen();
        let reporting = screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None;
        let sgr = screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr;
        Modes {
            application_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
            mouse: if reporting && sgr {
                MouseReporting::Sgr
            } else {
                MouseReporting::Off
            },
            cursor_visible: !screen.hide_cursor(),
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer, show_cursor: bool) {
        let mut cursor = Cursor::default();
        if !show_cursor {
            cursor.hide();
        }
        PseudoTerminal::new(self.0.screen())
            .cursor(cursor)
            .render(area, buf);
    }
}
