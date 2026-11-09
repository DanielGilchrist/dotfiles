//! A subagent's log as rows: a pinned status row, then the prompt, what it
//! said and its calls, or for a finished one its report first.

use std::path::Path;
use std::time::{Duration, SystemTime};

use agent_core::activity::ToolId;
use agent_core::activity::{Subagent, SubagentStatus};
use agent_core::ports::{
    ActivityDetail, Call, CallState, Excerpt, Log, LogEntry, Outcome, Progress, Summary, Target,
};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::page::count_spans;
use super::sidebar::file_marker;
use super::{
    SPINNER, accent, ago, bold, dim, error, fit, justify, line_width, success, truncate_end,
    truncate_start, warning, wrap,
};
use crate::log::{
    Entry, EntryKey, EntryRows, Nav, Phase, activity_entries, agent_count, call_count, delegates,
    entries, error_count, failing, target_text, time_of, unrecovered_count,
};
use crate::model::Asking;
use crate::touched::{Now, Roots, TouchedFile};

/// Below this width the summary column and tool names shrink.
const WIDE: usize = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Width {
    Narrow,
    Wide,
}

impl Context<'_> {
    fn width(&self) -> Width {
        if self.columns < WIDE {
            Width::Narrow
        } else {
            Width::Wide
        }
    }
}
const SUMMARY_WIDE: usize = 32;
const SUMMARY_NARROW: usize = 10;
const SAID_ROWS: usize = 4;
/// A call that has run this long, or a subagent this quiet, may be stuck.
const STUCK: Duration = Duration::from_secs(300);
const RESULT_LINES: usize = 40;
const DURATION_FROM: u64 = 5;

pub(super) struct Context<'a> {
    pub columns: usize,
    pub now: SystemTime,
    pub spinner: &'static str,
    /// The agent's checkout, to show paths inside it relative.
    pub roots: Option<&'a Roots>,
    pub home: Option<&'a Path>,
}

pub(super) struct Rendered {
    pub pinned: Vec<Line<'static>>,
    pub rows: Vec<Line<'static>>,
    /// Each entry's rows.
    pub layout: Vec<EntryRows>,
    pub finished: bool,
}

/// `38s`, `4m12s`, `6m02s`, `1h02m`.
pub(super) fn span(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// `12`, or `12+` when the log starts after the transcript does.
fn call_total(detail: &Log) -> String {
    let n = call_count(detail);
    if detail.truncated {
        format!("{n}+")
    } else {
        n.to_string()
    }
}

/// `12 calls`, or `12+ calls` when the log starts after the transcript does.
fn calls_label(detail: &Log) -> String {
    let n = call_count(detail);
    if detail.truncated {
        format!("{n}+ calls")
    } else {
        plural(n, "call", "calls")
    }
}

/// `+9 −14`, leaving out the side that has nothing.
fn diff_spans(added: usize, removed: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if added > 0 || removed == 0 {
        spans.push(Span::styled(format!("+{added}"), success()));
    }
    if removed > 0 {
        let gap = if spans.is_empty() { "" } else { " " };
        spans.push(Span::styled(format!("{gap}−{removed}"), error()));
    }
    spans
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn lines_text(n: usize) -> String {
    if n >= 1000 {
        format!("{}.{}k lines", n / 1000, n % 1000 / 100)
    } else {
        plural(n, "line", "lines")
    }
}

fn summary_spans(summary: &Summary) -> Vec<Span<'static>> {
    let text = match summary {
        Summary::Lines(n) => lines_text(*n),
        Summary::Diff { added, removed } => return diff_spans(*added, *removed),
        Summary::NewFile { lines } => format!("new · {}", lines_text(*lines)),
        Summary::Matches(n) => plural(*n, "match", "matches"),
        Summary::Files(n) => plural(*n, "file", "files"),
        Summary::Text(text) => text.clone(),
        Summary::NoOutput => "no output".into(),
        Summary::Image => "image".into(),
    };
    vec![Span::styled(text, dim())]
}

/// The most that can be told apart from a command that keeps failing.
fn failure_text(code: Option<i32>) -> String {
    code.map_or_else(|| "failed".into(), |c| format!("exit {c}"))
}

fn unwind(path: &str, ctx: &Context<'_>) -> String {
    let path = Path::new(path);
    if let Some(rest) = ctx.roots.and_then(|roots| roots.strip(path)) {
        return rest.to_string_lossy().into_owned();
    }
    match ctx.home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.to_string_lossy().into_owned(),
    }
}

/// Printable text on one row: tabs widened, other control characters gone.
fn tame(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\t' => Some(' '),
            '\n' => Some('\n'),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

fn gutter() -> Span<'static> {
    Span::raw(" ")
}

