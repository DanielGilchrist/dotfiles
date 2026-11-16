use std::num::{NonZeroU16, NonZeroU64};

use agent_core::size::TermSize;
use agent_host::{Colours, Scrollback, Style};

use crate::answer::{Facts, ReportedCursor, reply};
use crate::egress::{Asked, Mode};
use crate::ledger::{CursorSave, Event, Ledger, Switch};

/// What `vt100` spends on a cell, which turns a scrollback cap in bytes into
/// rows of a given width.
const CELL_BYTES: u64 = 32;

/// Modes `vt100` serialises itself, or that need more than being set again:
/// the alternate screen and synchronized output are handled by the replay,
/// and origin mode would move the cursor the replay has already placed.
const SERIALISED_BY_THE_SCREEN: [Mode; 14] = [
    Mode::ApplicationCursor,
    Mode::Origin,
    Mode::MouseX10,
    Mode::CursorVisible,
    Mode::AlternateScreenLegacy,
    Mode::MouseNormal,
    Mode::MouseDrag,
    Mode::MouseAny,
    Mode::MouseUtf8,
    Mode::MouseSgr,
    Mode::MouseUrxvt,
    Mode::AlternateScreen,
    Mode::BracketedPaste,
    Mode::SynchronizedOutput,
];

/// How much scrollback a host keeps, in bytes of cells, so the cost doesn't
/// depend on the width of the owner's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackCap(NonZeroU64);

impl ScrollbackCap {
    pub const DEFAULT: Self = Self(NonZeroU64::MIN.saturating_add(32 * 1024 * 1024 - 1));

    pub fn bytes(bytes: NonZeroU64) -> Self {
        Self(bytes)
    }

    /// The rows that fit at `cols` columns. The width is the one the screen
    /// had when it was made, so a terminal that later widens costs more.
    fn rows(self, cols: NonZeroU16) -> usize {
        let rows = self.0.get() / (u64::from(cols.get()) * CELL_BYTES);
        usize::try_from(rows).unwrap_or(usize::MAX)
    }
}

/// The host's own terminal screen, fed everything the program prints. A
/// different emulator replaces [`Vt100Screen`] behind it, so everything
/// the host needs from an emulator is here: what to send a client that
/// attaches, and what a snapshot prints.
pub(crate) trait Screen {
    /// Takes what the program printed once the egress parser had read it,
    /// which is what the clients are shown, and returns what a terminal would
    /// answer to the queries that were in it, to be typed back into the
    /// program. `asked` says how many bytes of `cleaned` came before each
    /// query, in order, so a reply reads the screen as it was at that point
    /// and not at the end of the chunk. `colours` are the owner's, for the
    /// queries about them.
    fn feed(&mut self, cleaned: &[u8], asked: &[Asked], colours: Option<Colours>) -> Vec<u8>;
    fn resize(&mut self, size: TermSize);
    /// Whether the program is between the start and end of a synchronized
    /// update, so a replay now would show half a frame.
    fn synchronized(&self) -> bool;
    /// Bytes that rebuild this screen on a terminal that has never seen it.
    fn replay(&self) -> Vec<u8>;
    /// Styled text rows for people: the screen, or its scrollback too.
    fn dump(&mut self, scrollback: Scrollback, style: Style) -> Vec<u8>;
}

type Position = (u16, u16);

#[derive(Debug, Default, Clone, Copy)]
struct SavedCursors {
    csi: Option<Position>,
    dec: Option<Position>,
}

impl SavedCursors {
    fn set(&mut self, kind: CursorSave, at: Position) {
        match kind {
            CursorSave::Csi => self.csi = Some(at),
            CursorSave::Dec => self.dec = Some(at),
        }
    }
}

/// `vt100` plus a [`Ledger`] for what `vt100` doesn't serialise, and the
/// main screen as it was when the program switched to the alternate one,
/// since `vt100` can't give the screen it isn't showing.
pub(crate) struct Vt100Screen {
    parser: vt100::Parser,
    ledger: Ledger,
    saved: SavedCursors,
    main: Option<Vec<u8>>,
    regions: [Option<Region>; 2],
    size: TermSize,
}

/// A scroll region as `DECSTBM` states it: rows from 1, both included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    top: u16,
    bottom: u16,
}

/// The screen `vt100` is showing, which it alone knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grid {
    Main,
    Alternate,
}

impl Grid {
    fn index(self) -> usize {
        match self {
            Self::Main => 0,
            Self::Alternate => 1,
        }
    }
}

fn cursor_to((row, col): Position) -> Vec<u8> {
    format!("\x1b[{};{}H", row + 1, col + 1).into_bytes()
}

impl Vt100Screen {
    pub fn new(size: TermSize, scrollback: ScrollbackCap) -> Self {
        Self {
            parser: vt100::Parser::new(
                size.rows.get(),
                size.cols.get(),
                scrollback.rows(size.cols),
            ),
            ledger: Ledger::new(),
            saved: SavedCursors::default(),
            main: None,
            regions: [None; 2],
            size,
        }
    }

    fn grid(&self) -> Grid {
        if self.parser.screen().alternate_screen() {
            Grid::Alternate
        } else {
            Grid::Main
        }
    }

