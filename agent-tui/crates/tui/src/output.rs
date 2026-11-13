//! What a background command has printed, as [0]'s command document shows it.

use std::time::{Duration, Instant, SystemTime};

use agent_core::ports::{CommandOutput, Cut};

use crate::cause::Cause;
use crate::model::{Moment, Presence, Ticket};

/// A command's output is read again this long after the last answer.
pub(crate) const OUTPUT_REFRESH: Duration = Duration::from_secs(3);

/// A request the worker hasn't answered for this long is made again, and
/// until it is answered the section says reading is taking long.
pub(crate) const OUTPUT_STALLED: Duration = Duration::from_secs(15);

/// Output lines kept for a document: its last ones, so that a file of tiny
/// lines is as cheap as any other.
const MAX_LINES: usize = 5000;

/// A command's output as printable lines, runs of blank ones made one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    lines: Vec<String>,
    /// How many lines before these were left out.
    capped: usize,
}

/// `text` as printable lines with each run of blank ones made one.
fn collapsed(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in agent_term::output_lines(text) {
        let blank = line.trim().is_empty();
        if blank && lines.last().is_some_and(String::is_empty) {
            continue;
        }
        lines.push(if blank { String::new() } else { line });
    }
    lines
}

impl Shown {
    pub fn of(text: &str) -> Self {
        let mut lines = collapsed(text);
        let capped = lines.len().saturating_sub(MAX_LINES);
        lines.drain(..capped);
        Self { lines, capped }
    }

    #[cfg(test)]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// How many lines `newer` no longer starts with, when it is these with
    /// some taken off the front and more added at the end: what is left of
    /// these is the longest run at their end that `newer` starts with. The
    /// last line may have been unfinished, so it isn't compared. All of them
    /// when `newer` has nothing in common. Equal lines make it ambiguous,
    /// which is why the byte offsets are used when they can be.
    fn dropped_from(&self, newer: &Self) -> Option<usize> {
        let settled = self.lines.len().saturating_sub(1);
        if settled == 0 {
            return Some(0);
        }
        let kept = &self.lines[..settled];
        let wanted = &newer.lines;
        // The longest prefix of `wanted` that ends `kept`, by the prefix
        // function of `wanted`, a gap, then `kept`.
        let at = |n: usize| -> Option<&str> {
            match n.cmp(&wanted.len()) {
                std::cmp::Ordering::Less => Some(wanted[n].as_str()),
                std::cmp::Ordering::Equal => None,
                std::cmp::Ordering::Greater => Some(kept[n - wanted.len() - 1].as_str()),
            }
        };
        let total = wanted.len() + 1 + kept.len();
        let mut prefix = vec![0usize; total];
        for n in 1..total {
            let mut length = prefix[n - 1];
            while length > 0 && (at(n) != at(length) || at(n).is_none()) {
                length = prefix[length - 1];
            }
            if at(n) == at(length) && at(n).is_some() {
                length += 1;
            }
            prefix[n] = length;
        }
        let overlap = prefix[total - 1];
        (overlap > 0).then(|| settled - overlap)
    }
}

/// How much of the front of the text an answer took off, in lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dropped {
    Lines(usize),
    /// None of the old text is in the new.
    Everything,
}

/// What the file's growth since `old` took off the front of the text: the
/// lines in the bytes between where the two start, when the new text carries
/// on from the old. Otherwise what the lines say.
fn dropped(old: &Read, new: &Read) -> Option<Dropped> {
    let (CommandOutput::Tail(before), CommandOutput::Tail(after)) = (&old.output, &new.output)
    else {
        return None;
    };
    let (old_shown, new_shown) = (old.shown.as_ref()?, new.shown.as_ref()?);
    let carried_on = || {
        let bytes = usize::try_from(after.from().checked_sub(before.from())?).ok()?;
        let rest = before.text().get(bytes..)?;
        let compared = rest.len().min(after.text().len());
        (rest.as_bytes()[..compared] == after.text().as_bytes()[..compared]).then_some(bytes)
    };
    Some(match carried_on() {
        Some(bytes) => {
            let gone = if bytes == 0 {
                0
            } else {
                collapsed(&before.text()[..bytes]).len()
            };
            let left = (gone + new_shown.capped).saturating_sub(old_shown.capped);
            Dropped::Lines(left.min(old_shown.lines.len()))
        }
        None => old_shown
            .dropped_from(new_shown)
            .map_or(Dropped::Everything, Dropped::Lines),
    })
}