fn indented(indent: usize, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut row = vec![gutter(), Span::raw(" ".repeat(indent))];
    row.extend(spans);
    Line::from(row)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    Over,
    /// Still going, so its last call is live.
    Going(Pace),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pace {
    Moving,
    /// It has gone quiet, so spinners stand still.
    Stalled,
}

impl Standing {
    fn of(live: bool, stale: bool) -> Self {
        match (live, stale) {
            (false, _) => Self::Over,
            (true, false) => Self::Going(Pace::Moving),
            (true, true) => Self::Going(Pace::Stalled),
        }
    }
}

/// Which running calls are live.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Concurrency {
    LastOnly,
    /// An agent can run several calls at once.
    Every,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LastRow {
    Text,
    /// A blank row never follows it.
    Rule,
}

struct Rows<'a, 'b> {
    ctx: &'a Context<'b>,
    standing: Standing,
    concurrency: Concurrency,
    /// The call the user's answer is awaited for.
    waiting: Option<ToolId>,
    /// When the hooks say its running tool started, for a call whose
    /// request carried no time.
    running_since: Option<SystemTime>,
    out: Vec<Line<'static>>,
    last_row: LastRow,
}

impl Rows<'_, '_> {
    fn push(&mut self, line: Line<'static>) {
        self.last_row = LastRow::Text;
        self.out.push(line);
    }

    fn blank(&mut self) {
        if !self.out.is_empty() && self.last_row == LastRow::Text {
            self.out.push(Line::raw(""));
        }
    }

    fn rule(&mut self, label: &str) {
        let room = self.ctx.columns.saturating_sub(1);
        let text = format!("{label} ");
        let rule = "─".repeat(room.saturating_sub(text.chars().count()));
        self.out.push(Line::from(vec![
            gutter(),
            Span::styled(text + &rule, dim()),
        ]));
        self.last_row = LastRow::Rule;
    }

    fn text_rows(&mut self, text: &str, indent: usize) {
        let width = self.ctx.columns.saturating_sub(1 + indent).max(1);
        for row in wrap(&tame(text), width) {
            self.push(indented(indent, vec![Span::raw(row)]));
        }
    }

    fn report(&mut self, report: &str, detail: &Log) {
        self.blank();
        self.push(Line::from(vec![gutter(), Span::styled("Report", bold())]));
        self.text_rows(report, 2);
        self.blank();
        self.rule(&format!("Calls · {}", call_total(detail)));
    }

    fn prompt(&mut self, prompt: &str, open: bool) {
        let prompt = tame(prompt);
        let lead = Span::styled("›", accent());
        if open {
            let width = self.ctx.columns.saturating_sub(3).max(1);
            for (i, row) in wrap(&prompt, width).into_iter().enumerate() {
                let first = if i == 0 { lead.clone() } else { Span::raw(" ") };
                self.push(Line::from(vec![
                    gutter(),
                    first,
                    Span::raw(" "),
                    Span::raw(row),
                ]));
            }
            return;
        }
        let first = prompt.lines().next().unwrap_or_default();
        let more = prompt.lines().count().saturating_sub(1);
        let room = self.ctx.columns.saturating_sub(3);
        self.push(Line::from(vec![
            gutter(),
            lead,
            Span::raw(" "),
            Span::raw(truncate_end(first, room)),
        ]));
        if more > 0 {
            self.push(indented(
                2,
                vec![Span::styled(
                    format!("+{}", plural(more, "line", "lines")),
                    dim(),
                )],
            ));
        }
    }

    fn said(&mut self, text: &str, open: bool) {
        let width = self.ctx.columns.saturating_sub(1).max(1);
        let rows = wrap(&tame(text), width);
        let shown = if open {
            rows.len()
        } else {
            SAID_ROWS.min(rows.len())
        };
        for row in rows.iter().take(shown) {
            self.push(Line::from(vec![gutter(), Span::raw(row.clone())]));
        }
        if shown < rows.len() {
            self.push(indented(
                2,
                vec![Span::styled(
                    format!("+{}", plural(rows.len() - shown, "line", "lines")),
                    dim(),
                )],
            ));
        }
    }

    fn is_live(&self, call: &Call, last: bool) -> bool {
        let going = matches!(self.standing, Standing::Going(_));
        let several = last || self.concurrency == Concurrency::Every;
        going && several && matches!(call.state, CallState::Running)
    }

    fn asks(&self, call: &Call) -> bool {
        self.waiting.as_ref() == Some(&call.id)
    }

    fn glyph(&self, call: &Call, last: bool) -> Span<'static> {
        match &call.state {
            CallState::Running if self.asks(call) => {
                Span::styled("◆", warning().add_modifier(Modifier::BOLD))
            }
            CallState::Running
                if self.is_live(call, last) && self.standing == Standing::Going(Pace::Stalled) =>
            {
                Span::styled(SPINNER[0], dim())
            }
            CallState::Running if self.is_live(call, last) => Span::raw(self.ctx.spinner),
            CallState::Running | CallState::Done { .. } => Span::styled("●", dim()),
            CallState::Failed { .. } => Span::styled("!", error().add_modifier(Modifier::BOLD)),
            CallState::Flagged { .. } => Span::styled("●", warning()),
        }
    }

    fn target_spans(&self, target: &Target, room: usize) -> Vec<Span<'static>> {
        match target {
            Target::Path { path, range } => {
                let shown = tame(&unwind(path, self.ctx));
                let suffix = target_text(&Target::Path {
                    path: String::new(),
                    range: *range,
                });
                let keep = room.saturating_sub(suffix.chars().count());
                vec![Span::raw(format!(
                    "{}{suffix}",
                    truncate_start(&shown, keep)
                ))]
            }
            Target::Search { pattern, path } => {
                let mut spans = vec![Span::raw(format!("\"{pattern}\""))];
                if let Some(path) = path {
                    spans.push(Span::styled(
                        format!(" in {}", tame(&unwind(path, self.ctx))),
                        dim(),
                    ));
                }
                spans
            }
            other => vec![Span::raw(target_text(other))],
        }
    }

    /// The right side of a call row: how it came out, and how long it took.
    fn outcome_spans(&self, call: &Call, last: bool) -> Vec<Span<'static>> {
        let column = if self.ctx.width() == Width::Narrow {
            SUMMARY_NARROW
        } else {
            SUMMARY_WIDE
        };
        let took = call
            .state
            .took()
            .filter(|d| d.as_secs() >= DURATION_FROM)
            .map(span);
        let mut spans = match &call.state {
            CallState::Running if self.asks(call) => {
                return vec![Span::styled("waiting", warning())];
            }
            CallState::Running => {
                let ran = call
                    .started
                    .or(self.running_since)
                    .and_then(|from| self.ctx.now.duration_since(from).ok());
                let mut text = ran.map(span).unwrap_or_default();
                let stuck = ran.is_some_and(|d| d >= STUCK);
                if self.ctx.width() == Width::Wide || text.is_empty() {
                    text = if text.is_empty() {
                        "running".into()
                    } else {
                        format!("running {text}")
                    };
                }
                if !self.is_live(call, last) {
                    return Vec::new();
                }
                let mut spans = vec![Span::styled(text, dim())];
                if stuck {
                    spans.push(Span::styled(" ?", warning()));
                }
                return spans;
            }
            CallState::Done { summary, .. } | CallState::Flagged { summary, .. } => {
                summary_spans(summary)
            }
            CallState::Failed { code, .. } => {
                vec![Span::styled(failure_text(*code), error())]
            }
        };
        if self.ctx.width() == Width::Narrow
            && matches!(call.state, CallState::Done { .. })
            && let Some(took) = &took
        {
            return vec![Span::styled(took.clone(), dim())];
        }
        spans = fit(spans, column);
        if let Some(took) = took {
            spans.push(Span::styled(format!("  {took}"), dim()));
        }
        spans
    }

    fn call_row(
        &mut self,
        call: &Call,
        last: bool,
        suffix: Option<Span<'static>>,
        label: Option<Vec<Span<'static>>>,
    ) {
        let name = if self.ctx.width() == Width::Narrow {
            5
        } else {
            6
        };
        let right = self.outcome_spans(call, last);
        let mut left = vec![
            gutter(),
            Span::raw("  "),
            self.glyph(call, last),
            Span::raw(" "),
            Span::raw(format!("{:<name$} ", call.tool)),
        ];
        let used = line_width(&left);
        let right_width = line_width(&right);
        let room = self
            .ctx
            .columns
            .saturating_sub(used + right_width + 2 + suffix.as_ref().map_or(0, Span::width));
        left.extend(label.unwrap_or_else(|| self.target_spans(&call.target, room)));
        let left = fit(
            left,
            self.ctx
                .columns
                .saturating_sub(right_width + 2 + suffix.as_ref().map_or(0, Span::width)),
        );
        let mut left = left;
        left.extend(suffix);
        self.push(justify(left, right, self.ctx.columns));
    }

    fn excerpt_rows(&mut self, excerpt: &Excerpt, style: ratatui::style::Style, more: &str) {
        let width = self.ctx.columns.saturating_sub(7);
        for (i, line) in excerpt.lines.iter().enumerate() {
            let lead = if i == 0 { "    ⎿ " } else { "      " };
            self.push(Line::from(vec![
                gutter(),
                Span::styled(format!("{lead}{}", truncate_end(&tame(line), width)), style),
            ]));
        }
        if excerpt.more > 0 {
            self.push(indented(
                6,
                vec![Span::styled(format!("+{} {more}", excerpt.more), dim())],
            ));
        }
    }

    fn detail_rows(&mut self, call: &Call) {
        let width = self.ctx.columns.saturating_sub(6).max(1);
        for row in wrap(&tame(&call.input), width) {
            self.push(indented(4, vec![Span::raw(row)]));
        }
        let result: Vec<String> = wrap(
            &tame(call.output.trim_end()),
            width.saturating_sub(2).max(1),
        );
        for (i, row) in result.iter().take(RESULT_LINES).enumerate() {
            let lead = if i == 0 { "⎿ " } else { "  " };
            self.push(indented(
                4,
                vec![Span::styled(format!("{lead}{row}"), dim())],
            ));
        }
        if result.len() > RESULT_LINES {
            self.push(indented(
                6,
                vec![Span::styled(
                    format!("+{}", plural(result.len() - RESULT_LINES, "line", "lines")),
                    dim(),
                )],
            ));
        }
    }

    fn call(&mut self, call: &Call, last: bool, suffix: Option<Span<'static>>, open: bool) {
        self.call_row(call, last, suffix, None);
        if open {
            self.detail_rows(call);
            return;
        }
        match &call.state {
            CallState::Failed { excerpt, .. } => self.excerpt_rows(excerpt, error(), "lines"),
            CallState::Flagged { excerpt, .. } => self.excerpt_rows(excerpt, warning(), "more"),
            CallState::Running | CallState::Done { .. } => {}
        }
    }

    fn group(&mut self, tool: &str, calls: &[&Call]) {
        let noun = match calls.first().map(|c| &c.target) {
            Some(Target::Path { .. }) => "files",
            Some(Target::Search { .. }) => "searches",
            Some(Target::Pattern(_)) => "globs",
            _ => "calls",
        };
        let names: Vec<String> = calls
            .iter()
            .map(|c| match &c.target {
                Target::Path { path, .. } => path.rsplit('/').next().unwrap_or(path).to_string(),
                Target::Search { pattern, .. } | Target::Pattern(pattern) => pattern.clone(),
                other => target_text(other),
            })
            .collect();
        let (mut lines, mut matches, mut files) = (0, 0, 0);
        for call in calls {
            match &call.state {
                CallState::Done {
                    summary: Summary::Lines(n),
                    ..
                } => lines += n,
                CallState::Done {
                    summary: Summary::Matches(n),
                    ..
                } => matches += n,
                CallState::Done {
                    summary: Summary::Files(n),
                    ..
                } => files += n,
                _ => {}
            }
        }
        let total = if lines > 0 {
            lines_text(lines)
        } else if matches > 0 {
            plural(matches, "match", "matches")
        } else if files > 0 {
            plural(files, "file", "files")
        } else {
            String::new()
        };
        let name = if self.ctx.width() == Width::Narrow {
            5
        } else {
            6
        };
        let head = format!("{:<name$} {} {noun}  ", tool, calls.len());
        let used = 3 + 2 + head.chars().count() + total.chars().count() + 2;
        let room = self.ctx.columns.saturating_sub(used);
        let shown = names_fit(&names, room);
        self.push(justify(
            vec![
                gutter(),
                Span::raw("  "),
                Span::styled("●", dim()),
                Span::raw(" "),
                Span::raw(head),
                Span::styled(shown, dim()),
            ],
            vec![Span::styled(total, dim())],
            self.ctx.columns,
        ));
    }
}