    fn set_region(&mut self, top: u16, bottom: u16) {
        let rows = self.size.rows.get();
        let bottom = if bottom == 0 { rows } else { bottom.min(rows) };
        let top = top.max(1);
        if top >= bottom {
            return;
        }
        let grid = self.grid().index();
        self.regions[grid] = (top > 1 || bottom < rows).then_some(Region { top, bottom });
    }

    /// What the visible screen needs on a fresh terminal: its cells and
    /// input modes, the scroll region, saved cursors, then the cursor and
    /// the drawing attributes last, since the others move or reset them.
    fn visible_state(&self, grid: Grid) -> Vec<u8> {
        let region = self.regions[grid.index()];
        let screen = self.parser.screen();
        let mut out = screen.state_formatted();
        if let Some(Region { top, bottom }) = region {
            out.extend(format!("\x1b[{top};{bottom}r").bytes());
        }
        if let Some(at) = self.saved.csi {
            out.extend(cursor_to(at));
            out.extend(b"\x1b[s");
        }
        if let Some(at) = self.saved.dec {
            out.extend(cursor_to(at));
            out.extend(b"\x1b7");
        }
        out.extend(cursor_to(screen.cursor_position()));
        out.extend(screen.attributes_formatted());
        out
    }

    /// The cursor as a terminal reports it: from 1, and from the top margin
    /// while origin mode is set.
    fn reported_cursor(&self) -> ReportedCursor {
        let (row, col) = self.parser.screen().cursor_position();
        let origin = self.ledger.modes().contains(&Mode::Origin);
        let top = match self.regions[self.grid().index()] {
            Some(region) if origin => region.top,
            Some(_) | None => 1,
        };
        ReportedCursor {
            row: row.saturating_add(2).saturating_sub(top).max(1),
            col: col.saturating_add(1),
        }
    }

    fn answer(&self, asked: Asked, colours: Option<Colours>) -> Option<Vec<u8>> {
        let facts = Facts {
            cursor: self.reported_cursor(),
            size: self.size,
            keyboard_flags: self.ledger.kitty_stack().last().copied().unwrap_or(0),
            modes: self.ledger.modes(),
            colours,
        };
        reply(asked.query, &facts)
    }

    /// What `event`, which ended at `bytes[at]`, asks of the screen, with
    /// `bytes[from..at]` not yet given to `vt100`.
    fn act_on(&mut self, event: Event, bytes: &[u8], from: usize, at: usize) {
        self.parser.process(&bytes[from..at]);
        match event {
            Event::Switched(Switch::Enter) => {
                if self.grid() == Grid::Main {
                    self.main = Some(self.visible_state(Grid::Main));
                }
            }
            Event::Switched(Switch::Leave) | Event::RestoredCsi => {}
            Event::Region { top, bottom } => self.set_region(top, bottom),
            Event::Saved(kind) => self.saved.set(kind, self.parser.screen().cursor_position()),
            Event::Reset => {
                self.regions = [None; 2];
                self.saved = SavedCursors::default();
            }
        }
        self.parser.process(&bytes[at..=at]);
        if let (Event::RestoredCsi, Some(saved)) = (event, self.saved.csi) {
            self.parser.process(&cursor_to(saved));
        }
    }

    fn rows(&self, style: Style) -> Vec<Vec<u8>> {
        let (_, cols) = self.parser.screen().size();
        match style {
            Style::Styled => self
                .parser
                .screen()
                .rows_formatted(0, cols)
                .map(|mut row| {
                    row.extend(b"\x1b[0m");
                    row
                })
                .collect(),
            Style::Plain => self
                .parser
                .screen()
                .rows(0, cols)
                .map(String::into_bytes)
                .collect(),
        }
    }
}

impl Screen for Vt100Screen {
    fn feed(&mut self, cleaned: &[u8], asked: &[Asked], colours: Option<Colours>) -> Vec<u8> {
        let mut replies = Vec::new();
        let mut questions = asked.iter().peekable();
        let mut from = 0;
        for at in 0..=cleaned.len() {
            while let Some(question) = questions.next_if(|question| question.at == at) {
                if from < at {
                    self.parser.process(&cleaned[from..at]);
                    from = at;
                }
                replies.extend(self.answer(*question, colours).unwrap_or_default());
            }
            let Some(byte) = cleaned.get(at) else {
                break;
            };
            if let Some(event) = self.ledger.step(*byte) {
                self.act_on(event, cleaned, from, at);
                from = at + 1;
            }
        }
        self.parser.process(&cleaned[from..]);
        replies
    }

    fn resize(&mut self, size: TermSize) {
        if size == self.size {
            return;
        }
        self.parser
            .screen_mut()
            .set_size(size.rows.get(), size.cols.get());
        self.regions = [None; 2];
        self.main = None;
        self.size = size;
    }

    fn synchronized(&self) -> bool {
        self.ledger.synchronized()
    }

