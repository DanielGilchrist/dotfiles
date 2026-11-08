//! A subagent's log as rows: a pinned status row, then the prompt, what it
//! said and its calls, or for a finished one its report first.

use std::time::{Duration, SystemTime};

use agent_core::activity::Subagent;
use agent_core::ports::{Call, CallState, Excerpt, SubagentDetail, Summary, Target};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::{
    SPINNER, accent, ago, bold, dim, error, fit, justify, line_width, success, truncate_end,
    truncate_start, warning, wrap,
};
use crate::log::{Entry, Nav, call_count, entries, error_count, failing, target_text};

/// A call or a line of text sits within this many columns, so it stays near
/// its result on a wide screen.
pub(super) const CAP: usize = 100;
/// Below this width the summary column and tool names shrink.
const WIDE: usize = 100;
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
    pub checkout: Option<&'a str>,
    pub home: Option<&'a str>,
}

pub(super) struct Rendered {
    pub pinned: Vec<Line<'static>>,
    pub rows: Vec<Line<'static>>,
    /// Each entry's first row and row count in `rows`.
    pub layout: Vec<(usize, usize)>,
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
        Summary::Diff { added, removed } => {
            return vec![
                Span::styled(format!("+{added}"), success()),
                Span::styled(format!(" −{removed}"), error()),
            ];
        }
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
    if let Some(rest) = ctx
        .checkout
        .and_then(|root| path.strip_prefix(root))
        .and_then(|rest| rest.strip_prefix('/'))
    {
        return rest.to_string();
    }
    match ctx.home.and_then(|home| path.strip_prefix(home)) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
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

#[allow(clippy::struct_excessive_bools)]
struct Rows<'a, 'b> {
    ctx: &'a Context<'b>,
    cap: usize,
    narrow: bool,
    /// Whether the subagent is still going, so its last call is live.
    live: bool,
    /// Whether it has gone quiet, so spinners stand still.
    stale: bool,
    /// When the hooks say its running tool started, for a call whose
    /// request carried no time.
    running_since: Option<SystemTime>,
    out: Vec<Line<'static>>,
    /// Whether the last row is a rule, which a blank row never follows.
    after_rule: bool,
}

impl Rows<'_, '_> {
    fn push(&mut self, line: Line<'static>) {
        self.after_rule = false;
        self.out.push(line);
    }

    fn blank(&mut self) {
        if !self.out.is_empty() && !self.after_rule {
            self.out.push(Line::raw(""));
        }
    }

    fn rule(&mut self, label: &str) {
        let room = self.cap.saturating_sub(1);
        let text = format!("{label} ");
        let rule = "─".repeat(room.saturating_sub(text.chars().count()));
        self.out.push(Line::from(vec![
            gutter(),
            Span::styled(text + &rule, dim()),
        ]));
        self.after_rule = true;
    }

    fn text_rows(&mut self, text: &str, indent: usize) {
        let width = self.cap.saturating_sub(1 + indent).max(1);
        for row in wrap(&tame(text), width) {
            self.push(indented(indent, vec![Span::raw(row)]));
        }
    }

    fn report(&mut self, report: &str, detail: &SubagentDetail) {
        self.blank();
        self.push(Line::from(vec![gutter(), Span::styled("Report", bold())]));
        self.text_rows(report, 2);
        self.blank();
        let calls = call_count(detail);
        let count = if detail.truncated {
            format!("{calls}+ calls")
        } else {
            plural(calls, "call", "calls")
        };
        self.rule(&format!("Activity · {count}"));
    }