/// `a · b · c +1`, as many as fit in `room`.
fn names_fit(names: &[String], room: usize) -> String {
    let mut out = String::new();
    for (at, name) in names.iter().enumerate() {
        let left = names.len() - at - 1;
        let more = if left > 0 {
            format!(" +{left}").len()
        } else {
            0
        };
        let sep = if out.is_empty() { 0 } else { 3 };
        if out.chars().count() + sep + name.chars().count() + more > room {
            let hidden = names.len() - at;
            if !out.is_empty() {
                out.push_str(" +");
                out.push_str(&hidden.to_string());
            }
            return out;
        }
        if sep > 0 {
            out.push_str(" · ");
        }
        out.push_str(name);
    }
    out
}

/// What the status row quotes about a subagent, before it picks a state.
struct Figures {
    calls: Span<'static>,
    errors: Option<Span<'static>>,
    model: Option<Span<'static>>,
    working: Option<String>,
    took: Option<String>,
}

fn status_row(
    detail: &Log,
    subagent: Option<&Subagent>,
    ctx: &Context<'_>,
    standing: Standing,
) -> Vec<Span<'static>> {
    let figures = figures(detail, subagent, ctx);
    let over = standing == Standing::Over;
    match &detail.outcome {
        Outcome::Finished { report: Some(_) } if over => finished_row(figures),
        Outcome::Stopped { reason } => stopped_row(reason),
        _ if over => ended_row(figures),
        _ => match failing(detail) {
            Some(reason) => failing_row(&reason, figures, ctx),
            None => working_row(detail, figures, ctx, standing),
        },
    }
}

fn figures(detail: &Log, subagent: Option<&Subagent>, ctx: &Context<'_>) -> Figures {
    let calls = Span::styled(calls_label(detail), dim());
    let errors = error_count(detail);
    let began = subagent.map(|s| s.since).or(detail.started);
    let ended = match subagent.map(|s| &s.status) {
        Some(SubagentStatus::Finished { at }) => Some(*at),
        Some(SubagentStatus::Running { .. }) | None => detail.last_heard,
    };
    Figures {
        calls,
        errors: (errors > 0).then(|| Span::styled(plural(errors, "error", "errors"), error())),
        model: detail
            .model
            .clone()
            .filter(|_| ctx.width() == Width::Wide)
            .map(|m| Span::styled(m, dim())),
        working: began
            .and_then(|from| ctx.now.duration_since(from).ok())
            .map(span),
        took: began
            .zip(ended)
            .and_then(|(from, to)| to.duration_since(from).ok())
            .map(span),
    }
}

/// `spans` followed by each extra that is there, after a separator.
fn with_tail(
    mut spans: Vec<Span<'static>>,
    extras: Vec<Option<Span<'static>>>,
) -> Vec<Span<'static>> {
    for extra in extras.into_iter().flatten() {
        spans.push(Span::styled(" · ", dim()));
        spans.push(extra);
    }
    spans
}