    fn replay(&self) -> Vec<u8> {
        let grid = self.grid();
        let mut out = b"\x1b[0m".to_vec();
        if grid == Grid::Alternate {
            if let Some(main) = &self.main {
                out.extend(main);
            }
            out.extend(b"\x1b[?1049h");
        }
        out.extend(self.visible_state(grid));
        for mode in self
            .ledger
            .modes()
            .iter()
            .filter(|mode| !SERIALISED_BY_THE_SCREEN.contains(mode))
        {
            let direction = if mode.starts_on() { 'l' } else { 'h' };
            out.extend(format!("\x1b[?{}{direction}", mode.number()).bytes());
        }
        for flags in self.ledger.kitty_stack() {
            out.extend(format!("\x1b[>{flags}u").bytes());
        }
        if let Some(level) = self.ledger.modify_other_keys() {
            out.extend(format!("\x1b[>4;{level}m").bytes());
        }
        if self.ledger.application_keypad() {
            out.extend(b"\x1b=");
        }
        if let Some(title) = self.ledger.title() {
            out.extend(b"\x1b]2;");
            out.extend(title);
            out.extend(b"\x1b\\");
        }
        out
    }

    fn dump(&mut self, scrollback: Scrollback, style: Style) -> Vec<u8> {
        let (rows, _) = self.parser.screen().size();
        let rows = usize::from(rows);
        let mut lines = Vec::new();
        if scrollback == Scrollback::Full {
            self.parser.screen_mut().set_scrollback(usize::MAX);
            let mut left = self.parser.screen().scrollback();
            while left > 0 {
                self.parser.screen_mut().set_scrollback(left);
                let taken = left.min(rows);
                lines.extend(self.rows(style).into_iter().take(taken));
                left -= taken;
            }
            self.parser.screen_mut().set_scrollback(0);
        }
        lines.extend(self.rows(style));
        lines.join(&b"\r\n"[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::size;

    impl Vt100Screen {
        fn feed_plain(&mut self, bytes: &[u8]) {
            self.feed(bytes, &[], None);
        }
    }

    fn screen(rows: u16, cols: u16) -> Vt100Screen {
        Vt100Screen::new(size(rows, cols), ScrollbackCap::DEFAULT)
    }

    fn differences(left: &vt100::Screen, right: &vt100::Screen) -> Vec<String> {
        crate::testing::screen_differences(left, right)
    }

    fn client_of(source: &Vt100Screen) -> vt100::Parser {
        let (rows, cols) = source.parser.screen().size();
        let mut client = vt100::Parser::new(rows, cols, 0);
        client.process(&source.replay());
        client
    }

    const LEAVE: &[u8] = b"\x1b[?1049l";

    fn feed_both(source: &mut Vt100Screen, client: &mut vt100::Parser, bytes: &[u8]) {
        source.feed_plain(bytes);
        client.process(bytes);
    }

    #[test]
    fn a_replay_of_styled_text_matches_cell_for_cell() {
        let mut source = screen(10, 40);
        source.feed_plain(
            "\x1b[1;31mred bold\x1b[0m plain\r\n\x1b[48;2;1;2;3m\x1b[4munder\x1b[0m wide: 日本\r\n"
                .as_bytes(),
        );
        source.feed_plain(b"\x1b[3;7r\x1b[5;5Hxyz\x1b[?25l");
        let client = client_of(&source);
        assert_eq!(
            differences(source.parser.screen(), client.screen()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_client_that_attaches_during_an_alternate_screen_program_sees_the_main_screen_after_it_quits()
     {
        let mut source = screen(10, 40);
        source.feed_plain(b"$ ls\r\nfile one\r\nfile two\r\n$ vim\r\n");
        source.feed_plain(b"\x1b[?1049h\x1b[2J\x1b[Hediting\x1b[5;1Hmore");
        let mut client = client_of(&source);
        assert!(client.screen().alternate_screen());
        feed_both(&mut source, &mut client, LEAVE);
        feed_both(&mut source, &mut client, b"$ ");
        assert_eq!(
            differences(source.parser.screen(), client.screen()),
            Vec::<String>::new()
        );
        assert!(source.parser.screen().contents().contains("file two"));
    }

    #[test]
    fn plain_rows_keep_the_gaps_that_cursor_moves_and_tabs_left() {
        let mut source = screen(2, 30);
        source.feed_plain(b"a\tb  c\x1b[5Cd");
        let plain = String::from_utf8(source.dump(Scrollback::Screen, Style::Plain)).expect("utf8");
        assert_eq!(plain.lines().next(), Some("a       b  c     d"));
        assert!(!plain.contains('\x1b'));
    }

    #[test]
    fn the_replay_carries_the_scroll_region_so_later_output_scrolls_the_same_rows() {
        let mut source = screen(10, 20);
        source.feed_plain(b"\x1b[?1049h\x1b[H\x1b[2J");
        for row in 1..=10 {
            source.feed_plain(format!("\x1b[{row};1Hrow {row}").as_bytes());
        }
        source.feed_plain(b"\x1b[3;8r\x1b[8;1H");
        let mut client = client_of(&source);
        for line in 0..6 {
            feed_both(
                &mut source,
                &mut client,
                format!("\r\nnew {line}").as_bytes(),
            );
        }
        assert_eq!(
            differences(source.parser.screen(), client.screen()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_replay_carries_a_cursor_saved_with_csi_s_and_restores_it_with_csi_u() {
        let mut source = screen(10, 20);
        source.feed_plain(b"\x1b[4;6H\x1b[s\x1b[9;1Hfooter");
        source.feed_plain(b"\x1b[u");
        assert_eq!(source.parser.screen().cursor_position(), (3, 5));
        let mut client = screen(10, 20);
        client.feed_plain(&source.replay());
        for bytes in [&b"\x1b[8;2Hmore\x1b[u!"[..], b"\x1b[2;2Hx\x1b[u?"] {
            source.feed_plain(bytes);
            client.feed_plain(bytes);
        }
        assert_eq!(
            differences(source.parser.screen(), client.parser.screen()),
            Vec::<String>::new()
        );
        assert_eq!(client.parser.screen().cursor_position(), (3, 6));
    }

    #[test]
    fn the_replay_carries_a_cursor_saved_with_esc_7() {
        let mut source = screen(10, 20);
        source.feed_plain(b"\x1b[4;6H\x1b7\x1b[9;1Hfooter");
        let mut client = client_of(&source);
        feed_both(&mut source, &mut client, b"\x1b8!");
        assert_eq!(
            differences(source.parser.screen(), client.screen()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_chunk_that_ends_inside_the_sequence_that_enters_the_alternate_screen_still_saves_the_main_screen()
     {
        let mut source = screen(6, 20);
        source.feed_plain(b"main text\x1b[?10");
        source.feed_plain(b"49h\x1b[2Jalt");
        let mut client = client_of(&source);
        feed_both(&mut source, &mut client, LEAVE);
        assert_eq!(
            differences(source.parser.screen(), client.screen()),
            Vec::<String>::new()
        );
        assert!(client.screen().contents().contains("main text"));
    }

    #[test]
    fn modes_the_screen_does_not_serialise_are_replayed() {
        let mut source = screen(6, 20);
        source.feed_plain(b"\x1b[?1004h\x1b[>1u\x1b[>5u\x1b[>4;2m\x1b]2;a title\x07\x1b=\x1b[?7l");
        let replay = String::from_utf8(source.replay()).expect("utf-8");
        for wanted in [
            "\x1b[?1004h",
            "\x1b[>1u\x1b[>5u",
            "\x1b[>4;2m",
            "\x1b]2;a title\x1b\\",
            "\x1b=",
            "\x1b[?7l",
        ] {
            assert!(
                replay.contains(wanted),
                "{wanted:?} missing from {replay:?}"
            );
        }
    }

    #[test]
    fn a_program_that_enters_the_alternate_screen_twice_keeps_the_first_main_screen() {
        let mut source = screen(6, 20);
        source.feed_plain(b"shell\x1b[?1049h\x1b[2Jone\x1b[?1049l\x1b[?1049h\x1b[2Jtwo");
        let mut client = client_of(&source);
        feed_both(&mut source, &mut client, LEAVE);
        assert_eq!(
            differences(source.parser.screen(), client.screen()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_resize_on_the_alternate_screen_forgets_the_main_screen_it_cannot_redraw() {
        let mut source = screen(6, 20);
        source.feed_plain(b"shell\x1b[?1049h\x1b[2Jalt");
        source.resize(size(8, 30));
        let client = client_of(&source);
        assert!(client.screen().alternate_screen());
        assert_eq!(client.screen().size(), (8, 30));
    }

    #[test]
    fn a_mode_47_reset_that_vt100_obeys_leaves_the_replay_on_the_main_screen() {
        let mut source = screen(6, 20);
        source.feed_plain(b"shell\x1b[?1049h\x1b[2Jalt\x1b[?47l");
        assert!(!source.parser.screen().alternate_screen());
        let client = client_of(&source);
        assert!(!client.screen().alternate_screen());
        assert_eq!(
            differences(source.parser.screen(), client.screen()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_soft_or_full_reset_forgets_the_scroll_region() {
        for reset in [&b"\x1b[!p"[..], b"\x1bc"] {
            let mut source = screen(10, 20);
            source.feed_plain(b"\x1b[3;8r");
            assert!(String::from_utf8_lossy(&source.replay()).contains("\x1b[3;8r"));
            source.feed_plain(reset);
            let replay = String::from_utf8_lossy(&source.replay()).into_owned();
            assert!(!replay.contains("\x1b[3;8r"), "{reset:?}");
        }
    }

    #[test]
    fn a_region_set_in_the_same_chunk_after_a_screen_switch_belongs_to_the_screen_that_is_showing()
    {
        let mut source = screen(10, 20);
        source.feed_plain(b"\x1b[?1049h\x1b[3;8r\x1b[?47l\x1b[5;9r");
        let client = client_of(&source);
        assert!(!client.screen().alternate_screen());
        assert_eq!(
            source.regions[Grid::Main.index()],
            Some(Region { top: 5, bottom: 9 })
        );
        assert_eq!(
            source.regions[Grid::Alternate.index()],
            Some(Region { top: 3, bottom: 8 })
        );
    }

    #[test]
    fn synchronized_output_is_reported_while_a_frame_is_open() {
        let mut source = screen(6, 20);
        source.feed_plain(b"\x1b[?2026hhalf a frame");
        assert!(source.synchronized());
        source.feed_plain(b"\x1b[?2026l");
        assert!(!source.synchronized());
    }

    #[test]
    fn a_screen_dump_is_the_visible_rows_and_a_full_dump_adds_what_scrolled_off() {
        let mut source = screen(3, 10);
        for line in 1..=8 {
            source.feed_plain(format!("line {line}\r\n").as_bytes());
        }
        let text = |bytes: Vec<u8>| {
            let mut parser = vt100::Parser::new(40, 10, 0);
            parser.process(&bytes);
            parser.screen().contents()
        };
        let screen_only = text(source.dump(Scrollback::Screen, Style::Styled));
        assert!(screen_only.contains("line 8"), "{screen_only}");
        assert!(!screen_only.contains("line 1\n"), "{screen_only}");
        let full = text(source.dump(Scrollback::Full, Style::Styled));
        let lines: Vec<_> = full.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(
            lines,
            [
                "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8"
            ]
        );
    }

    #[test]
    fn a_dump_leaves_the_screen_where_it_was_so_the_next_replay_is_unchanged() {
        let mut source = screen(3, 10);
        for line in 1..=8 {
            source.feed_plain(format!("line {line}\r\n").as_bytes());
        }
        let before = source.replay();
        source.dump(Scrollback::Full, Style::Styled);
        assert_eq!(source.replay(), before);
    }

    #[test]
    fn scrollback_is_capped_by_bytes_so_a_wider_screen_keeps_fewer_rows() {
        let cap = ScrollbackCap::bytes(NonZeroU64::new(32 * 20 * 4).expect("non-zero"));
        let mut source = Vt100Screen::new(size(2, 20), cap);
        for line in 1..=40 {
            source.feed_plain(format!("line {line}\r\n").as_bytes());
        }
        let mut parser = vt100::Parser::new(80, 20, 0);
        parser.process(&source.dump(Scrollback::Full, Style::Styled));
        let kept = parser
            .screen()
            .contents()
            .lines()
            .filter(|line| !line.is_empty())
            .count();
        assert_eq!(kept, 4 + 1, "four rows of scrollback and the current row");
    }

    use agent_core::config::Osc52;
    use agent_core::theme::Rgb;

    use crate::egress::{Egress, Policy, Recipient};

    const POLICY: Policy = Policy {
        clipboard: Osc52::Off,
    };

    /// A screen and the parser that finds what it is asked, as the host
    /// runs them.
    struct Terminal {
        screen: Vt100Screen,
        egress: Egress,
        colours: Option<Colours>,
    }

    impl Terminal {
        fn new(rows: u16, cols: u16) -> Self {
            Self {
                screen: screen(rows, cols),
                egress: Egress::new(POLICY),
                colours: None,
            }
        }

        fn print(&mut self, bytes: &[u8]) -> String {
            let filtered = self.egress.feed(bytes);
            let replies = self
                .screen
                .feed(&filtered.owner, &filtered.asked, self.colours);
            String::from_utf8(replies).expect("replies are text")
        }
    }

    #[test]
    fn a_cursor_position_report_reads_the_cursor_at_the_byte_that_asked() {
        let mut terminal = Terminal::new(10, 40);
        assert_eq!(terminal.print(b"\x1b[5;10H\x1b[6n"), "\x1b[5;10R");
        assert_eq!(
            terminal.print(b"\x1b[1;1Hab\x1b[6ncd\x1b[6n\x1b[?6n"),
            "\x1b[1;3R\x1b[1;5R\x1b[?1;5;1R"
        );
    }

    #[test]
    fn a_cursor_position_report_split_across_reads_still_reads_the_cursor_where_it_ended() {
        let mut terminal = Terminal::new(10, 40);
        assert_eq!(terminal.print(b"\x1b[3;4Hxy\x1b["), "");
        assert_eq!(terminal.print(b"6n"), "\x1b[3;6R");
    }

    #[test]
    fn a_cursor_position_report_counts_from_the_top_margin_in_origin_mode() {
        let mut terminal = Terminal::new(10, 40);
        assert_eq!(
            terminal.print(b"\x1b[3;8r\x1b[?6h\x1b[2;4H\x1b[6n"),
            "\x1b[2;4R"
        );
        assert_eq!(terminal.screen.parser.screen().cursor_position(), (3, 3));
        assert_eq!(terminal.print(b"\x1b[?6l\x1b[6n"), "\x1b[1;1R");
    }

    #[test]
    fn the_keyboard_flags_reply_follows_the_pushes_and_pops_the_program_made() {
        let mut terminal = Terminal::new(6, 20);
        assert_eq!(terminal.print(b"\x1b[?u"), "\x1b[?0u");
        assert_eq!(terminal.print(b"\x1b[>1u\x1b[>5u\x1b[?u"), "\x1b[?5u");
        assert_eq!(terminal.print(b"\x1b[<u\x1b[?u"), "\x1b[?1u");
        assert_eq!(terminal.print(b"\x1b[=8;1u\x1b[?u"), "\x1b[?8u");
        assert_eq!(terminal.print(b"\x1b[<5u\x1b[?u"), "\x1b[?0u");
    }

    #[test]
    fn a_mode_report_says_set_reset_or_unknown_from_what_the_program_set() {
        let mut terminal = Terminal::new(6, 20);
        assert_eq!(terminal.print(b"\x1b[?2026$p"), "\x1b[?2026;2$y");
        assert_eq!(terminal.print(b"\x1b[?2004h\x1b[?2004$p"), "\x1b[?2004;1$y");
        assert_eq!(terminal.print(b"\x1b[?2004l\x1b[?2004$p"), "\x1b[?2004;2$y");
        assert_eq!(
            terminal.print(b"\x1b[?7$p\x1b[?25$p"),
            "\x1b[?7;1$y\x1b[?25;1$y"
        );
        assert_eq!(
            terminal.print(b"\x1b[?7l\x1b[?25l\x1b[?7$p\x1b[?25$p"),
            "\x1b[?7;2$y\x1b[?25;2$y"
        );
        assert_eq!(terminal.print(b"\x1b[?1049h\x1b[?1049$p"), "\x1b[?1049;1$y");
        assert_eq!(terminal.print(b"\x1b[?9999$p"), "\x1b[?9999;0$y");
        assert_eq!(terminal.print(b"\x1b[?1001$p"), "\x1b[?1001;0$y");
        assert_eq!(terminal.print(b"\x1b[4$p"), "\x1b[4;0$y");
    }

    #[test]
    fn the_identity_replies_are_the_documented_bytes() {
        let mut terminal = Terminal::new(24, 80);
        assert_eq!(terminal.print(b"\x1b[c"), "\x1b[?62;22c");
        assert_eq!(terminal.print(b"\x1b[0c"), "\x1b[?62;22c");
        assert_eq!(terminal.print(b"\x1bZ"), "\x1b[?62;22c");
        assert_eq!(terminal.print(b"\x1b[>c"), "\x1b[>1;10;0c");
        assert_eq!(terminal.print(b"\x1b[5n"), "\x1b[0n");
        assert_eq!(
            terminal.print(b"\x1b[>0q"),
            format!("\x1bP>|agent-host({})\x1b\\", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(terminal.print(b"\x1b[18t"), "\x1b[8;24;80t");
        assert_eq!(terminal.print(b"\x1b[14t"), "\x1b[4;384;640t");
        terminal.screen.resize(size(30, 100));
        assert_eq!(terminal.print(b"\x1b[18t"), "\x1b[8;30;100t");
    }

    #[test]
    fn colour_queries_are_answered_from_the_owner_with_the_terminator_they_used_and_not_without() {
        let mut terminal = Terminal::new(6, 20);
        assert_eq!(terminal.print(b"\x1b]11;?\x07\x1b]10;?\x1b\\"), "");
        terminal.colours = Some(Colours {
            foreground: Rgb {
                r: 0xe0,
                g: 0x01,
                b: 0xff,
            },
            background: Rgb {
                r: 0,
                g: 0x10,
                b: 0x20,
            },
        });
        assert_eq!(
            terminal.print(b"\x1b]11;?\x07\x1b]10;?\x1b\\"),
            "\x1b]11;rgb:0000/1010/2020\x07\x1b]10;rgb:e0e0/0101/ffff\x1b\\"
        );
    }

    #[test]
    fn a_query_in_the_middle_of_a_chunk_leaves_the_screen_as_the_program_meant_it() {
        let mut terminal = Terminal::new(6, 20);
        let mut plain = screen(6, 20);
        let bytes: &[u8] = b"one\x1b[6ntwo\x1b[c\r\nthree";
        terminal.print(bytes);
        plain.feed_plain(b"one\x1b[6ntwo\x1b[c\r\nthree");
        assert_eq!(
            differences(terminal.screen.parser.screen(), plain.parser.screen()),
            Vec::<String>::new()
        );
    }

    fn replay_after(bytes: &[u8]) -> Vec<u8> {
        let mut terminal = Terminal::new(10, 40);
        terminal.print(bytes);
        terminal.screen.replay()
    }

    #[test]
    fn a_replay_passes_the_allowlist_untouched_for_the_owner() {
        for bytes in [
            &b"\x1b[1;31mred bold\x1b[0m plain\r\n\x1b[48;2;1;2;3m\x1b[4munder\x1b[0m wide: \xe6\x97\xa5\xe6\x9c\xac\r\n"[..],
            b"$ ls\r\nfile\r\n\x1b[?1049h\x1b[2J\x1b[Hediting\x1b[5;1Hmore\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[?2004h",
            b"\x1b[3;7r\x1b[5;5Hxyz\x1b[4;6H\x1b[s\x1b7\x1b[9;1Hfooter\x1b[?1004h\x1b[>1u\x1b[>5u\x1b[>4;2m\x1b]2;a title\x07\x1b=\x1b[?7l\x1b[?1h",
            b"\x1b[?47h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1005h\x1b[?1015h\x1b[?9h\x1b[?12h\x1b[?5h\x1b[?6h\x1b[?1047h",
        ] {
            let replay = replay_after(bytes);
            let cleaned = Egress::new(POLICY).clean(&replay, Recipient::Owner);
            assert_eq!(
                String::from_utf8_lossy(&cleaned.bytes),
                String::from_utf8_lossy(&replay)
            );
            assert!(cleaned.refused.is_empty(), "{}", cleaned.refused);
        }
    }

    #[test]
    fn a_replay_never_carries_what_live_output_was_not_allowed_to() {
        let replay = replay_after(
            b"text\x1b[?9999h\x1b[?1001h\x1b[?3h\x1b]2;owner only title\x07\x1b]1337;SetUserVar=a=Yg==\x07\x1b]7;file:///tmp\x07",
        );
        let owner = Egress::new(POLICY).clean(&replay, Recipient::Owner);
        let watcher = Egress::new(POLICY).clean(&replay, Recipient::Watcher);
        let owner_text = String::from_utf8_lossy(&owner.bytes).into_owned();
        let watcher_text = String::from_utf8_lossy(&watcher.bytes).into_owned();
        assert!(owner_text.contains("owner only title"), "{owner_text:?}");
        assert!(
            !watcher_text.contains("owner only title"),
            "{watcher_text:?}"
        );
        for text in [&owner_text, &watcher_text] {
            for forbidden in ["?9999", "?1001", "?3h", "1337", "]7;"] {
                assert!(!text.contains(forbidden), "{forbidden} in {text:?}");
            }
        }
        assert!(owner.refused.is_empty(), "{}", owner.refused);
        assert!(watcher.refused.is_empty(), "{}", watcher.refused);
    }

    #[test]
    fn a_dump_of_a_styled_screen_passes_the_allowlist_untouched() {
        let mut terminal = Terminal::new(4, 30);
        terminal.print(
            b"\x1b[1;31mred\x1b[0m \xe6\x97\xa5\xe6\x9c\xac a\x01b\x1b]2;t\x07\x1b_G\x1b\\c",
        );
        let dump = terminal.screen.dump(Scrollback::Full, Style::Styled);
        let cleaned = Egress::new(POLICY).clean(&dump, Recipient::Watcher);
        assert_eq!(cleaned.bytes, dump);
        assert!(cleaned.refused.is_empty(), "{}", cleaned.refused);
    }

    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next() % u64::try_from(bound).unwrap_or(u64::MAX)).unwrap_or(0)
        }
    }

    /// What the table forwards.
    fn forwarded_fragments() -> Vec<Vec<u8>> {
        [
            &b"text "[..],
            "h\u{e9}llo \u{65e5}\u{672c} ".as_bytes(),
            b"\r\n",
            b"\x08",
            b"\t",
            b"\x1b[1;31m",
            b"\x1b[0m",
            b"\x1b[38;2;1;2;3m",
            b"\x1b[H",
            b"\x1b[5;10H",
            b"\x1b[2J",
            b"\x1b[K",
            b"\x1b[3A",
            b"\x1b[2L",
            b"\x1b[2P",
            b"\x1b[2S",
            b"\x1b[3;6r",
            b"\x1b[r",
            b"\x1b7",
            b"\x1b8",
            b"\x1bM",
            b"\x1bD",
            b"\x1bE",
            b"\x1bc",
            b"\x1b[!p",
            b"\x1b[?25l",
            b"\x1b[?25h",
            b"\x1b[?1049h",
            b"\x1b[?1049l",
            b"\x1b[?47h",
            b"\x1b[?47l",
            b"\x1b[?1h",
            b"\x1b[?7l",
            b"\x1b[?7h",
            b"\x1b[?2004h",
            b"\x1b[?1000;1006h",
            b"\x1b[?1000;1006l",
            b"\x1b[?1004h",
            b"\x1b[>1u",
            b"\x1b[<u",
            b"\x1b[=4;2u",
            b"\x1b[>4;2m",
            b"\x1b[>4;m",
            b"\x1b[>4n",
            b"\x1b=",
            b"\x1b>",
            b"\x1b]2;a title\x07",
            b"\x1b]0;other\x1b\\",
            b"\x1b]8;;http://example.com\x07link\x1b]8;;\x07",
            b"\x1b]52;c;aGk=\x07",
            b"\x1b[2 q",
            b"\x1b[22t",
            b"\x1b[23t",
            b"\x1b(0",
            b"\x1b(B",
            b"\x1b[3b",
        ]
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect()
    }

    /// What the host answers itself.
    fn answered_fragments() -> Vec<Vec<u8>> {
        [
            &b"\x1b[6n"[..],
            b"\x1b[c",
            b"\x1b[>c",
            b"\x1bZ",
            b"\x1b]11;?\x07",
            b"\x1b[?u",
            b"\x1b[?2026$p",
            b"\x1b[18t",
        ]
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect()
    }

    /// What is dropped, trimmed or cut short.
    fn hostile_fragments() -> Vec<Vec<u8>> {
        [
            &b"\x1b[?1049;2031h"[..],
            b"\x1b[?2031;1049l",
            b"\x1b[?9999h",
            b"\x1b[?1001;2004h",
            b"\x1b[?3h",
            b"\x1b[1;2;3;4;5T",
            b"\x1b[2\x08;5H",
            b"\x1b[2\n;5H",
            b"\x1b[3\x18m",
            b"\x1b[3\x1b[31m",
            b"\x9b31m",
            b"\xff",
            b"\xe2\x82",
            b"\x01",
            b"\x7f",
            b"\x18",
            b"\x1b\x1b",
            b"\x1b[3",
            b"\x1b]0;unterminated",
            b"\x1b]52;c;?\x07",
            b"\x1b]1337;SetUserVar=a=Yg==\x07",
            b"\x1b]7;file:///tmp\x1b\\",
            b"\x1bPq#0;2;0;0;0\x1b\\",
            b"\x1b_Gf=100;AAAA\x1b\\",
            b"\x1b^pm\x1b\\",
            b"\x1bXsos\x1b\\",
            b"\x1b[<1;2;3M",
        ]
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect()
    }

    /// Every class the egress table forwards, answers or drops, and the ways
    /// a program can cut a sequence short.
    fn fragments() -> Vec<Vec<u8>> {
        let long = |start: &[u8], filler: u8, count: usize, end: &[u8]| {
            let mut bytes = start.to_vec();
            bytes.extend(std::iter::repeat_n(filler, count));
            bytes.extend_from_slice(end);
            bytes
        };
        let mut all = forwarded_fragments();
        all.extend(answered_fragments());
        all.extend(hostile_fragments());
        all.push(long(b"\x1b]1337;File=inline=1:", b'A', 20_000, b"\x07"));
        all.push(long(b"\x1bPq", b'#', 20_000, b"\x1b\\"));
        all.push(long(b"\x1b[", b'1', 400, b"H"));
        all.push(long(b"\x1b]2;", b'T', 5000, b"\x07"));
        all.push(long(b"\x1b(", b' ', 400, b"0"));
        all
    }

    /// How two screens differ, leaving out two things a replay doesn't
    /// carry and the parser doesn't forward differently: the mouse protocol
    /// after a program reset it on the alternate screen it set on the main
    /// one, and a cursor waiting to wrap at the right edge, which a replay
    /// puts on the last column.
    fn visible_differences(left: &vt100::Screen, right: &vt100::Screen) -> Vec<String> {
        let last_column = left.size().1.saturating_sub(1);
        let comparable = |side: &str| {
            let side = side.split(" mouse ").next().unwrap_or(side);
            let Some(rest) = side.strip_prefix("cursor (") else {
                return side.to_owned();
            };
            let Some((position, rest)) = rest.split_once(')') else {
                return side.to_owned();
            };
            let (row, column) = position.split_once(", ").unwrap_or((position, "0"));
            let column = column.parse::<u16>().unwrap_or(0).min(last_column);
            format!("cursor ({row}, {column}){rest}")
        };
        differences(left, right)
            .into_iter()
            .filter(|line| {
                line.split_once(" | ")
                    .is_none_or(|(a, b)| comparable(a) != comparable(b))
            })
            .collect()
    }

    fn screen_of(egress: &Egress, bytes: &[u8]) -> Vec<u8> {
        egress.clean(bytes, Recipient::Owner).bytes
    }

    #[test]
    fn a_live_client_a_late_client_and_a_reference_fed_the_cleaned_stream_end_on_the_same_screen() {
        let fragments = fragments();
        let (rows, cols) = (8, 30);
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        for round in 0..400 {
            let mut stream = Vec::new();
            for _ in 0..=random.below(60) {
                stream.extend_from_slice(&fragments[random.below(fragments.len())]);
                if random.below(8) == 0 {
                    stream.push(u8::try_from(random.below(256)).unwrap_or(0));
                }
            }
            let mut egress = Egress::new(POLICY);
            let mut host = screen(rows, cols);
            let mut live = vt100::Parser::new(rows, cols, 0);
            let mut left = stream.as_slice();
            while !left.is_empty() {
                let take = (1 + random.below(40)).min(left.len());
                let (chunk, rest) = left.split_at(take);
                left = rest;
                let filtered = egress.feed(chunk);
                host.feed(&filtered.owner, &filtered.asked, None);
                live.process(&filtered.owner);
            }
            let mut late = vt100::Parser::new(rows, cols, 0);
            late.process(&screen_of(&egress, &host.replay()));
            let mut reference = vt100::Parser::new(rows, cols, 0);
            reference.process(&Egress::new(POLICY).feed(&stream).owner);
            let shown: String = format!("{stream:?}").chars().take(300).collect();
            let context = format!("round {round}: {shown}");
            assert_eq!(
                visible_differences(live.screen(), reference.screen())
                    .into_iter()
                    .take(5)
                    .collect::<Vec<_>>(),
                Vec::<String>::new(),
                "live against reference, {context}"
            );
            assert_eq!(
                visible_differences(late.screen(), live.screen())
                    .into_iter()
                    .take(5)
                    .collect::<Vec<_>>(),
                Vec::<String>::new(),
                "late against live, {context}"
            );
        }
    }
}