    fn prompt(&mut self, prompt: &str, open: bool) {
        let prompt = tame(prompt);
        let lead = Span::styled("›", accent());
        if open {
            let width = self.cap.saturating_sub(3).max(1);
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
        let room = self.cap.saturating_sub(3);
        self.push(Line::from(vec![
            gutter(),
            lead,
            Span::raw(" "),
            Span::raw(truncate_end(first, room)),
        ]));
        if more > 0 {
            self.push(indented(
                2,
                vec![Span::styled(format!("+{more} lines"), dim())],
            ));
        }
    }

    fn said(&mut self, text: &str, open: bool) {
        let width = self.cap.saturating_sub(1).max(1);
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
                    format!("+{} lines", rows.len() - shown),
                    dim(),
                )],
            ));
        }
    }

    fn is_live(&self, call: &Call, last: bool) -> bool {
        self.live && last && matches!(call.state, CallState::Running)
    }

    fn glyph(&self, call: &Call, last: bool) -> Span<'static> {
        match &call.state {
            CallState::Running if self.is_live(call, last) && self.stale => {
                Span::styled(SPINNER[0], dim())
            }
            CallState::Running if self.is_live(call, last) => Span::raw(self.ctx.spinner),
            CallState::Running | CallState::Done(_) => Span::styled("●", dim()),
            CallState::Failed { .. } => Span::styled("!", error().add_modifier(Modifier::BOLD)),
            CallState::Flagged { .. } => Span::styled("●", warning()),
        }
    }

    fn target_spans(&self, target: &Target, room: usize) -> Vec<Span<'static>> {
        match target {
            Target::Path { path, range } => {
                let shown = unwind(path, self.ctx);
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
                        format!(" in {}", unwind(path, self.ctx)),
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
        let column = if self.narrow {
            SUMMARY_NARROW
        } else {
            SUMMARY_WIDE
        };
        let took = call.took.filter(|d| d.as_secs() >= DURATION_FROM).map(span);
        let mut spans = match &call.state {
            CallState::Running => {
                let ran = call
                    .started
                    .or(self.running_since)
                    .and_then(|from| self.ctx.now.duration_since(from).ok());
                let mut text = ran.map(span).unwrap_or_default();
                let stuck = ran.is_some_and(|d| d >= STUCK);
                if !self.narrow || text.is_empty() {
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
            CallState::Done(summary) | CallState::Flagged { summary, .. } => summary_spans(summary),
            CallState::Failed { code, .. } => {
                vec![Span::styled(failure_text(*code), error())]
            }
        };
        if self.narrow
            && matches!(call.state, CallState::Done(_))
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
        let name = if self.narrow { 5 } else { 6 };
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
            .cap
            .saturating_sub(used + right_width + 2 + suffix.as_ref().map_or(0, Span::width));
        left.extend(label.unwrap_or_else(|| self.target_spans(&call.target, room)));
        let left = fit(
            left,
            self.cap
                .saturating_sub(right_width + 2 + suffix.as_ref().map_or(0, Span::width)),
        );
        let mut left = left;
        left.extend(suffix);
        self.push(justify(left, right, self.cap));
    }

    fn excerpt_rows(&mut self, excerpt: &Excerpt, style: ratatui::style::Style, more: &str) {
        let width = self.cap.saturating_sub(7);
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
        let width = self.cap.saturating_sub(6).max(1);
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
                    format!("+{} lines", result.len() - RESULT_LINES),
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
            CallState::Running | CallState::Done(_) => {}
        }
    }

    fn group(&mut self, tool: &str, calls: &[&Call]) {
        let noun = match tool {
            "Read" => "files",
            "Grep" => "searches",
            "Glob" => "globs",
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
                CallState::Done(Summary::Lines(n)) => lines += n,
                CallState::Done(Summary::Matches(n)) => matches += n,
                CallState::Done(Summary::Files(n)) => files += n,
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
        let name = if self.narrow { 5 } else { 6 };
        let head = format!("{:<name$} {} {noun}  ", tool, calls.len());
        let used = 3 + 2 + head.chars().count() + total.chars().count() + 2;
        let room = self.cap.saturating_sub(used);
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
            self.cap,
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

fn status_row(
    detail: &SubagentDetail,
    subagent: Option<&Subagent>,
    ctx: &Context<'_>,
    live: bool,
    stale: bool,
    narrow: bool,
) -> Vec<Span<'static>> {
    let sep = || Span::styled(" · ", dim());
    let calls = {
        let n = call_count(detail);
        let text = if detail.truncated {
            format!("{n}+ calls")
        } else {
            plural(n, "call", "calls")
        };
        Span::styled(text, dim())
    };
    let errors = error_count(detail);
    let error_span = (errors > 0).then(|| Span::styled(plural(errors, "error", "errors"), error()));
    let model = detail
        .model
        .clone()
        .filter(|_| !narrow)
        .map(|m| Span::styled(m, dim()));
    let began = subagent.map(|s| s.since).or(detail.started);
    let ended = subagent.and_then(|s| s.finished).or(detail.last_heard);
    let working = began
        .and_then(|from| ctx.now.duration_since(from).ok())
        .map(span);
    let took = began
        .zip(ended)
        .and_then(|(from, to)| to.duration_since(from).ok())
        .map(span);
    let finished = !live;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let tail = |spans: &mut Vec<Span<'static>>, extras: Vec<Option<Span<'static>>>| {
        for extra in extras.into_iter().flatten() {
            spans.push(sep());
            spans.push(extra);
        }
    };
    if finished && detail.report.is_some() {
        spans.push(Span::styled("✓", success().add_modifier(Modifier::BOLD)));
        spans.push(Span::raw(format!(
            " finished{}",
            took.map(|t| format!(" in {t}")).unwrap_or_default()
        )));
        tail(&mut spans, vec![Some(calls), error_span, model]);
    } else if finished {
        spans.push(Span::styled("●", dim()));
        spans.push(Span::styled(
            format!(
                " ended{}",
                took.map(|t| format!(" after {t}")).unwrap_or_default()
            ),
            dim(),
        ));
        tail(
            &mut spans,
            vec![
                Some(Span::styled("no report", dim())),
                Some(calls),
                error_span,
            ],
        );
    } else if let Some(stopped) = &detail.stopped {
        spans.push(Span::styled("!", error().add_modifier(Modifier::BOLD)));
        spans.push(Span::raw(" stopped"));
        tail(&mut spans, vec![Some(Span::raw(stopped.clone()))]);
    } else if let Some(reason) = failing(detail) {
        spans.push(Span::styled("!", error().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(" failing", bold()));
        spans.push(sep());
        spans.push(Span::raw(reason));
        if !narrow {
            tail(
                &mut spans,
                vec![
                    working.map(|w| Span::styled(format!("working {w}"), dim())),
                    Some(calls),
                    model,
                ],
            );
        }
    } else {
        let glyph = if stale {
            Span::styled(SPINNER[0], dim())
        } else {
            Span::raw(ctx.spinner)
        };
        spans.push(glyph);
        spans.push(Span::raw(format!(
            " working{}",
            working.map(|w| format!(" {w}")).unwrap_or_default()
        )));
        let quiet = stale
            .then(|| {
                detail
                    .last_heard
                    .and_then(|at| ctx.now.duration_since(at).ok())
            })
            .flatten()
            .map(|d| Span::raw(format!("quiet {} ", ago(ctx.now - d, ctx.now))));
        let quiet = quiet.map(|q| {
            let mut text = q.content.to_string();
            text.push('?');
            Span::styled(text, dim())
        });
        tail(&mut spans, vec![quiet, Some(calls), error_span, model]);
    }
    spans
}

pub(super) fn render(
    detail: &SubagentDetail,
    subagent: Option<&Subagent>,
    nav: &Nav,
    ctx: &Context<'_>,
) -> Rendered {
    let finished = detail.finished || subagent.is_some_and(|s| s.finished.is_some());
    let live = !finished;
    let idle = subagent.is_none_or(|s| s.tools.is_empty())
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
    let narrow = ctx.columns < WIDE;
    let cap = ctx.columns.min(CAP);
    let status = status_row(detail, subagent, ctx, live, stale, narrow);
    let mut pinned = vec![Line::from(
        std::iter::once(gutter())
            .chain(fit(status, cap.saturating_sub(1)))
            .collect::<Vec<_>>(),
    )];
    pinned.push(Line::from(vec![
        gutter(),
        Span::styled("─".repeat(cap.saturating_sub(1)), dim()),
    ]));

    let mut rows = Rows {
        ctx,
        cap,
        narrow,
        live,
        stale,
        running_since: subagent.and_then(|s| s.tools.last()).map(|t| t.since),
        out: Vec::new(),
        after_rule: false,
    };
    let mut layout = Vec::new();
    let all = entries(detail);
    let last_call = all.len().saturating_sub(1);
    let report = detail.report.as_deref().filter(|_| finished);
    let has_prompt = matches!(all.first(), Some(Entry::Prompt(_)));
    if let (false, Some(report)) = (has_prompt, report) {
        rows.report(report, detail);
    }
    for (at, entry) in all.iter().enumerate() {
        let open = nav.is_open(at);
        let started = rows.out.len();
        match entry {
            Entry::Prompt(prompt) => rows.prompt(prompt, open),
            Entry::Said(text) => {
                rows.blank();
                let started = rows.out.len();
                rows.said(text, open);
                layout.push((started, rows.out.len() - started));
                continue;
            }
            Entry::Call(call) => rows.call(call, at == last_call, None, open),
            Entry::Group { tool, calls } => {
                if open {
                    for call in calls {
                        rows.call_row(call, false, None, None);
                    }
                } else {
                    rows.group(tool, calls);
                }
            }
            Entry::Repeat(calls) => {
                if open {
                    for call in calls {
                        rows.call(call, false, None, false);
                    }
                } else if let Some(call) = calls.last() {
                    let times = Span::styled(format!("  ×{}", calls.len()), dim());
                    rows.call(call, at == last_call, Some(times), false);
                }
            }
        }
        layout.push((started, rows.out.len() - started));
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