/// ` in 3m` or ` after 3m`, nothing when it's unknown how long.
fn elapsed(took: Option<String>, preposition: &str) -> String {
    took.map(|t| format!(" {preposition} {t}"))
        .unwrap_or_default()
}

fn finished_row(figures: Figures) -> Vec<Span<'static>> {
    let spans = vec![
        Span::styled("✓", success().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" finished{}", elapsed(figures.took, "in"))),
    ];
    with_tail(
        spans,
        vec![Some(figures.calls), figures.errors, figures.model],
    )
}

fn stopped_row(reason: &str) -> Vec<Span<'static>> {
    let spans = vec![
        Span::styled("!", error().add_modifier(Modifier::BOLD)),
        Span::raw(" stopped"),
    ];
    with_tail(spans, vec![Some(Span::raw(reason.to_string()))])
}

fn ended_row(figures: Figures) -> Vec<Span<'static>> {
    let spans = vec![
        Span::styled("●", dim()),
        Span::styled(format!(" ended{}", elapsed(figures.took, "after")), dim()),
    ];
    with_tail(
        spans,
        vec![
            Some(Span::styled("no report", dim())),
            Some(figures.calls),
            figures.errors,
        ],
    )
}

fn failing_row(reason: &str, figures: Figures, ctx: &Context<'_>) -> Vec<Span<'static>> {
    let spans = vec![
        Span::styled("!", error().add_modifier(Modifier::BOLD)),
        Span::styled(" failing", bold()),
        Span::styled(" · ", dim()),
        Span::raw(reason.to_string()),
    ];
    if ctx.width() == Width::Narrow {
        return spans;
    }
    with_tail(
        spans,
        vec![
            figures
                .working
                .map(|w| Span::styled(format!("working {w}"), dim())),
            Some(figures.calls),
            figures.model,
        ],
    )
}

fn working_row(
    detail: &Log,
    figures: Figures,
    ctx: &Context<'_>,
    standing: Standing,
) -> Vec<Span<'static>> {
    let stalled = standing == Standing::Going(Pace::Stalled);
    let glyph = spinner_span(standing, ctx);
    let spans = vec![
        glyph,
        Span::raw(format!(
            " working{}",
            figures.working.map(|w| format!(" {w}")).unwrap_or_default()
        )),
    ];
    let quiet = stalled
        .then(|| {
            detail
                .last_heard
                .and_then(|at| ctx.now.duration_since(at).ok())
        })
        .flatten()
        .map(|d| Span::styled(format!("quiet {} ?", ago(ctx.now - d, ctx.now)), dim()));
    with_tail(
        spans,
        vec![quiet, Some(figures.calls), figures.errors, figures.model],
    )
}

/// What kind of activity [0] is showing, with what the status row needs of the
/// agent's own state.
pub(super) enum ActivityStage<'a> {
    Working {
        since: SystemTime,
        /// Quiet long enough that the spinner should stand still.
        stale: bool,
        /// Subagents running.
        running: usize,
    },
    Asking {
        since: SystemTime,
        /// What the harness says it was asked.
        question: &'a str,
        /// What the question is about, when it could be placed.
        asking: Option<&'a Asking>,
        /// Who is asking, when it isn't the agent itself.
        by: Option<String>,
    },
    Delegating {
        running: usize,
        quiet: Duration,
        stale: bool,
    },
    Done {
        /// Whether the user has already looked at it.
        seen: bool,
    },
    Failed {
        reason: &'a str,
        since: SystemTime,
    },
    Interrupted {
        since: SystemTime,
    },
}

impl ActivityStage<'_> {
    fn finished(&self) -> bool {
        matches!(
            self,
            Self::Done { .. } | Self::Failed { .. } | Self::Interrupted { .. }
        )
    }
}

pub(super) struct ActivityView<'a> {
    pub state: ActivityStage<'a>,
    /// What the agent ended the activity with, shown on top of its activity.
    pub closing: Option<&'a str>,
    /// The last moment the user had the activity in front of them.
    pub looked: Option<SystemTime>,
    /// The panel's height, which the question may take half of.
    pub height: usize,
    /// The files the activity touched, which the status row counts.
    pub touched: &'a [TouchedFile],
    /// The ones it lists as rows.
    pub listed: &'a [TouchedFile],
}

impl Rows<'_, '_> {
    /// Rows for one entry, and where they are. `last` says it is the log's
    /// last, which a running call there is live in.
    fn entry(&mut self, entry: &Entry<'_>, last: bool, open: bool) -> (usize, usize) {
        let started = self.out.len();
        match entry {
            Entry::Prompt(prompt) => self.prompt(prompt, open),
            Entry::Said(text) => {
                self.blank();
                let started = self.out.len();
                self.said(text, open);
                return (started, self.out.len() - started);
            }
            Entry::Queued(text) => {
                self.blank();
                let started = self.out.len();
                self.queued(text);
                return (started, self.out.len() - started);
            }
            Entry::Compacted => {
                self.blank();
                let started = self.out.len();
                self.push(Line::from(vec![gutter(), Span::styled("compacted", dim())]));
                return (started, 1);
            }
            Entry::Call(call) => self.call(call, last, None, open),
            Entry::Group { tool, calls } => {
                if open {
                    for call in calls {
                        self.call_row(call, false, None, None);
                    }
                } else {
                    self.group(tool, calls);
                }
            }
            Entry::Repeat(calls) => {
                if open {
                    for call in calls {
                        self.call(call, false, None, false);
                    }
                } else if let Some(call) = calls.last() {
                    let times = Span::styled(format!("  ×{}", calls.len()), dim());
                    self.call(call, last, Some(times), false);
                }
            }
            Entry::Phase(phase) if open => {
                for inner in &phase.entries {
                    self.entry(inner, false, matches!(inner, Entry::Said(_)));
                }
            }
            Entry::Phase(phase) => self.phase(phase),
            Entry::Touched(file) => self.touched(file),
        }
        (started, self.out.len() - started)
    }

    /// A touched file as [3] shows it: its marker and path, then the counts.
    fn touched(&mut self, file: &TouchedFile) {
        let mut left = vec![gutter(), Span::raw("  ")];
        let right = match &file.now {
            Now::Changed(change) => {
                left.extend(file_marker(change.state()));
                count_spans(change.kind)
            }
            Now::Clean => {
                left.push(Span::raw("   "));
                vec![Span::styled("no difference", dim())]
            }
            Now::Unknown => {
                left.push(Span::raw("   "));
                Vec::new()
            }
        };
        let used = line_width(&left) + line_width(&right) + 2;
        let path = unwind(&file.path.to_string_lossy(), self.ctx);
        left.push(Span::raw(truncate_start(
            &tame(&path),
            self.ctx.columns.saturating_sub(used),
        )));
        self.push(justify(left, right, self.ctx.columns));
    }