/// [`Shown`]'s rows at a width.
#[derive(Debug, PartialEq, Eq)]
pub struct Wrapped {
    width: usize,
    /// The document row of the `Output` heading.
    heading: usize,
    /// The answer it was made from has been replaced.
    stale: bool,
    rows: Vec<String>,
    /// How many rows each line took.
    per_line: Vec<usize>,
}

impl Wrapped {
    pub fn rows(&self) -> &[String] {
        &self.rows
    }

    fn of(
        shown: &Shown,
        width: usize,
        heading: usize,
        wrap: fn(&str, usize) -> Vec<String>,
    ) -> Self {
        let mut rows = Vec::new();
        let mut per_line = Vec::with_capacity(shown.lines.len());
        for line in &shown.lines {
            let before = rows.len();
            if line.is_empty() {
                rows.push(String::new());
            } else {
                rows.extend(wrap(line, width));
            }
            per_line.push(rows.len() - before);
        }
        Self {
            width,
            heading,
            stale: false,
            rows,
            per_line,
        }
    }
}

/// What the harness said of a command's output and when.
pub struct Read {
    pub output: CommandOutput,
    pub at: SystemTime,
    /// Its text as lines, when it has any.
    pub shown: Option<Shown>,
}

#[derive(Debug, Default, Clone, Copy)]
enum Request {
    #[default]
    Never,
    /// With the worker. A request it has held longer than [`OUTPUT_STALLED`]
    /// is made again, and the worker serves a key one request at a time, so
    /// the answer to any request from `first` on counts: reads that always
    /// take longer than that still end in an answer. `presence` is the least
    /// an answer can know: `Listed` when any request of the wait was asked
    /// while the command was listed, since an answer may predate what came
    /// after. `since` is when this wait began.
    Waiting {
        first: Ticket,
        presence: Presence,
        at: Instant,
        since: Instant,
    },
    /// Asked at `at` while the command was `presence`.
    Answered {
        at: Instant,
        presence: Presence,
        looking: Looking,
    },
}

/// Whether the harness had found where the command writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Looking {
    Done,
    /// It was still looking, so asking again can find more.
    Still,
}

/// What an answer to a read came to.
#[derive(Debug, PartialEq, Eq)]
pub struct Heard {
    /// The read failed where the last one hadn't, for the user to be told.
    pub new_failure: Option<Cause>,
    /// How the document's rows moved, which a paused scroll follows.
    pub moved: Moved,
}

/// How rows moved when an answer changed the document, for a scroll that was
/// not following its end to follow the text it was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved {
    /// The row of the `Output` heading. Nothing at or above it moves.
    heading: usize,
    /// The row the text started on.
    text: usize,
    /// How many more rows came between the heading and the text.
    above: isize,
    /// What the answer took off the front of the text.
    dropped: DroppedRows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DroppedRows {
    Rows(usize),
    Everything,
}

fn signed(rows: usize) -> isize {
    isize::try_from(rows).unwrap_or(isize::MAX)
}

impl Moved {
    /// Nothing moved.
    fn none() -> Self {
        Self {
            heading: usize::MAX,
            text: usize::MAX,
            above: 0,
            dropped: DroppedRows::Rows(0),
        }
    }

    /// Where a scroll of `scroll` goes. A row above the heading stays. One on
    /// a row between the heading and the text moves with the rows that came
    /// or went there, and no higher than the first row under the heading. One
    /// in the text moves with them and with the rows that left, and no higher
    /// than the text's first row, to which a text that was replaced as a whole
    /// takes it.
    pub fn scroll(self, scroll: usize) -> usize {
        if scroll <= self.heading {
            return scroll;
        }
        let first = self.heading + 1;
        if scroll < self.text {
            return scroll.saturating_add_signed(self.above).max(first);
        }
        let text = self.text.saturating_add_signed(self.above);
        match self.dropped {
            DroppedRows::Everything => text,
            DroppedRows::Rows(gone) => scroll
                .saturating_add_signed(self.above)
                .saturating_sub(gone)
                .max(text),
        }
    }
}

/// What reading a background command's output has found, and where its latest
/// request stands. A failed read keeps the last output it found.
#[derive(Default)]
pub struct OutputWatch {
    request: Request,
    last: Option<Read>,
    failure: Option<Cause>,
    /// The rows of `last` at the width [0] has, and where its document's
    /// `Output` heading is. Settled with the model, so that what an answer
    /// moved is known without having drawn.
    wrapped: Option<Wrapped>,
}