    /// The rows of the entry `key` names, as the layout remembers them.
    fn keyed(&mut self, key: EntryKey, entry: &Entry<'_>, last: bool, nav: &Nav) -> EntryRows {
        let (start, len) = self.entry(entry, last, nav.is_open(&key));
        EntryRows { key, start, len }
    }

    fn queued(&mut self, text: &str) {
        let width = self.ctx.columns.saturating_sub(3).max(1);
        for (i, row) in wrap(&tame(text), width).into_iter().enumerate() {
            let lead = if i == 0 {
                Span::styled("›", accent())
            } else {
                Span::raw(" ")
            };
            let first = if i == 0 { "queued: " } else { "        " };
            self.push(Line::from(vec![
                gutter(),
                lead,
                Span::raw(" "),
                Span::styled(format!("{first}{row}"), dim()),
            ]));
        }
    }

    /// A finished phase on one row: what it started with, how many calls it
    /// took and the one thing about it worth knowing.
    fn phase(&mut self, phase: &Phase<'_>) {
        let calls: Vec<&Call> = phase.entries.iter().flat_map(Entry::calls).collect();
        let heading = phase.heading().map(|h| tame(&h)).unwrap_or_default();
        let mut right = vec![Span::styled(plural(calls.len(), "call", "calls"), dim())];
        if let Some(note) = phase_note(&calls, phase.took) {
            right.push(Span::raw("  "));
            right.extend(note);
        }
        let room = self.ctx.columns.saturating_sub(line_width(&right) + 5);
        self.push(justify(
            vec![
                gutter(),
                Span::styled("▸", dim()),
                Span::raw(" "),
                Span::raw(truncate_end(heading.trim(), room)),
            ],
            right,
            self.ctx.columns,
        ));
    }
}

/// What stands out about a folded phase, most urgent first.
fn phase_note(calls: &[&Call], took: Option<Duration>) -> Option<Vec<Span<'static>>> {
    let errors = calls
        .iter()
        .filter(|c| matches!(c.state, CallState::Failed { .. }))
        .count();
    if errors > 0 {
        return Some(vec![Span::styled(
            plural(errors, "error", "errors"),
            error(),
        )]);
    }
    let (mut added, mut removed) = (0, 0);
    for call in calls {
        if let CallState::Done {
            summary:
                Summary::Diff {
                    added: a,
                    removed: r,
                },
            ..
        } = &call.state
        {
            added += a;
            removed += r;
        }
    }
    if added + removed > 0 {
        return Some(diff_spans(added, removed));
    }
    let agents = delegates(calls);
    if agents > 0 {
        return Some(vec![Span::styled(format!("↳{agents}"), dim())]);
    }
    took.filter(|d| d.as_secs() >= 60)
        .map(|d| vec![Span::styled(span(d), dim())])
}

pub(super) fn render(
    detail: &Log,
    subagent: Option<&Subagent>,
    nav: &Nav,
    ctx: &Context<'_>,
) -> Rendered {
    let finished = matches!(detail.outcome, Outcome::Finished { .. })
        || subagent.is_some_and(|s| !s.is_running());
    let live = !finished;
    let idle = subagent.is_none_or(|s| s.tools().is_empty())
        && !detail
            .log
            .iter()
            .any(|e| matches!(e, agent_core::ports::LogEntry::Called(c) if matches!(c.state, CallState::Running)));
    let silent = detail
        .last_heard
        .or(subagent.map(|s| s.since))
        .and_then(|at| ctx.now.duration_since(at).ok())
        .is_some_and(|d| d >= STUCK);
    let stale = live && idle && silent;
    let standing = Standing::of(live, stale);
    let status = status_row(detail, subagent, ctx, standing);
    let pinned = status_rows(status, ctx.columns);

    let mut rows = Rows {
        ctx,
        standing,
        concurrency: Concurrency::LastOnly,
        waiting: None,
        running_since: subagent.and_then(|s| s.tools().last()).map(|t| t.since),
        out: Vec::new(),
        last_row: LastRow::Text,
    };
    let mut layout = Vec::new();
    let all = entries(detail);
    let last = all.len().saturating_sub(1);
    let report = match &detail.outcome {
        Outcome::Finished { report } => report.as_deref().filter(|_| finished),
        Outcome::Running | Outcome::Stopped { .. } => None,
    };
    let has_prompt = matches!(all.first(), Some((Entry::Prompt(_), _)));
    if let (false, Some(report)) = (has_prompt, report) {
        rows.report(report, detail);
    }
    for (at, (entry, key)) in all.into_iter().enumerate() {
        layout.push(rows.keyed(key, &entry, at == last, nav));
        if let (0, true, Some(report)) = (at, has_prompt, report) {
            rows.report(report, detail);
        }
    }
    Rendered {
        pinned,
        rows: rows.out,
        layout,
        finished,
    }
}

/// `text` without the `**` and `__` Markdown emphasises with, which a
/// terminal would show as they are.
fn strip_emphasis(text: &str) -> String {
    text.replace("**", "").replace("__", "")
}

/// The status row and the rule under it, which stay on top.
fn status_rows(status: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    vec![
        Line::from(
            std::iter::once(gutter())
                .chain(fit(status, width.saturating_sub(1)))
                .collect::<Vec<_>>(),
        ),
        rule_row(width),
    ]
}

fn rule_row(width: usize) -> Line<'static> {
    Line::from(vec![
        gutter(),
        Span::styled("─".repeat(width.saturating_sub(1)), dim()),
    ])
}

/// The main agent's current activity: a status row (and the question while it
/// asks) on top, then the prompt, the agent's closing message once it's
/// done, and the log with its older phases folded.
pub(super) fn render_activity(
    detail: &ActivityDetail,
    view: &ActivityView<'_>,
    nav: &Nav,
    ctx: &Context<'_>,
) -> Rendered {
    let finished = view.state.finished();
    let standing = Standing::of(
        !finished,
        matches!(
            view.state,
            ActivityStage::Working { stale: true, .. }
                | ActivityStage::Delegating { stale: true, .. }
        ),
    );
    let status = activity_status(detail, view, ctx, standing);
    let mut pinned = status_rows(status, ctx.columns);
    let mut rows = Rows {
        ctx,
        standing,
        concurrency: Concurrency::Every,
        waiting: match &view.state {
            ActivityStage::Asking {
                asking: Some(Asking::Choice { call, .. } | Asking::Permission { call }),
                ..
            } => Some(call.clone()),
            _ => None,
        },
        running_since: None,
        out: Vec::new(),
        last_row: LastRow::Text,
    };
    pin_question(&mut pinned, detail, view, &rows);
    let layout = activity_rows(&mut rows, detail, view, nav);
    Rendered {
        pinned,
        rows: rows.out,
        layout,
        finished,
    }
}

/// The whole question an agent asks, pinned under its status row.
fn pin_question(
    pinned: &mut Vec<Line<'static>>,
    detail: &ActivityDetail,
    view: &ActivityView<'_>,
    rows: &Rows<'_, '_>,
) {
    if let ActivityStage::Asking {
        question,
        asking,
        by,
        ..
    } = &view.state
    {
        let block = question_rows(
            detail,
            &Pinned {
                question,
                asking: *asking,
                by: by.as_deref(),
            },
            (view.height * 3 / 4).saturating_sub(3),
            rows,
        );
        pinned.splice(1..1, std::iter::once(Line::raw("")).chain(block));
    }
}

/// The index of the first entry of an activity's calls the user hasn't seen
/// since they `looked`, which is never the first.
fn unseen_from(
    detail: &ActivityDetail,
    calls: &[(Entry<'_>, usize)],
    looked: Option<SystemTime>,
) -> Option<usize> {
    let looked = looked?;
    calls
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, (_, from))| {
            detail
                .log
                .log
                .get(*from)
                .and_then(time_of)
                .is_some_and(|at| at > looked)
        })
        .map(|(index, _)| index)
}

impl Rows<'_, '_> {
    /// The files an activity touched under their heading, a row each.
    fn touched_block(&mut self, files: &[TouchedFile], nav: &Nav) -> Vec<EntryRows> {
        let mut layout = Vec::new();
        if files.is_empty() {
            return layout;
        }
        self.blank();
        self.push(Line::from(vec![
            gutter(),
            Span::styled("Touched files", bold()),
        ]));
        for file in files {
            let key = EntryKey::Touched(file.path.clone());
            layout.push(self.keyed(key, &Entry::Touched(file), false, nav));
        }
        layout
    }

    /// The rule that says where what the user hasn't seen begins.
    fn since_rule(&mut self, looked: SystemTime) {
        let ago = self.ctx.now.duration_since(looked).map(span);
        self.blank();
        self.rule(&format!("since you looked · {}", ago.unwrap_or_default()));
    }
}

/// An activity's scrolling rows in the order of its entries: the prompt, the
/// closing message, the files touched, then the calls.
fn activity_rows(
    rows: &mut Rows<'_, '_>,
    detail: &ActivityDetail,
    view: &ActivityView<'_>,
    nav: &Nav,
) -> Vec<EntryRows> {
    let all = activity_entries(&detail.log, view.closing, view.listed);
    let mut layout = Vec::new();
    if let Some(prompt) = all.prompt {
        layout.push(rows.keyed(EntryKey::Prompt, &Entry::Prompt(prompt), false, nav));
    }
    if let Some(text) = view.closing {
        rows.blank();
        rows.text_rows(&strip_emphasis(text), 0);
    }
    layout.extend(rows.touched_block(all.touched, nav));
    if view.closing.is_some() || !all.touched.is_empty() {
        rows.blank();
        rows.rule(&format!("Calls · {}", call_total(&detail.log)));
    }
    let unseen = unseen_from(detail, &all.activity, view.looked);
    let last = all.activity.len().saturating_sub(1);
    for (index, (entry, from)) in all.activity.iter().enumerate() {
        if let (Some(at), Some(looked)) = (unseen, view.looked)
            && at == index
        {
            rows.since_rule(looked);
        }
        layout.push(rows.keyed(EntryKey::Log(*from), entry, index == last, nav));
    }
    layout
}

/// What the status row quotes about an activity, before it picks a state.
struct ActivityFigures<'a> {
    log: &'a Log,
    calls: Span<'static>,
    errors: usize,
    files: usize,
    agents: usize,
    /// Since the prompt, or since the agent began when that's unknown.
    working: Option<String>,
    ctx: &'a Context<'a>,
}

impl ActivityFigures<'_> {
    /// `1 error`, which says whether it was recovered from once the activity
    /// is done.
    fn errors(&self, recovered: Option<bool>) -> Option<Span<'static>> {
        let n = self.errors;
        (n > 0).then(|| {
            let mut text = plural(n, "error", "errors");
            let recovered = recovered == Some(true);
            if recovered {
                text.push_str(if n == 1 {
                    ", recovered"
                } else {
                    ", all recovered"
                });
            }
            Span::styled(text, if recovered { dim() } else { error() })
        })
    }

    fn files(&self) -> Option<Span<'static>> {
        (self.files > 0 && self.ctx.width() == Width::Wide)
            .then(|| Span::styled(plural(self.files, "file", "files"), dim()))
    }

    fn subagents(&self) -> Option<Span<'static>> {
        (self.agents > 0).then(|| Span::styled(plural(self.agents, "subagent", "subagents"), dim()))
    }

    fn quiet(&self, standing: Standing) -> Option<Span<'static>> {
        let stalled = standing == Standing::Going(Pace::Stalled);
        let heard = stalled.then_some(self.log.last_heard).flatten()?;
        let quiet = self.ctx.now.duration_since(heard).ok()?;
        Some(Span::styled(
            format!("quiet {} ?", ago(self.ctx.now - quiet, self.ctx.now)),
            dim(),
        ))
    }
}

fn spinner_span(standing: Standing, ctx: &Context<'_>) -> Span<'static> {
    match standing {
        Standing::Going(Pace::Stalled) => Span::styled(SPINNER[0], dim()),
        Standing::Going(Pace::Moving) | Standing::Over => Span::raw(ctx.spinner),
    }
}

/// How the activity reads on the status row.
fn activity_status(
    detail: &ActivityDetail,
    view: &ActivityView<'_>,
    ctx: &Context<'_>,
    standing: Standing,
) -> Vec<Span<'static>> {
    let log = &detail.log;
    let since = match view.state {
        ActivityStage::Working { since, .. } => Some(since),
        _ => None,
    };
    let figures = ActivityFigures {
        log,
        calls: Span::styled(calls_label(log), dim()),
        errors: error_count(log),
        files: view.touched.len(),
        agents: agent_count(log),
        working: since
            .and_then(|from| ctx.now.duration_since(from).ok())
            .map(span),
        ctx,
    };
    match &view.state {
        ActivityStage::Working { running, .. } => {
            working_activity_row(&figures, standing, *running)
        }
        ActivityStage::Asking {
            since,
            question,
            asking,
            by,
        } => asking_activity_row(&figures, *since, question, *asking, by.as_deref()),
        ActivityStage::Delegating { running, quiet, .. } => {
            delegating_activity_row(&figures, standing, *running, *quiet)
        }
        ActivityStage::Done { seen } => done_activity_row(detail, &figures, *seen),
        ActivityStage::Failed { reason, since } => failed_activity_row(&figures, reason, *since),
        ActivityStage::Interrupted { since } => interrupted_activity_row(&figures, *since),
    }
}