impl OutputWatch {
    /// Whether to ask at `now`. A command its agent lists is asked again
    /// [`OUTPUT_REFRESH`] after its last answer. One it stopped listing is
    /// asked once more if its last read came while it was listed, so that
    /// what it printed since isn't missed, and again while the harness is
    /// still looking for where it writes.
    pub fn due(&self, presence: Presence, now: Instant) -> bool {
        match (self.request, presence) {
            (Request::Never, _)
            | (
                Request::Answered {
                    presence: Presence::Listed,
                    ..
                },
                Presence::Ended,
            ) => true,
            (Request::Waiting { at, .. }, _) => now >= at + OUTPUT_STALLED,
            (Request::Answered { at, .. }, Presence::Listed) => now >= at + OUTPUT_REFRESH,
            (
                Request::Answered {
                    at,
                    presence: Presence::Ended,
                    looking,
                },
                Presence::Ended,
            ) => looking == Looking::Still && now >= at + OUTPUT_REFRESH,
        }
    }

    /// It was asked for, as `ticket`, at `now`.
    pub fn ask(&mut self, ticket: Ticket, presence: Presence, now: Instant) {
        let (first, presence, since) = match self.request {
            Request::Waiting {
                first,
                presence: before,
                since,
                ..
            } => {
                let least = if before == Presence::Listed || presence == Presence::Listed {
                    Presence::Listed
                } else {
                    Presence::Ended
                };
                (first, least, since)
            }
            Request::Never | Request::Answered { .. } => (ticket, presence, now),
        };
        self.request = Request::Waiting {
            first,
            presence,
            at: now,
            since,
        };
    }

    /// Whether the first answer is taking long.
    pub fn stalled(&self, now: Instant) -> bool {
        matches!(self.request, Request::Waiting { since, .. } if now >= since + OUTPUT_STALLED)
    }

    /// An answer to the request `ticket` named, at `moment`. One to a request
    /// from before this wait began is dropped.
    pub fn hear(
        &mut self,
        ticket: Ticket,
        result: Result<CommandOutput, Cause>,
        moment: Moment,
    ) -> Option<Heard> {
        let Request::Waiting {
            first, presence, ..
        } = self.request
        else {
            return None;
        };
        if ticket < first {
            return None;
        }
        let looking = if matches!(result, Ok(CommandOutput::Searching)) {
            Looking::Still
        } else {
            Looking::Done
        };
        self.request = Request::Answered {
            at: moment.mono,
            presence,
            looking,
        };
        let was_failing = self.failure.is_some();
        let above = self.rows_above_text();
        let mut gone = DroppedRows::Rows(0);
        let mut new_failure = None;
        match result {
            Err(cause) => {
                new_failure = (!was_failing).then(|| cause.clone());
                self.failure = Some(cause);
            }
            Ok(output) => {
                self.failure = None;
                gone = self.replace(output, moment.wall);
            }
        }
        let moved = match &self.wrapped {
            Some(wrapped) => Moved {
                heading: wrapped.heading,
                text: wrapped.heading + 1 + above,
                above: signed(self.rows_above_text()) - signed(above),
                dropped: gone,
            },
            None => Moved::none(),
        };
        Some(Heard { new_failure, moved })
    }

    /// The rows between the heading and the text: the failure and the note
    /// that the text is cut.
    fn rows_above_text(&self) -> usize {
        let cut = self.last.as_ref().is_some_and(
            |last| matches!(&last.output, CommandOutput::Tail(tail) if tail.cut() != Cut::Whole),
        );
        usize::from(self.failure.is_some()) + usize::from(cut)
    }

    /// Keeps `output` as the last, and says how much of the text's front it
    /// took off, in rows.
    fn replace(&mut self, output: CommandOutput, at: SystemTime) -> DroppedRows {
        let shown = match &output {
            CommandOutput::Tail(tail) => Some(Shown::of(tail.text())),
            CommandOutput::Unlocated
            | CommandOutput::Searching
            | CommandOutput::Gone
            | CommandOutput::Empty => None,
        };
        let new = Read { output, at, shown };
        let rows = self.wrapped.as_ref().filter(|wrapped| !wrapped.stale);
        let gone = match (self.last.as_ref().and_then(|old| dropped(old, &new)), rows) {
            (Some(Dropped::Lines(lines)), Some(wrapped)) => {
                DroppedRows::Rows(wrapped.per_line.iter().take(lines).sum())
            }
            (Some(Dropped::Everything), _) => DroppedRows::Everything,
            (Some(Dropped::Lines(_)) | None, _) => DroppedRows::Rows(0),
        };
        self.last = Some(new);
        if let Some(wrapped) = &mut self.wrapped {
            wrapped.stale = true;
        }
        gone
    }