fn working_activity_row(
    figures: &ActivityFigures<'_>,
    standing: Standing,
    running: usize,
) -> Vec<Span<'static>> {
    let narrow = figures.ctx.width() == Width::Narrow;
    if let Some(reason) = failing(figures.log) {
        let spans = vec![
            Span::styled("!", error().add_modifier(Modifier::BOLD)),
            Span::styled(" failing", bold()),
            Span::styled(" · ", dim()),
            Span::raw(reason),
        ];
        if narrow {
            return spans;
        }
        let working = figures
            .working
            .as_ref()
            .map(|w| Span::styled(format!("working {w}"), dim()));
        return with_tail(spans, vec![working, Some(figures.calls.clone())]);
    }
    let spans = vec![
        spinner_span(standing, figures.ctx),
        Span::raw(format!(
            " working{}",
            figures
                .working
                .as_ref()
                .map(|w| format!(" {w}"))
                .unwrap_or_default()
        )),
    ];
    with_tail(
        spans,
        vec![
            figures.quiet(standing),
            Some(figures.calls.clone()),
            figures.errors(None),
            figures.files(),
            (running > 0 && !narrow).then(|| Span::styled(format!("↳{running}"), dim())),
        ],
    )
}

fn asking_activity_row(
    figures: &ActivityFigures<'_>,
    since: SystemTime,
    question: &str,
    asking: Option<&Asking>,
    by: Option<&str>,
) -> Vec<Span<'static>> {
    let ctx = figures.ctx;
    let mut spans = vec![
        Span::styled("◆", warning().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" asking {}", ago(since, ctx.now))),
        Span::styled(" · ", dim()),
    ];
    let first = |text: &str| tame(text.lines().next().unwrap_or_default().trim());
    let about = match (asking, by) {
        (Some(Asking::Choice { questions, .. }), _) => questions
            .first()
            .map(|q| first(&q.header))
            .filter(|header| !header.is_empty())
            .unwrap_or_else(|| "question".to_string()),
        (_, Some(by)) => format!("↳ {by}"),
        _ => first(question),
    };
    let width = ctx.columns;
    let room = width.saturating_sub(line_width(&spans) + 1);
    if figures.ctx.width() == Width::Wide && room > about.chars().count() + ANSWER_HINT.len() + 3 {
        let used = line_width(&spans) + about.chars().count();
        let gap = " ".repeat(width.saturating_sub(used + ANSWER_HINT.len() + 1));
        spans.extend([
            Span::raw(about),
            Span::raw(gap),
            Span::styled(ANSWER_HINT, dim()),
        ]);
    } else {
        spans.push(Span::raw(truncate_end(&about, room)));
    }
    spans
}

const ANSWER_HINT: &str = "i answer in the terminal";

fn delegating_activity_row(
    figures: &ActivityFigures<'_>,
    standing: Standing,
    running: usize,
    quiet: Duration,
) -> Vec<Span<'static>> {
    let stalled = standing == Standing::Going(Pace::Stalled);
    let quiet = (quiet >= QUIET_FROM).then(|| {
        Span::styled(
            format!("quiet {}{}", span(quiet), if stalled { " ?" } else { "" }),
            dim(),
        )
    });
    with_tail(
        vec![
            spinner_span(standing, figures.ctx),
            Span::raw(" delegating"),
        ],
        vec![Some(Span::styled(format!("↳{running}"), dim())), quiet],
    )
}

fn done_activity_row(
    detail: &ActivityDetail,
    figures: &ActivityFigures<'_>,
    seen: bool,
) -> Vec<Span<'static>> {
    let log = figures.log;
    let took = match detail.progress {
        Progress::Finished { took } => took.map(span),
        Progress::Running { .. } | Progress::Stopped => None,
    };
    let glyph = if seen {
        Span::styled("●", dim())
    } else {
        Span::styled("✓", success().add_modifier(Modifier::BOLD))
    };
    let narrow = figures.ctx.width() == Width::Narrow;
    let preposition = if narrow { "" } else { " in" };
    let spans = vec![
        glyph,
        Span::raw(format!(
            " finished{}",
            took.map(|t| format!("{preposition} {t}"))
                .unwrap_or_default()
        )),
    ];
    if narrow {
        return with_tail(spans, vec![Some(figures.calls.clone())]);
    }
    with_tail(
        spans,
        vec![
            Some(figures.calls.clone()),
            figures.errors(Some(unrecovered_count(log) == 0)),
            figures.files(),
            figures.subagents(),
        ],
    )
}

fn failed_activity_row(
    figures: &ActivityFigures<'_>,
    reason: &str,
    since: SystemTime,
) -> Vec<Span<'static>> {
    let spans = vec![
        Span::styled("!", error().add_modifier(Modifier::BOLD)),
        Span::raw(" failed"),
        Span::styled(" · ", dim()),
        Span::raw(tame(reason)),
    ];
    let mut extras = vec![Some(Span::styled(ago(since, figures.ctx.now), dim()))];
    if figures.ctx.width() == Width::Wide {
        extras.extend([Some(figures.calls.clone()), figures.errors(None)]);
    }
    with_tail(spans, extras)
}

fn interrupted_activity_row(
    figures: &ActivityFigures<'_>,
    since: SystemTime,
) -> Vec<Span<'static>> {
    let spans = vec![Span::styled("■", dim()), Span::raw(" interrupted")];
    let mut extras = vec![Some(Span::styled(ago(since, figures.ctx.now), dim()))];
    if figures.ctx.width() == Width::Wide {
        extras.extend([Some(figures.calls.clone()), figures.errors(None)]);
    }
    with_tail(spans, extras)
}

/// A quiet delegating activity says so from this long.
const QUIET_FROM: Duration = Duration::from_secs(30);

/// What the pinned question block draws.
struct Pinned<'a> {
    question: &'a str,
    asking: Option<&'a Asking>,
    by: Option<&'a str>,
}

/// The question the agent is waiting on, pinned under the status row: a
/// choice's questions with their options, a permission prompt's call with
/// what the agent said before it, or a subagent's question. At most
/// `budget` rows.
fn question_rows(
    detail: &ActivityDetail,
    pinned: &Pinned<'_>,
    budget: usize,
    rows: &Rows<'_, '_>,
) -> Vec<Line<'static>> {
    let budget = budget.max(6);
    let width = rows.ctx.columns.saturating_sub(1).max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    match pinned.asking {
        Some(Asking::Choice { questions, .. }) => {
            for (at, question) in questions.iter().enumerate() {
                if at > 0 {
                    out.push(Line::raw(""));
                }
                choice_rows(question, rows, &mut out);
            }
        }
        Some(Asking::Permission { call }) => permission_rows(detail, call, rows, &mut out),
        Some(Asking::Subagent(_)) | None => {
            if let Some(by) = pinned.by {
                out.push(Line::from(vec![
                    gutter(),
                    Span::styled(format!("↳ {by} asks"), dim()),
                ]));
            }
            out.extend(
                wrap(&tame(pinned.question), width)
                    .into_iter()
                    .take(PERMISSION_ROWS)
                    .map(plain_row),
            );
        }
    }
    if out.len() > budget {
        let hidden = out.len() - (budget - 1);
        out.truncate(budget - 1);
        out.push(Line::from(vec![
            gutter(),
            Span::styled(
                format!("+{hidden} more rows · i answer in the terminal"),
                dim(),
            ),
        ]));
    }
    out
}

fn plain_row(text: String) -> Line<'static> {
    Line::from(vec![gutter(), Span::raw(text)])
}

/// Where the numbers, labels and descriptions of a question's options sit.
struct Columns {
    /// Columns before a label: the gutter, the number and its gap.
    lead: usize,
    label: usize,
    /// Room for a description beside its label, or `None` when it goes
    /// under it.
    beside: Option<usize>,
    width: usize,
}

impl Columns {
    fn of(question: &agent_core::ports::Question, rows: &Rows<'_, '_>) -> Self {
        let lead = 4;
        let label = question
            .options
            .iter()
            .map(|o| o.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(OPTION_LABEL);
        let room = rows.ctx.columns.saturating_sub(lead + label + 3);
        Self {
            lead,
            label,
            beside: (rows.ctx.width() == Width::Wide && room >= OPTION_BESIDE).then_some(room),
            width: rows.ctx.columns,
        }
    }
}

/// The question's text, then its options as numbered rows, then the
/// `Other` the harness always adds.
fn choice_rows(
    question: &agent_core::ports::Question,
    rows: &Rows<'_, '_>,
    out: &mut Vec<Line<'static>>,
) {
    let width = rows.ctx.columns.saturating_sub(1).max(1);
    out.extend(
        wrap(&tame(&question.text), width)
            .into_iter()
            .map(plain_row),
    );
    if question.multi {
        out.push(Line::from(vec![
            gutter(),
            Span::styled("pick any number", dim()),
        ]));
    }
    out.push(Line::raw(""));
    let columns = Columns::of(question, rows);
    for (at, option) in question.options.iter().enumerate() {
        option_rows(at + 1, &option.label, &option.description, &columns, out);
    }
    option_rows(
        question.options.len() + 1,
        "Other",
        "type an answer",
        &columns,
        out,
    );
}

/// One option: `1  label   description`, the description wrapped beside the
/// label or, in a narrow panel, under it and cut to two rows.
fn option_rows(
    number: usize,
    label: &str,
    description: &str,
    columns: &Columns,
    out: &mut Vec<Line<'static>>,
) {
    let Columns {
        lead,
        label: label_width,
        beside,
        width,
    } = *columns;
    let number = Span::styled(number.to_string(), bold());
    let label = truncate_end(&tame(label), label_width.max(OTHER.len()).min(width));
    let Some(room) = beside else {
        out.push(Line::from(vec![
            gutter(),
            number,
            Span::raw("  "),
            Span::raw(truncate_end(&label, width.saturating_sub(lead))),
        ]));
        let rows = wrap(&tame(description), width.saturating_sub(lead).max(1));
        for (at, row) in rows.iter().take(NARROW_DESCRIPTION).enumerate() {
            let cut = at + 1 == NARROW_DESCRIPTION && rows.len() > NARROW_DESCRIPTION;
            let row = if cut {
                let kept: String = row.chars().take(width.saturating_sub(lead + 1)).collect();
                format!("{kept}…")
            } else {
                row.clone()
            };
            out.push(Line::from(vec![
                Span::raw(" ".repeat(lead)),
                Span::styled(row, dim()),
            ]));
        }
        return;
    };
    let rows = wrap(&tame(description), room.max(1));
    let indent = " ".repeat(lead + label_width + 3);
    for (at, row) in rows.iter().take(BESIDE_DESCRIPTION).enumerate() {
        let mut spans = if at == 0 {
            vec![
                gutter(),
                number.clone(),
                Span::raw("  "),
                Span::raw(format!("{label:<label_width$}   ")),
            ]
        } else {
            vec![Span::raw(indent.clone())]
        };
        spans.push(Span::styled(row.clone(), dim()));
        out.push(Line::from(spans));
    }
    if rows.len() > BESIDE_DESCRIPTION {
        out.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(format!("+{} lines", rows.len() - BESIDE_DESCRIPTION), dim()),
        ]));
    }
}

const OTHER: &str = "Other";
/// The widest an option's label column gets, and the least room a
/// description needs to sit beside it.
const OPTION_LABEL: usize = 28;
const OPTION_BESIDE: usize = 24;
/// Rows of an option's description kept beside it, and under it when
/// narrow.
const BESIDE_DESCRIPTION: usize = 4;
const NARROW_DESCRIPTION: usize = 2;

fn permission_rows(
    detail: &ActivityDetail,
    waiting: &ToolId,
    rows: &Rows<'_, '_>,
    out: &mut Vec<Line<'static>>,
) {
    let width = rows.ctx.columns.saturating_sub(1).max(1);
    let Some((at, call)) = detail
        .log
        .log
        .iter()
        .enumerate()
        .find_map(|(at, e)| match e {
            LogEntry::Called(call) if call.id == *waiting => Some((at, call)),
            _ => None,
        })
    else {
        return;
    };
    let said = detail.log.log[..at].iter().rev().find_map(|e| match e {
        LogEntry::Said { text, .. } => Some(text),
        _ => None,
    });
    if let Some(said) = said {
        let wrapped = wrap(&tame(said), width);
        let mut shown: Vec<String> = wrapped.iter().take(SAID_BEFORE).cloned().collect();
        if wrapped.len() > SAID_BEFORE
            && let Some(last) = shown.last_mut()
        {
            let kept: String = last.chars().take(width.saturating_sub(1)).collect();
            *last = format!("{kept}…");
        }
        out.extend(shown.into_iter().map(plain_row));
        out.push(Line::raw(""));
    }
    let name = format!("{:<6} ", call.tool);
    let room = width.saturating_sub(name.chars().count()).max(1);
    let input: Vec<String> = call
        .input
        .lines()
        .flat_map(|line| wrap(&tame(line), room))
        .collect();
    for (i, row) in input.iter().take(PERMISSION_ROWS).enumerate() {
        let lead = if i == 0 {
            Span::styled(name.clone(), bold())
        } else {
            Span::raw(" ".repeat(name.chars().count()))
        };
        out.push(Line::from(vec![gutter(), lead, Span::raw(row.clone())]));
    }
    if input.len() > PERMISSION_ROWS {
        out.push(Line::from(vec![
            gutter(),
            Span::raw(" ".repeat(name.chars().count())),
            Span::styled(format!("+{} lines", input.len() - PERMISSION_ROWS), dim()),
        ]));
    }
}

/// The most rows of what the agent said before a permission prompt that stay
/// pinned with it.
const SAID_BEFORE: usize = 2;
/// The most rows of a permission prompt's call that stay pinned.
const PERMISSION_ROWS: usize = 6;