    pub fn last(&self) -> Option<&Read> {
        self.last.as_ref()
    }

    pub fn failure(&self) -> Option<&Cause> {
        self.failure.as_ref()
    }

    /// The last output's rows, once they have been fitted to the width of [0].
    pub fn rows(&self) -> Option<&Wrapped> {
        self.wrapped.as_ref().filter(|wrapped| !wrapped.stale)
    }

    /// Fits the last output to `width`, wrapping it with `wrap`, when [0]
    /// shows it with its `Output` heading on document row `heading`.
    pub fn fit(&mut self, width: usize, heading: usize, wrap: fn(&str, usize) -> Vec<String>) {
        let shown = self.last.as_ref().and_then(|last| last.shown.as_ref());
        match (&mut self.wrapped, shown) {
            (Some(wrapped), _) if !wrapped.stale && wrapped.width == width => {
                wrapped.heading = heading;
            }
            (_, Some(shown)) => self.wrapped = Some(Wrapped::of(shown, width, heading, wrap)),
            (_, None) => {
                self.wrapped = Some(Wrapped::of(
                    &Shown {
                        lines: Vec::new(),
                        capped: 0,
                    },
                    width,
                    heading,
                    wrap,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Model;

    fn shown(text: &str) -> Shown {
        Shown::of(text)
    }

    #[test]
    fn a_newline_only_output_is_one_blank_line() {
        let all_blank = "\n".repeat(64 * 1024);
        assert_eq!(shown(&all_blank).lines(), [""]);
    }

    #[test]
    fn runs_of_blank_lines_become_one_and_the_last_lines_are_kept() {
        assert_eq!(shown("a\n\n\n  \nb\n").lines(), ["a", "", "b"]);
        let many: String = std::iter::repeat_n("x\n", MAX_LINES + 10).collect();
        assert_eq!(shown(&many).lines().len(), MAX_LINES);
    }

    #[test]
    fn the_lines_a_newer_output_lost_at_the_front_are_counted() {
        let old = shown("a\nb\nc\nd\n");
        assert_eq!(old.dropped_from(&shown("c\nd\ne\n")), Some(2));
        assert_eq!(old.dropped_from(&shown("a\nb\nc\nd\ne\n")), Some(0));
        assert_eq!(old.dropped_from(&shown("x\ny\n")), None);
    }

    #[test]
    fn an_unfinished_last_line_does_not_hide_what_was_dropped() {
        let old = shown("a\nb\nc\npart");
        assert_eq!(old.dropped_from(&shown("b\nc\npartial line\n")), Some(1));
    }

    #[test]
    fn five_thousand_similar_lines_are_compared_in_one_pass_and_correctly() {
        let old = shown(&"x\n".repeat(5000));
        let mut newer = "x\n".repeat(4990);
        newer.push_str("y\n");
        assert_eq!(old.dropped_from(&shown(&newer)), Some(4999 - 4990));
    }

    fn read(text: &str, cut: Cut) -> Read {
        let output = CommandOutput::of(text.to_string(), cut);
        let shown = Some(Shown::of(text));
        Read {
            output,
            at: SystemTime::now(),
            shown,
        }
    }

    #[test]
    fn identical_lines_are_counted_exactly_from_where_the_window_moved() {
        let window = "x\n".repeat(5000);
        let old = read(
            &window,
            Cut::Last {
                bytes: 10_000,
                from: 0,
            },
        );
        let new = read(
            &window,
            Cut::Last {
                bytes: 10_000,
                from: 2,
            },
        );
        assert_eq!(dropped(&old, &new), Some(Dropped::Lines(1)));
        let same = read(
            &window,
            Cut::Last {
                bytes: 10_000,
                from: 0,
            },
        );
        assert_eq!(dropped(&old, &same), Some(Dropped::Lines(0)));
    }

    #[test]
    fn a_window_that_moved_over_text_that_is_not_the_same_is_taken_from_the_lines() {
        let old = read(
            "a\nb\nc\nd\n",
            Cut::Last {
                bytes: 8,
                from: 100,
            },
        );
        let rewritten = read(
            "x\ny\n",
            Cut::Last {
                bytes: 4,
                from: 104,
            },
        );
        assert_eq!(dropped(&old, &rewritten), Some(Dropped::Everything));
        let carried = read(
            "c\nd\ne\n",
            Cut::Last {
                bytes: 6,
                from: 104,
            },
        );
        assert_eq!(dropped(&old, &carried), Some(Dropped::Lines(2)));
    }

    fn one_row(line: &str, _: usize) -> Vec<String> {
        vec![line.to_string()]
    }

    fn numbered(first: usize, count: usize) -> CommandOutput {
        let mut text = (first..first + count)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        let from = (0..first).map(|n| format!("line {n}\n").len() as u64).sum();
        CommandOutput::of(text, Cut::Last { bytes: 65536, from })
    }

    fn moment() -> Moment {
        Moment {
            wall: SystemTime::now(),
            mono: Instant::now(),
        }
    }

    fn answered(watch: &mut OutputWatch, ticket: Ticket, output: CommandOutput) -> Heard {
        let moment = moment();
        watch.ask(ticket, Presence::Listed, moment.mono);
        watch.hear(ticket, Ok(output), moment).expect("counted")
    }

    #[test]
    fn an_answer_before_any_draw_knows_how_many_rows_it_took_off_the_front() {
        let mut model = crate::fixtures::fleet(10);
        let mut watch = OutputWatch::default();
        answered(&mut watch, model.ticket(), numbered(0, 100));
        watch.fit(80, 7, one_row);
        let heard = answered(&mut watch, model.ticket(), numbered(20, 100));
        assert_eq!(heard.moved.scroll(60), 40);
        assert_eq!(
            heard.moved.scroll(8 + 1 + 20),
            8 + 1,
            "the first row it kept"
        );
    }

    #[test]
    fn one_more_line_of_the_same_text_drops_exactly_one_row() {
        let mut model = crate::fixtures::fleet(10);
        let mut watch = OutputWatch::default();
        let same = |from| {
            CommandOutput::of(
                "x\n".repeat(5000),
                Cut::Last {
                    bytes: 10_000,
                    from,
                },
            )
        };
        answered(&mut watch, model.ticket(), same(0));
        watch.fit(80, 7, one_row);
        let heard = answered(&mut watch, model.ticket(), same(2));
        assert_eq!(heard.moved.scroll(100), 99);
    }

    #[test]
    fn a_scroll_two_rows_into_the_text_with_thirty_rows_dropped_stays_at_the_texts_first_row() {
        let moved = Moved {
            heading: 7,
            text: 8,
            above: 0,
            dropped: DroppedRows::Rows(30),
        };
        assert_eq!(moved.scroll(10), 8);
    }

    #[test]
    fn a_scroll_on_a_note_row_is_not_moved_by_the_rows_dropped() {
        let moved = Moved {
            heading: 7,
            text: 10,
            above: 0,
            dropped: DroppedRows::Rows(30),
        };
        assert_eq!(moved.scroll(8), 8);
        assert_eq!(moved.scroll(9), 9);
    }

    #[test]
    fn a_text_with_nothing_in_common_takes_a_paused_view_to_the_start_of_the_text() {
        let moved = Moved {
            heading: 7,
            text: 9,
            above: 1,
            dropped: DroppedRows::Everything,
        };
        assert_eq!(moved.scroll(40), 10);
        assert_eq!(moved.scroll(9), 10);
    }

    #[test]
    fn rows_that_come_or_go_between_the_heading_and_the_text_move_the_scroll_but_not_above_them() {
        let appeared = Moved {
            heading: 7,
            text: 8,
            above: 1,
            dropped: DroppedRows::Rows(0),
        };
        assert_eq!(appeared.scroll(5), 5);
        assert_eq!(appeared.scroll(7), 7);
        assert_eq!(appeared.scroll(20), 21);
        let went = Moved {
            heading: 7,
            text: 9,
            above: -1,
            dropped: DroppedRows::Rows(0),
        };
        assert_eq!(
            went.scroll(8),
            8,
            "no higher than the first row under the heading"
        );
        assert_eq!(went.scroll(20), 19);
    }

    #[test]
    fn the_note_that_the_text_is_cut_moves_the_rows_below_it_down_by_one() {
        let mut model = crate::fixtures::fleet(10);
        let mut watch = OutputWatch::default();
        let whole = CommandOutput::of("line 0\nline 1\n".into(), Cut::Whole);
        answered(&mut watch, model.ticket(), whole);
        watch.fit(80, 7, one_row);
        let heard = answered(&mut watch, model.ticket(), numbered(0, 2));
        assert_eq!(heard.moved.scroll(20), 21);
    }

    fn watch_asked_while(
        presence: Presence,
        model: &mut Model,
        start: Instant,
    ) -> (OutputWatch, Ticket) {
        let mut watch = OutputWatch::default();
        let ticket = model.ticket();
        watch.ask(ticket, presence, start);
        (watch, ticket)
    }

    #[test]
    fn a_read_asked_while_listed_and_answered_after_the_end_is_followed_by_one_more() {
        let mut model = crate::fixtures::fleet(10);
        let start = Instant::now();
        let (mut watch, t1) = watch_asked_while(Presence::Listed, &mut model, start);
        let stalled = start + OUTPUT_STALLED;
        assert!(watch.due(Presence::Ended, stalled));
        let t2 = model.ticket();
        watch.ask(t2, Presence::Ended, stalled);
        let late = Moment {
            wall: SystemTime::now(),
            mono: stalled + Duration::from_secs(5),
        };
        let output = CommandOutput::of("early\n".into(), Cut::Whole);
        assert!(watch.hear(t1, Ok(output), late).is_some());
        assert!(
            watch.due(Presence::Ended, late.mono),
            "t1 was asked while the command was listed, so what came after is still to fetch"
        );
        let t3 = model.ticket();
        watch.ask(t3, Presence::Ended, late.mono);
        let output = CommandOutput::of("final\n".into(), Cut::Whole);
        assert!(watch.hear(t3, Ok(output), late).is_some());
        assert!(!watch.due(Presence::Ended, late.mono + OUTPUT_REFRESH * 10));
    }

    #[test]
    fn a_read_for_an_ended_command_that_found_the_search_unfinished_is_asked_again_later() {
        let mut model = crate::fixtures::fleet(10);
        let start = moment();
        let (mut watch, t1) = watch_asked_while(Presence::Ended, &mut model, start.mono);
        assert!(
            watch
                .hear(t1, Ok(CommandOutput::Searching), start)
                .is_some()
        );
        assert!(!watch.due(Presence::Ended, start.mono));
        assert!(watch.due(Presence::Ended, start.mono + OUTPUT_REFRESH));
        let t2 = model.ticket();
        watch.ask(t2, Presence::Ended, start.mono);
        assert!(
            watch
                .hear(t2, Ok(CommandOutput::Unlocated), start)
                .is_some()
        );
        assert!(!watch.due(Presence::Ended, start.mono + OUTPUT_REFRESH * 10));
    }

    #[test]
    fn lines_that_left_the_front_are_counted_when_only_the_new_side_is_capped() {
        let lines = |range: std::ops::Range<usize>| {
            let mut text = range
                .map(|n| format!("l{n}"))
                .collect::<Vec<_>>()
                .join("\n");
            text.push('\n');
            text
        };
        let window = Cut::Last {
            bytes: 65536,
            from: 0,
        };
        let old = read(&lines(0..MAX_LINES), window);
        let new = read(&lines(0..MAX_LINES + 10), window);
        assert_eq!(dropped(&old, &new), Some(Dropped::Lines(10)));
    }

    #[test]
    fn the_presence_of_a_wait_is_listed_when_any_request_of_it_was_asked_listed() {
        let mut model = crate::fixtures::fleet(10);
        let start = Instant::now();
        let mut watch = OutputWatch::default();
        watch.ask(model.ticket(), Presence::Ended, start);
        let t2 = model.ticket();
        watch.ask(t2, Presence::Listed, start);
        watch.ask(model.ticket(), Presence::Ended, start);
        let moment = Moment {
            wall: SystemTime::now(),
            mono: start,
        };
        let output = CommandOutput::of("x\n".into(), Cut::Whole);
        assert!(watch.hear(t2, Ok(output), moment).is_some());
        assert!(watch.due(Presence::Ended, start), "t2 may predate the end");
    }
}
