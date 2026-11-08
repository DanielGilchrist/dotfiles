//! What each agent is doing, as its harness reports it through hooks
//! (`agent event`), so the TUI can show it and say which agents need you.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// No event for this long while working, with no tool running, looks stuck.
/// Thinking through a long answer can go a few minutes without an event,
/// and a running tool (a test suite, a build) can take as long as it likes.
pub const STALE_AFTER: Duration = Duration::from_secs(300);

/// A subagent's id, as the harness names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A tool call's id, as the harness names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolId(String);

impl ToolId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A process id. Zero is never a process, so it can't be one here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Pid(NonZeroU32);

impl Pid {
    pub fn new(raw: u32) -> Option<Self> {
        NonZeroU32::new(raw).map(Self)
    }

    pub fn current() -> Option<Self> {
        Self::new(std::process::id())
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// How a session's hooks can be traced back to its launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Join {
    /// The harness was just started by this process, which every hook of
    /// the new session descends from.
    Fresh(Pid),
    /// Its hooks come from some other process, as when it joined a session
    /// that was already running, so a report can't be told from a replaced
    /// session's.
    Untraced,
}

/// Who an event came from: the agent itself or one of its subagents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Main,
    Subagent(AgentId),
}

/// An event and who it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    pub event: Event,
    /// The reporting process and its ancestors, when they could be read.
    pub lineage: Option<Vec<Pid>>,
}

impl Reported {
    pub fn main(event: impl Into<MainEvent>) -> Self {
        Self {
            event: Event::Main(event.into()),
            lineage: None,
        }
    }

    pub fn subagent(id: AgentId, event: impl Into<SubagentEvent>) -> Self {
        Self {
            event: Event::Subagent(id, event.into()),
            lineage: None,
        }
    }

    #[must_use]
    pub fn from_lineage(mut self, lineage: Option<Vec<Pid>>) -> Self {
        self.lineage = lineage;
        self
    }
}

/// Something a harness reported, from the agent or from one of its
/// subagents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Main(MainEvent),
    Subagent(AgentId, SubagentEvent),
}

/// What either the agent or a subagent can report about its own tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Common {
    /// A tool began: its id, its name, and what it's acting on when that's
    /// short.
    ToolStarted {
        id: ToolId,
        tool: String,
        target: Option<String>,
    },
    /// A tool finished, failed, or was denied permission.
    ToolFinished { id: ToolId },
    /// Waiting on the user: a permission prompt or a question.
    Asked { question: String, about: About },
    /// The user turned down the asker's permission prompt.
    Declined,
}

/// Something the agent itself reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainEvent {
    /// `agent exec` is about to start the harness: a new session, not yet
    /// reporting.
    Launched(Join),
    /// A session began (fresh, resumed or cleared).
    Started,
    /// The context was compacted.
    Compacted,
    Prompted(Input),
    /// The turn ended. `summary` is the agent's last message, and `running`
    /// the background subagents it's still waiting on.
    Finished {
        summary: Option<String>,
        running: Vec<Background>,
    },
    /// The turn ended in an error (rate limited, overloaded…).
    Failed(String),
    /// The session ended.
    Ended,
    Common(Common),
}

/// Something a subagent reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentEvent {
    /// The subagent began running, in the foreground until a launch says
    /// otherwise.
    Started {
        kind: String,
    },
    /// Tool call `tool` handed this subagent off to run in the background.
    Launched {
        tool: ToolId,
        kind: String,
        description: String,
    },
    Stopped,
    Common(Common),
}

impl From<Common> for MainEvent {
    fn from(common: Common) -> Self {
        Self::Common(common)
    }
}

impl From<Common> for SubagentEvent {
    fn from(common: Common) -> Self {
        Self::Common(common)
    }
}

impl Common {
    fn answers(&self, by: &Answer) -> bool {
        match (self, by) {
            (Self::ToolFinished { id }, Answer::ToolEnding(tool)) => id == tool,
            (Self::ToolStarted { .. } | Self::ToolFinished { .. }, Answer::AnyTool)
            | (Self::Declined, _) => true,
            (Self::ToolStarted { .. } | Self::Asked { .. }, _) => false,
        }
    }
}

impl Event {
    fn is_from(&self, asker: &Origin) -> bool {
        match (self, asker) {
            (Self::Main(_), Origin::Main) => true,
            (Self::Subagent(id, _), Origin::Subagent(asker)) => id == asker,
            (Self::Main(_), Origin::Subagent(_)) | (Self::Subagent(..), Origin::Main) => false,
        }
    }

    fn answers(&self, by: &Answer) -> bool {
        match self {
            Self::Main(MainEvent::Common(common))
            | Self::Subagent(_, SubagentEvent::Common(common)) => common.answers(by),
            Self::Main(MainEvent::Prompted(_) | MainEvent::Failed(_))
            | Self::Subagent(_, SubagentEvent::Stopped) => true,
            Self::Main(
                MainEvent::Launched(_)
                | MainEvent::Started
                | MainEvent::Compacted
                | MainEvent::Finished { .. }
                | MainEvent::Ended,
            )
            | Self::Subagent(_, SubagentEvent::Started { .. } | SubagentEvent::Launched { .. }) => {
                false
            }
        }
    }
}

/// What a question is about, as far as the harness can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum About {
    /// This tool call.
    Tool(ToolId),
    /// A call to `tool` on `target` that's already running.
    Call {
        tool: String,
        target: Option<String>,
    },
    Unknown,
}

/// A prompt the agent received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    User(String),
    /// The harness telling the agent something, like a subagent finishing.
    Notification,
}

/// A background subagent a finished turn is still waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Background {
    pub id: AgentId,
    pub kind: String,
    pub description: Option<String>,
}

/// What an agent is doing, apart from any questions it's waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum State {
    /// Waiting for a prompt, and the user has seen where it got to.
    Idle,
    /// The tools running, oldest first. Several run at once when the agent
    /// fans out.
    Working {
        tools: Vec<Tool>,
    },
    /// The turn ended but background subagents are still running, so
    /// another turn follows when they report back.
    Delegating,
    /// Finished a turn the user hasn't looked at yet.
    Done {
        summary: Option<String>,
    },
    Failed {
        reason: String,
    },
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub id: ToolId,
    pub name: String,
    pub target: Option<String>,
    pub since: SystemTime,
}

/// A question waiting on the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub asker: Origin,
    pub question: String,
    pub answered_by: Answer,
    pub since: SystemTime,
}

/// What shows a question was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// The end of the tool call it's about.
    ToolEnding(ToolId),
    /// Any tool activity from the asker, when the call isn't known.
    AnyTool,
}

/// A subagent the agent started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredSubagent", into = "WrittenSubagent")]
pub struct Subagent {
    pub kind: String,
    pub description: Option<String>,
    pub launch: Launch,
    pub since: SystemTime,
    pub status: SubagentStatus,
}

/// Whether a subagent is still going. A finished one is kept until the next
/// prompt so its row and transcript stay to be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentStatus {
    /// The tools it's running.
    Running {
        tools: Vec<Tool>,
    },
    Finished {
        at: SystemTime,
    },
}

impl Subagent {
    fn new(kind: String, launch: Launch, now: SystemTime) -> Self {
        Self {
            kind,
            description: None,
            launch,
            since: now,
            status: SubagentStatus::Running { tools: Vec::new() },
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, SubagentStatus::Running { .. })
    }

    /// The tools it's running, none once it has finished.
    pub fn tools(&self) -> &[Tool] {
        match &self.status {
            SubagentStatus::Running { tools } => tools,
            SubagentStatus::Finished { .. } => &[],
        }
    }

    /// Marks it finished at `at`, unless it already was.
    pub fn finish(&mut self, at: SystemTime) {
        if self.is_running() {
            self.status = SubagentStatus::Finished { at };
        }
    }

    pub fn start(&mut self, tool: Tool) {
        if let SubagentStatus::Running { tools } = &mut self.status {
            tools.push(tool);
        }
    }

    fn end(&mut self, id: &ToolId) {
        if let SubagentStatus::Running { tools } = &mut self.status {
            tools.retain(|t| &t.id != id);
        }
    }
}

/// Whether the agent waits on a subagent's tool call or carries on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Launch {
    Foreground,
    Background,
}

/// Where the current session is in its life, which decides whose reports to
/// believe and when typed text is safe. Each carries how the session was
/// launched: reports from processes that don't descend from a fresh launch
/// are from a session it replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Session {
    /// Launched through `agent`, which records it before the harness
    /// starts, and nothing has been reported since: still inside the boot
    /// window.
    Launched {
        join: Join,
        at: SystemTime,
    },
    /// Launched, and the harness has reported something other than
    /// starting.
    LaunchReported {
        join: Join,
        at: SystemTime,
    },
    /// The last report was a session starting, which a dialog may still be
    /// covering.
    Booting {
        join: Join,
    },
    Live {
        join: Join,
    },
}

impl Session {
    fn join(self) -> Join {
        match self {
            Self::Launched { join, .. }
            | Self::LaunchReported { join, .. }
            | Self::Booting { join }
            | Self::Live { join } => join,
        }
    }

    /// When the session was launched through `agent`, until it reports
    /// starting or ending. Absent for sessions an older binary started.
    fn launched_at(self) -> Option<SystemTime> {
        match self {
            Self::Launched { at, .. } | Self::LaunchReported { at, .. } => Some(at),
            Self::Booting { .. } | Self::Live { .. } => None,
        }
    }

    pub fn is_booting(self) -> bool {
        matches!(self, Self::Launched { .. } | Self::Booting { .. })
    }

    fn after(self, event: &Event, now: SystemTime) -> Self {
        match (event, self) {
            (Event::Main(MainEvent::Launched(join)), _) => Self::Launched {
                join: *join,
                at: now,
            },
            (Event::Main(MainEvent::Started), session) => Self::Booting {
                join: session.join(),
            },
            (Event::Main(MainEvent::Ended), session) => Self::Live {
                join: session.join(),
            },
            (_, Self::Launched { join, at }) => Self::LaunchReported { join, at },
            (_, Self::Booting { join }) => Self::Live { join },
            (_, session @ (Self::LaunchReported { .. } | Self::Live { .. })) => session,
        }
    }
}

/// An agent's state, since when, what it was last asked, and what it's
/// waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Stored", into = "Written")]
pub struct Activity {
    pub state: State,
    pub since: SystemTime,
    pub prompt: Option<String>,
    /// When the harness last reported anything.
    pub heard: SystemTime,
    /// Counts the harness's reports, so a view can show progress as it
    /// happens.
    pub events: u64,
    /// Open questions, at most one per asker, oldest first.
    pub asks: Vec<Ask>,
    pub subagents: BTreeMap<AgentId, Subagent>,
    pub session: Session,
}

/// How much earlier than a bound a launch may be recorded and still count
/// as the session the bound is about: the TUI notices a session a moment
/// after it is launched.
pub const LAUNCH_SLACK: Duration = Duration::from_secs(15);

/// How long a session that just reported starting must stay quiet before
/// typed text is safe: a startup dialog may be open without any report.
pub const BOOT_QUIET: Duration = Duration::from_secs(5);

impl Activity {
    /// What an agent that has reported nothing yet is taken to be doing.
    pub fn fresh(now: SystemTime) -> Self {
        Self {
            state: State::Working { tools: Vec::new() },
            since: now,
            prompt: None,
            heard: now,
            events: 0,
            asks: Vec::new(),
            subagents: BTreeMap::new(),
            session: Session::Live {
                join: Join::Untraced,
            },
        }
    }

    /// The activity after `reported` happened at `now`. The time in state
    /// only restarts when the state itself changes, not when a working agent
    /// moves to its next tool or answers a question.
    pub fn next(previous: Option<Self>, reported: Reported, now: SystemTime) -> Self {
        if let Some(previous) = previous.as_ref().filter(|p| p.is_stale_report(&reported)) {
            return previous.clone();
        }
        let mut activity = previous.unwrap_or_else(|| Self::fresh(now));
        activity.session = activity.session.after(&reported.event, now);
        let before = activity.state.clone();
        let answered = activity.answer(&reported.event);
        activity.apply(reported.event, answered, now);
        if !same_kind(&before, &activity.state) {
            activity.since = now;
        }
        activity.heard = now;
        activity.events += 1;
        activity
    }

    /// `reported` came from a process that isn't part of the session
    /// launched last, such as a background session it replaced. Reports
    /// whose process couldn't be traced are given the benefit of the doubt.
    fn is_stale_report(&self, reported: &Reported) -> bool {
        match (self.session.join(), &reported.lineage, &reported.event) {
            (_, _, Event::Main(MainEvent::Launched(_))) | (Join::Untraced, _, _) | (_, None, _) => {
                false
            }
            (Join::Fresh(pid), Some(lineage), _) => !lineage.contains(&pid),
        }
    }

    /// Takes out the question `event` shows was answered.
    fn answer(&mut self, event: &Event) -> Option<Ask> {
        let at = self
            .asks
            .iter()
            .position(|ask| event.is_from(&ask.asker) && event.answers(&ask.answered_by))?;
        Some(self.asks.remove(at))
    }

    fn apply(&mut self, event: Event, answered: Option<Ask>, now: SystemTime) {
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match event {
            Event::Main(event) => self.apply_main(event, state, answered, now),
            Event::Subagent(id, event) => self.apply_subagent(id, event, state, answered, now),
        };
    }

    fn apply_main(
        &mut self,
        event: MainEvent,
        state: State,
        answered: Option<Ask>,
        now: SystemTime,
    ) -> State {
        match event {
            MainEvent::Launched(_) => {
                self.forget_subagents();
                State::Working { tools: Vec::new() }
            }
            MainEvent::Started => {
                self.forget_subagents();
                State::Idle
            }
            MainEvent::Ended => {
                self.forget_subagents();
                State::Exited
            }
            MainEvent::Compacted => state,
            MainEvent::Prompted(input) => {
                if let Input::User(text) = input {
                    self.prompt = Some(text);
                    self.subagents
                        .retain(|_, s| s.launch == Launch::Background && s.is_running());
                    let subagents = &self.subagents;
                    self.asks.retain(|ask| match &ask.asker {
                        Origin::Subagent(id) => subagents.contains_key(id),
                        Origin::Main => false,
                    });
                }
                State::Working { tools: Vec::new() }
            }
            MainEvent::Finished { summary, running } => {
                self.asks.retain(|ask| match &ask.asker {
                    Origin::Subagent(id) => running.iter().any(|b| &b.id == id),
                    Origin::Main => false,
                });
                self.track_background(&running, now);
                if running.is_empty() {
                    State::Done { summary }
                } else {
                    State::Delegating
                }
            }
            MainEvent::Failed(reason) => State::Failed { reason },
            MainEvent::Common(Common::ToolStarted { id, tool, target }) => {
                let mut tools = running(state);
                tools.retain(|t| t.id != id);
                tools.push(Tool {
                    id,
                    name: tool,
                    target,
                    since: now,
                });
                State::Working { tools }
            }
            MainEvent::Common(Common::ToolFinished { id }) => finish(state, &id),
            MainEvent::Common(Common::Declined) => match declined(answered) {
                Some(id) => finish(state, &id),
                None => state,
            },
            MainEvent::Common(Common::Asked { question, about }) => {
                self.ask(Origin::Main, question, about, &state, now);
                state
            }
        }
    }

    fn apply_subagent(
        &mut self,
        id: AgentId,
        event: SubagentEvent,
        state: State,
        answered: Option<Ask>,
        now: SystemTime,
    ) -> State {
        match event {
            SubagentEvent::Started { kind } => {
                self.subagents
                    .entry(id)
                    .or_insert_with(|| Subagent::new(kind, Launch::Foreground, now));
                state
            }
            SubagentEvent::Launched {
                tool,
                kind,
                description,
            } => {
                let subagent = self
                    .subagents
                    .entry(id)
                    .or_insert_with(|| Subagent::new(kind, Launch::Background, now));
                subagent.launch = Launch::Background;
                subagent.description = Some(description);
                finish(state, &tool)
            }
            SubagentEvent::Stopped => {
                if let Some(subagent) = self.subagents.get_mut(&id) {
                    subagent.finish(now);
                }
                state
            }
            SubagentEvent::Common(Common::ToolStarted {
                id: tool,
                tool: name,
                target,
            }) => {
                if let Some(subagent) = self.subagents.get_mut(&id) {
                    subagent.start(Tool {
                        id: tool,
                        name,
                        target,
                        since: now,
                    });
                }
                state
            }
            SubagentEvent::Common(Common::ToolFinished { id: tool }) => {
                if let Some(subagent) = self.subagents.get_mut(&id) {
                    subagent.end(&tool);
                }
                state
            }
            SubagentEvent::Common(Common::Declined) => {
                if let (Some(subagent), Some(tool)) =
                    (self.subagents.get_mut(&id), declined(answered))
                {
                    subagent.end(&tool);
                }
                state
            }
            SubagentEvent::Common(Common::Asked { question, about }) => {
                self.ask(Origin::Subagent(id), question, about, &state, now);
                state
            }
        }
    }

    fn ask(
        &mut self,
        asker: Origin,
        question: String,
        about: About,
        state: &State,
        now: SystemTime,
    ) {
        let answered_by = self.answered_by(&asker, about, state);
        self.asks.retain(|ask| ask.asker != asker);
        self.asks.push(Ask {
            asker,
            question,
            answered_by,
            since: now,
        });
    }

    /// The finished turn's background subagents replace the ones tracked.
    fn track_background(&mut self, running: &[Background], now: SystemTime) {
        self.subagents
            .retain(|id, s| !s.is_running() || running.iter().any(|b| &b.id == id));
        for b in running {
            let subagent = self
                .subagents
                .entry(b.id.clone())
                .or_insert_with(|| Subagent::new(b.kind.clone(), Launch::Background, now));
            subagent.launch = Launch::Background;
            if b.description.is_some() {
                subagent.description.clone_from(&b.description);
            }
        }
    }

    /// What will show `asker`'s question about `about` was answered: the
    /// end of the matching call it most recently started, when there is one.
    fn answered_by(&self, asker: &Origin, about: About, state: &State) -> Answer {
        let (tool, target) = match about {
            About::Tool(id) => return Answer::ToolEnding(id),
            About::Call { tool, target } => (tool, target),
            About::Unknown => return Answer::AnyTool,
        };
        let running: &[Tool] = match (asker, state) {
            (Origin::Main, State::Working { tools }) => tools,
            (Origin::Main, _) => &[],
            (Origin::Subagent(id), _) => self.subagents.get(id).map_or(&[], Subagent::tools),
        };
        running
            .iter()
            .rev()
            .find(|t| t.name == tool && t.target == target)
            .map_or(Answer::AnyTool, |t| Answer::ToolEnding(t.id.clone()))
    }

    fn forget_subagents(&mut self) {
        self.subagents.clear();
        self.asks.clear();
    }

    /// Its turn is over with nothing running and nothing asked, so nothing
    /// can open a prompt that typed text would answer.
    pub fn at_rest(&self) -> bool {
        matches!(self.state, State::Idle | State::Done { .. })
            && self.asks.is_empty()
            && self.running() == 0
    }

    /// [`Self::at_rest`], the bound is met, and a session that only just
    /// reported starting has had [`BOOT_QUIET`] to show a dialog.
    pub fn ready_for_text(&self, not_before: Option<SystemTime>, now: SystemTime) -> bool {
        self.at_rest()
            && self.bound_met(not_before)
            && !(self.session.is_booting()
                && now
                    .duration_since(self.heard)
                    .is_ok_and(|quiet| quiet < BOOT_QUIET))
    }

    /// The record can't be from before the session `not_before` marks: it
    /// has launched since (so its own boot state is authoritative), or the
    /// harness has reported after it.
    pub fn bound_met(&self, not_before: Option<SystemTime>) -> bool {
        not_before.is_none_or(|t| {
            self.heard > t
                || self
                    .session
                    .launched_at()
                    .is_some_and(|l| l + LAUNCH_SLACK >= t)
        })
    }

    /// How many subagents are still running.
    pub fn running(&self) -> usize {
        self.subagents.values().filter(|s| s.is_running()).count()
    }

    /// The question that's waited longest, if any.
    pub fn waiting(&self) -> Option<&Ask> {
        self.asks.first()
    }

    /// The user has looked at it: a finished turn stops being news. This
    /// isn't news from the harness, so it doesn't count as hearing from it.
    #[must_use]
    pub fn acknowledged(self, now: SystemTime) -> Self {
        match self.state {
            State::Done { .. } => Self {
                state: State::Idle,
                since: now,
                ..self
            },
            _ => self,
        }
    }

    /// The tool most recently started, if any is running.
    pub fn current_tool(&self) -> Option<&Tool> {
        match &self.state {
            State::Working { tools } => tools.last(),
            _ => None,
        }
    }

    /// Working or delegating with no tool running, its own or a subagent's,
    /// and nothing heard for [`STALE_AFTER`]: probably interrupted without
    /// the harness saying so, stuck, or its subagents died with the session.
    pub fn stale(&self, now: SystemTime) -> bool {
        let quiet = now.duration_since(self.heard).unwrap_or_default() > STALE_AFTER;
        let subagent_busy = self.subagents.values().any(|s| !s.tools().is_empty());
        match &self.state {
            State::Working { tools } => tools.is_empty() && !subagent_busy && quiet,
            State::Delegating => !subagent_busy && quiet,
            _ => false,
        }
    }
}

/// An activity file as written by any version, including ones that kept a
/// single question in the state and ones that spread the session over
/// `booting`, `launcher` and `launched`.
#[derive(Deserialize)]
struct Stored {
    state: StoredState,
    since: SystemTime,
    prompt: Option<String>,
    heard: SystemTime,
    events: u64,
    #[serde(default)]
    asks: Vec<Ask>,
    #[serde(default)]
    subagents: BTreeMap<AgentId, Subagent>,
    #[serde(default)]
    session: Option<Session>,
    #[serde(default)]
    booting: bool,
    #[serde(default)]
    launcher: Option<u32>,
    #[serde(default)]
    launched: Option<SystemTime>,
}

/// What an activity file holds. The fields `session` replaced are written
/// too, derived from it, so a binary from before it still reads a booting
/// agent as booting.
#[derive(Serialize)]
struct Written {
    state: State,
    since: SystemTime,
    prompt: Option<String>,
    heard: SystemTime,
    events: u64,
    asks: Vec<Ask>,
    subagents: BTreeMap<AgentId, Subagent>,
    session: Session,
    booting: bool,
    launcher: Option<u32>,
    launched: Option<SystemTime>,
}

impl From<Activity> for Written {
    fn from(activity: Activity) -> Self {
        let launcher = match activity.session.join() {
            Join::Fresh(pid) => Some(pid.get()),
            Join::Untraced => None,
        };
        Self {
            state: activity.state,
            since: activity.since,
            prompt: activity.prompt,
            heard: activity.heard,
            events: activity.events,
            asks: activity.asks,
            subagents: activity.subagents,
            session: activity.session,
            booting: activity.session.is_booting(),
            launcher,
            launched: activity.session.launched_at(),
        }
    }
}

/// A subagent with `tools` and `finished` written beside `status`, for the
/// same reason.
#[derive(Serialize)]
struct WrittenSubagent {
    kind: String,
    description: Option<String>,
    launch: Launch,
    since: SystemTime,
    tools: Vec<Tool>,
    finished: Option<SystemTime>,
    status: SubagentStatus,
}

impl From<Subagent> for WrittenSubagent {
    fn from(subagent: Subagent) -> Self {
        let finished = match subagent.status {
            SubagentStatus::Running { .. } => None,
            SubagentStatus::Finished { at } => Some(at),
        };
        Self {
            tools: subagent.tools().to_vec(),
            finished,
            kind: subagent.kind,
            description: subagent.description,
            launch: subagent.launch,
            since: subagent.since,
            status: subagent.status,
        }
    }
}

/// A subagent as written by any version, including ones that kept `tools`
/// and `finished` apart.
#[derive(Deserialize)]
struct StoredSubagent {
    kind: String,
    description: Option<String>,
    launch: Launch,
    #[serde(default)]
    tools: Vec<Tool>,
    since: SystemTime,
    #[serde(default)]
    finished: Option<SystemTime>,
    #[serde(default)]
    status: Option<SubagentStatus>,
}

impl From<StoredSubagent> for Subagent {
    fn from(stored: StoredSubagent) -> Self {
        let status = stored.status.unwrap_or(match stored.finished {
            Some(at) => SubagentStatus::Finished { at },
            None => SubagentStatus::Running {
                tools: stored.tools,
            },
        });
        Self {
            kind: stored.kind,
            description: stored.description,
            launch: stored.launch,
            since: stored.since,
            status,
        }
    }
}

fn legacy_session(booting: bool, launcher: Option<u32>, launched: Option<SystemTime>) -> Session {
    let join = launcher
        .and_then(Pid::new)
        .map_or(Join::Untraced, Join::Fresh);
    match (launched, booting) {
        (Some(at), true) => Session::Launched { join, at },
        (Some(at), false) => Session::LaunchReported { join, at },
        (None, true) => Session::Booting { join },
        (None, false) => Session::Live { join },
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredState {
    Current(State),
    Legacy(Legacy),
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Legacy {
    Asking { question: String },
}

impl From<Stored> for Activity {
    fn from(stored: Stored) -> Self {
        let Stored {
            state,
            since,
            prompt,
            heard,
            events,
            mut asks,
            subagents,
            session,
            booting,
            launcher,
            launched,
        } = stored;
        let state = match state {
            StoredState::Current(state) => state,
            StoredState::Legacy(Legacy::Asking { question }) => {
                asks.push(Ask {
                    asker: Origin::Main,
                    question,
                    answered_by: Answer::AnyTool,
                    since,
                });
                State::Working { tools: Vec::new() }
            }
        };
        Self {
            state,
            since,
            prompt,
            heard,
            events,
            asks,
            subagents,
            session: session.unwrap_or_else(|| legacy_session(booting, launcher, launched)),
        }
    }
}

/// The tools still running in `state`.
fn running(state: State) -> Vec<Tool> {
    match state {
        State::Working { tools } => tools,
        _ => Vec::new(),
    }
}

/// The call a declined question was about, when it named one.
fn declined(answered: Option<Ask>) -> Option<ToolId> {
    match answered {
        Some(Ask {
            answered_by: Answer::ToolEnding(id),
            ..
        }) => Some(id),
        _ => None,
    }
}

fn finish(state: State, id: &ToolId) -> State {
    match state {
        State::Working { mut tools } => {
            tools.retain(|t| &t.id != id);
            State::Working { tools }
        }
        State::Delegating => State::Delegating,
        State::Idle | State::Done { .. } | State::Failed { .. } | State::Exited => {
            State::Working { tools: Vec::new() }
        }
    }
}

fn same_kind(a: &State, b: &State) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn run(events: Vec<(u64, Reported)>) -> Activity {
        let mut activity = None;
        for (secs, reported) in events {
            activity = Some(Activity::next(activity, reported, at(secs)));
        }
        activity.expect("succeeds")
    }

    fn main(event: impl Into<MainEvent>) -> Reported {
        Reported::main(event)
    }

    fn sub(id: &str, event: impl Into<SubagentEvent>) -> Reported {
        Reported::subagent(AgentId::new(id), event)
    }

    fn pid(raw: u32) -> Pid {
        Pid::new(raw).expect("nonzero")
    }

    fn fresh(raw: u32) -> MainEvent {
        MainEvent::Launched(Join::Fresh(pid(raw)))
    }

    fn untraced() -> MainEvent {
        MainEvent::Launched(Join::Untraced)
    }

    fn start(id: &str, tool: &str) -> Common {
        Common::ToolStarted {
            id: ToolId::new(id),
            tool: tool.into(),
            target: None,
        }
    }

    fn end(id: &str) -> Common {
        Common::ToolFinished {
            id: ToolId::new(id),
        }
    }

    fn user(text: &str) -> MainEvent {
        MainEvent::Prompted(Input::User(text.into()))
    }

    fn asked(question: &str) -> Common {
        Common::Asked {
            question: question.into(),
            about: About::Unknown,
        }
    }

    fn finished(running: &[&str]) -> MainEvent {
        MainEvent::Finished {
            summary: None,
            running: running
                .iter()
                .map(|id| Background {
                    id: AgentId::new(*id),
                    kind: "general-purpose".into(),
                    description: None,
                })
                .collect(),
        }
    }

    fn started() -> SubagentEvent {
        SubagentEvent::Started {
            kind: "general-purpose".into(),
        }
    }

    fn launched(tool: &str) -> SubagentEvent {
        SubagentEvent::Launched {
            tool: ToolId::new(tool),
            kind: "general-purpose".into(),
            description: "review".into(),
        }
    }

    fn questions(activity: &Activity) -> Vec<&str> {
        activity.asks.iter().map(|a| a.question.as_str()).collect()
    }

    #[test]
    fn a_turn_works_through_tools_and_finishes_unseen() {
        let activity = run(vec![
            (0, main(MainEvent::Started)),
            (10, main(user("fix it"))),
            (12, main(start("t1", "Bash"))),
            (40, main(end("t1"))),
        ]);
        assert_eq!(activity.state, State::Working { tools: Vec::new() });
        assert_eq!(
            activity.since,
            at(10),
            "moving between tools isn't a new state"
        );
        assert_eq!(activity.prompt.as_deref(), Some("fix it"));
        let done = Activity::next(
            Some(activity),
            main(MainEvent::Finished {
                summary: Some("Fixed.".into()),
                running: Vec::new(),
            }),
            at(50),
        );
        assert_eq!(
            (done.state.clone(), done.since),
            (
                State::Done {
                    summary: Some("Fixed.".into())
                },
                at(50)
            )
        );
        let seen = done.clone().acknowledged(at(60));
        assert_eq!(
            (seen.state, seen.heard, seen.events),
            (State::Idle, done.heard, done.events),
            "seeing isn't news"
        );
    }

    #[test]
    fn parallel_tools_are_tracked_apart() {
        let activity = run(vec![
            (0, main(start("a", "Task"))),
            (1, main(start("b", "Task"))),
            (5, main(end("a"))),
        ]);
        assert_eq!(activity.current_tool().map(|t| t.id.as_str()), Some("b"));
        assert!(!activity.stale(at(900)), "the other task is still running");
    }

    #[test]
    fn a_permission_prompt_is_answered_when_its_tool_ends() {
        let asking = run(vec![
            (0, main(user("go"))),
            (1, main(start("t", "Bash"))),
            (
                2,
                main(Common::Asked {
                    question: "Bash: cargo build".into(),
                    about: About::Call {
                        tool: "Bash".into(),
                        target: None,
                    },
                }),
            ),
            (3, sub("x", launched("other"))),
            (4, main(end("parallel"))),
        ]);
        assert_eq!(
            questions(&asking),
            ["Bash: cargo build"],
            "other tools ending isn't an answer"
        );
        let ran = Activity::next(Some(asking), main(end("t")), at(9));
        assert_eq!(questions(&ran), [""; 0]);
        assert_eq!(
            (ran.state, ran.since),
            (State::Working { tools: Vec::new() }, at(0)),
            "asking isn't a new state"
        );
    }

    #[test]
    fn a_question_without_a_tool_is_answered_by_the_askers_next_tool() {
        let answered = run(vec![
            (0, main(asked("Which DB?"))),
            (5, main(start("t", "Read"))),
        ]);
        assert_eq!(questions(&answered), [""; 0]);
    }

    #[test]
    fn only_whoever_asked_can_answer() {
        let asking = run(vec![
            (0, main(start("agent", "Agent"))),
            (1, sub("a", started())),
            (1, sub("b", started())),
            (2, sub("a", start("x", "Bash"))),
            (3, sub("a", asked("Bash: rm -rf target"))),
            (4, sub("b", start("y", "Read"))),
            (5, sub("b", SubagentEvent::Stopped)),
            (6, main(MainEvent::Compacted)),
        ]);
        assert_eq!(questions(&asking), ["Bash: rm -rf target"]);
        let answered = Activity::next(Some(asking), sub("a", end("x")), at(7));
        assert_eq!(questions(&answered), [""; 0]);
        assert_eq!(
            answered.current_tool().map(|t| t.name.as_str()),
            Some("Agent")
        );
    }

    #[test]
    fn several_questions_wait_side_by_side() {
        let both = run(vec![
            (0, main(start("t", "Agent"))),
            (1, sub("a", launched("t"))),
            (1, sub("a", started())),
            (2, sub("a", asked("Bash: cargo test"))),
            (3, main(start("u", "Bash"))),
            (4, main(asked("Bash: git push"))),
        ]);
        assert_eq!(
            both.waiting().map(|a| a.question.as_str()),
            Some("Bash: cargo test"),
            "longest wait first"
        );
        let one = Activity::next(Some(both), main(end("u")), at(5));
        assert_eq!(questions(&one), ["Bash: cargo test"]);
        let delegating = Activity::next(Some(one), main(finished(&["a"])), at(6));
        assert_eq!(
            (delegating.state.clone(), questions(&delegating)),
            (State::Delegating, vec!["Bash: cargo test"]),
            "a background subagent's prompt outlives the turn"
        );
        let done = Activity::next(Some(delegating), main(finished(&[])), at(7));
        assert_eq!(questions(&done), [""; 0], "its subagent is gone");
    }

    #[test]
    fn a_permission_prompt_belongs_to_the_call_it_names() {
        let asking = run(vec![
            (0, main(start("rm", "Bash"))),
            (1, main(start("read", "Read"))),
            (
                2,
                main(Common::Asked {
                    question: "Bash: rm -rf target".into(),
                    about: About::Call {
                        tool: "Bash".into(),
                        target: None,
                    },
                }),
            ),
            (3, main(end("read"))),
        ]);
        assert_eq!(
            questions(&asking),
            ["Bash: rm -rf target"],
            "the parallel Read ending isn't an answer"
        );
        let declined = Activity::next(Some(asking), main(Common::Declined), at(4));
        assert_eq!(questions(&declined), [""; 0]);
        assert_eq!(
            declined.current_tool(),
            None,
            "the declined call isn't running"
        );
    }

    #[test]
    fn a_new_prompt_drops_questions_from_subagents_it_interrupted() {
        let next = run(vec![
            (0, main(start("t", "Agent"))),
            (1, sub("a", started())),
            (2, sub("a", asked("Bash: ls"))),
            (3, sub("b", launched("u"))),
            (4, sub("b", asked("Bash: pwd"))),
            (9, main(user("never mind"))),
        ]);
        assert_eq!(
            questions(&next),
            ["Bash: pwd"],
            "the background one still waits"
        );
    }

    #[test]
    fn a_turn_left_waiting_on_background_subagents_is_delegating_until_they_report_back() {
        let delegating = run(vec![
            (0, main(user("review it"))),
            (1, main(start("t", "Agent"))),
            (2, sub("a", launched("t"))),
            (3, sub("a", started())),
            (4, main(finished(&["a"]))),
            (60, sub("a", start("x", "Read"))),
        ]);
        assert_eq!(delegating.state, State::Delegating);
        let subagent = &delegating.subagents[&AgentId::new("a")];
        assert_eq!(
            (
                subagent.launch,
                subagent.description.as_deref(),
                subagent.since
            ),
            (Launch::Background, Some("review"), at(2)),
            "a start after the launch keeps when it began"
        );
        let handed_back = run(vec![
            (0, main(user("review it"))),
            (1, sub("a", launched("t"))),
            (3, main(finished(&["a"]))),
            (90, main(MainEvent::Prompted(Input::Notification))),
            (91, sub("a", SubagentEvent::Stopped)),
        ]);
        assert_eq!(
            handed_back.prompt.as_deref(),
            Some("review it"),
            "a notification isn't the user's prompt"
        );
        assert_eq!(handed_back.running(), 0);
        assert!(
            !handed_back.subagents[&AgentId::new("a")].is_running(),
            "the finished one stays until the next prompt"
        );
        let done = Activity::next(Some(handed_back), main(finished(&[])), at(95));
        assert_eq!(done.state, State::Done { summary: None });
    }

    #[test]
    fn a_subagent_asking_after_the_turn_stays_delegating() {
        let asking = run(vec![
            (0, sub("a", launched("t"))),
            (1, main(finished(&["a"]))),
            (2, sub("a", start("x", "Bash"))),
            (5, sub("a", asked("Bash: cargo test"))),
        ]);
        assert_eq!(
            (asking.state.clone(), questions(&asking)),
            (State::Delegating, vec!["Bash: cargo test"])
        );
        let answered = Activity::next(Some(asking), sub("a", end("x")), at(9));
        assert_eq!((answered.state, answered.since), (State::Delegating, at(1)));
    }

    #[test]
    fn a_subagent_that_stops_takes_its_question_with_it() {
        let gone = run(vec![
            (0, sub("a", started())),
            (1, sub("a", asked("Bash: ls"))),
            (2, sub("a", SubagentEvent::Stopped)),
        ]);
        assert_eq!(questions(&gone), [""; 0]);
    }

    #[test]
    fn a_foreground_subagent_keeps_its_tool_running() {
        let working = run(vec![
            (0, main(start("t", "Agent"))),
            (1, sub("a", started())),
            (700, sub("a", start("x", "Bash"))),
        ]);
        assert_eq!(
            working.subagents[&AgentId::new("a")].launch,
            Launch::Foreground
        );
        assert!(!working.stale(at(2000)), "the Agent tool is still running");
        let back = run(vec![
            (0, main(start("t", "Agent"))),
            (1, sub("a", started())),
            (5, sub("a", SubagentEvent::Stopped)),
            (6, main(end("t"))),
        ]);
        assert_eq!(back.running(), 0);
        let next = Activity::next(
            Some(back),
            Reported::main(MainEvent::Prompted(Input::User("again".into()))),
            at(6),
        );
        assert_eq!(
            next.subagents,
            BTreeMap::new(),
            "the next prompt clears them"
        );
    }

    #[test]
    fn an_interrupted_turns_foreground_subagents_are_dropped_by_the_next_prompt() {
        let next = run(vec![
            (0, main(start("t", "Agent"))),
            (1, sub("a", started())),
            (2, sub("b", launched("u"))),
            (9, main(user("try again"))),
        ]);
        assert_eq!(
            next.subagents.keys().collect::<Vec<_>>(),
            [&AgentId::new("b")]
        );
    }

    #[test]
    fn delegating_goes_stale_only_when_no_subagent_is_running_a_tool() {
        let quiet = run(vec![
            (0, sub("a", launched("t"))),
            (1, main(finished(&["a"]))),
        ]);
        assert!(!quiet.stale(at(200)));
        assert!(
            quiet.stale(at(400)),
            "interrupted, or its subagents died with the session"
        );
        let building = run(vec![
            (0, sub("a", launched("t"))),
            (1, main(finished(&["a"]))),
            (2, sub("a", start("x", "Bash"))),
        ]);
        assert!(
            !building.stale(at(3600)),
            "a subagent's long build isn't stuck"
        );
    }

    #[test]
    fn a_new_or_ended_session_forgets_its_subagents_and_questions() {
        let resumed = run(vec![
            (0, sub("a", launched("t"))),
            (1, main(finished(&["a"]))),
            (2, sub("a", asked("Bash: ls"))),
            (9, main(MainEvent::Started)),
        ]);
        assert!(resumed.subagents.is_empty() && resumed.asks.is_empty());
        let stopped = run(vec![(0, sub("a", launched("t"))), (9, main(finished(&[])))]);
        assert!(
            stopped.subagents.is_empty(),
            "the turn's end says what's still running"
        );
    }

    #[test]
    fn compacting_keeps_what_it_was_doing() {
        let compacting = run(vec![
            (0, main(start("t", "Read"))),
            (5, main(MainEvent::Compacted)),
        ]);
        assert_eq!(
            compacting.current_tool().map(|t| t.name.as_str()),
            Some("Read")
        );
        assert_eq!((compacting.heard, compacting.events), (at(5), 2));
    }

    #[test]
    fn seeing_only_clears_done() {
        let asking = run(vec![(0, main(asked("run rm?")))]).acknowledged(at(5));
        assert_eq!(questions(&asking), ["run rm?"]);
    }

    #[test]
    fn only_an_idle_wait_without_a_tool_goes_stale() {
        let thinking = run(vec![(0, main(user("go")))]);
        assert!(!thinking.stale(at(120)));
        assert!(thinking.stale(at(400)));
        let building = run(vec![(0, main(start("t", "Bash")))]);
        assert!(!building.stale(at(3600)), "a long build isn't stuck");
    }

    #[test]
    fn files_from_earlier_versions_still_load() {
        let time = r#"{"secs_since_epoch":0,"nanos_since_epoch":0}"#;
        let old = format!(
            r#"{{"state":{{"kind":"asking","question":"run rm?"}},"since":{time},"prompt":null,"heard":{time},"events":3}}"#
        );
        let activity: Activity = serde_json::from_str(&old).expect("json");
        assert_eq!(
            (activity.events, questions(&activity)),
            (3, vec!["run rm?"])
        );
        let working = format!(
            r#"{{"state":{{"kind":"working","tools":[]}},"since":{time},"prompt":"go","heard":{time},"events":1}}"#
        );
        let activity: Activity = serde_json::from_str(&working).expect("json");
        assert!(activity.subagents.is_empty() && activity.asks.is_empty());
        let states = [
            State::Idle,
            State::Working {
                tools: vec![Tool {
                    id: ToolId::new("t"),
                    name: "Bash".into(),
                    target: None,
                    since: at(0),
                }],
            },
            State::Delegating,
            State::Done { summary: None },
            State::Failed {
                reason: "overloaded".into(),
            },
            State::Exited,
        ];
        for state in states {
            let mut activity = run(vec![
                (0, sub("a", started())),
                (1, sub("a", asked("Bash: ls"))),
                (2, main(asked("Edit"))),
            ]);
            activity.state = state;
            let json = serde_json::to_string(&activity).expect("json");
            assert_eq!(
                serde_json::from_str::<Activity>(&json).expect("succeeds"),
                activity,
                "{json}"
            );
        }
    }

    #[test]
    fn text_waits_for_a_report_after_the_session_went_live() {
        let a = run(vec![(10, main(finished(&[])))]);
        assert!(a.ready_for_text(None, at(11)));
        assert!(a.ready_for_text(Some(at(9)), at(11)));
        assert!(
            !a.ready_for_text(Some(at(10)), at(11)),
            "heard at the bound"
        );
        assert!(
            !a.ready_for_text(Some(at(20)), at(30)),
            "a record from an earlier run"
        );
    }

    #[test]
    fn a_launch_at_or_after_the_bound_makes_the_record_its_own_authority() {
        let a = run(vec![(20, main(fresh(7))), (30, main(finished(&[])))]);
        assert!(a.session.launched_at().is_some());
        assert!(a.ready_for_text(Some(at(25)), at(100)));
        assert!(a.ready_for_text(Some(at(20 + LAUNCH_SLACK.as_secs())), at(100)));
        assert!(
            !a.ready_for_text(Some(at(500)), at(900)),
            "a launch far before the bound doesn't count"
        );
        let a = run(vec![(10, main(finished(&[])))]);
        assert!(a.session.launched_at().is_none());
        assert!(!a.ready_for_text(Some(at(20)), at(1000)), "no lapse");
        assert!(!a.bound_met(Some(at(20))));
    }

    #[test]
    fn a_session_that_only_just_started_gets_time_to_show_a_dialog() {
        let a = run(vec![
            (10, main(finished(&[]))),
            (20, main(MainEvent::Started)),
        ]);
        assert!(a.at_rest() && a.session.is_booting());
        assert!(!a.ready_for_text(Some(at(15)), at(21)));
        assert!(a.ready_for_text(Some(at(15)), at(20 + BOOT_QUIET.as_secs())));
        let a = run(vec![
            (20, main(MainEvent::Started)),
            (21, main(user("go"))),
            (22, main(finished(&[]))),
        ]);
        assert!(!a.session.is_booting());
        assert!(a.ready_for_text(Some(at(15)), at(23)));
    }

    #[test]
    fn a_launched_agent_is_busy_until_its_session_has_been_quiet() {
        let a = run(vec![(10, main(finished(&[]))), (20, main(fresh(7)))]);
        assert!(!a.at_rest() && a.session.is_booting());
        assert!(!a.ready_for_text(Some(at(15)), at(100)));
        let a = run(vec![
            (10, main(finished(&[]))),
            (20, main(fresh(7))),
            (21, main(MainEvent::Started)),
        ]);
        assert!(a.at_rest() && a.session.is_booting());
        assert!(!a.ready_for_text(Some(at(15)), at(22)));
        assert!(a.ready_for_text(Some(at(15)), at(21 + BOOT_QUIET.as_secs())));
        let a = run(vec![(10, main(finished(&[]))), (20, main(fresh(7)))]);
        assert!(a.asks.is_empty() && a.subagents.is_empty());
    }

    #[test]
    fn reports_from_a_replaced_process_are_ignored_after_a_launch() {
        let from = |pids: &[u32], event| {
            main(event).from_lineage(Some(pids.iter().filter_map(|p| Pid::new(*p)).collect()))
        };
        let a = run(vec![
            (10, main(finished(&[]))),
            (20, main(fresh(7))),
            (21, from(&[99, 98], finished(&[]))),
        ]);
        assert!(!a.at_rest() && a.session.is_booting());
        assert_eq!(a.heard, at(20));
        let a = run(vec![
            (20, main(fresh(7))),
            (21, from(&[50, 7, 3], MainEvent::Started)),
        ]);
        assert!(a.at_rest());
    }

    #[test]
    fn untraceable_or_unrecorded_reports_are_accepted() {
        let a = run(vec![(20, main(fresh(7))), (21, main(MainEvent::Started))]);
        assert!(a.at_rest(), "no lineage");
        let a = run(vec![
            (20, main(untraced())),
            (
                21,
                main(MainEvent::Started).from_lineage(Some(vec![pid(99)])),
            ),
        ]);
        assert!(a.at_rest(), "reattached session");
        let a = run(vec![(
            21,
            main(MainEvent::Started).from_lineage(Some(vec![pid(99)])),
        )]);
        assert!(a.at_rest(), "old record");
    }

    #[test]
    fn only_a_finished_turn_with_nothing_running_is_at_rest() {
        assert!(run(vec![(0, main(finished(&[])))]).at_rest());
        assert!(!run(vec![(0, main(user("go")))]).at_rest(), "working");
        assert!(
            !run(vec![
                (0, sub("a", launched("t"))),
                (1, main(finished(&["a"])))
            ])
            .at_rest(),
            "delegating"
        );
        assert!(
            !run(vec![
                (0, main(finished(&[]))),
                (1, main(asked("Which DB?")))
            ])
            .at_rest(),
            "asking"
        );
    }

    #[test]
    fn a_launch_vouches_only_for_its_own_boot() {
        let launched = run(vec![(0, main(untraced()))]);
        assert_eq!(launched.session.launched_at(), Some(at(0)));
        let started = Activity::next(Some(launched.clone()), main(MainEvent::Started), at(1));
        assert_eq!(
            started.session.launched_at(),
            None,
            "its session has reported, so heard takes over"
        );
        let ended = Activity::next(Some(launched), main(MainEvent::Ended), at(2));
        assert_eq!(
            ended.session.launched_at(),
            None,
            "a gone session can't vouch for the next"
        );
    }

    #[test]
    fn a_tool_starting_after_its_subagent_stopped_isnt_running() {
        let a = run(vec![
            (0, sub("a", started())),
            (1, sub("a", SubagentEvent::Stopped)),
            (2, sub("a", start("x", "Bash"))),
        ]);
        let subagent = &a.subagents[&AgentId::new("a")];
        assert_eq!(
            (subagent.status.clone(), subagent.tools().len()),
            (SubagentStatus::Finished { at: at(1) }, 0)
        );
        assert!(!a.stale(at(3)) && a.running() == 0);
        let working = run(vec![(0, main(user("go")))]);
        assert!(working.stale(at(400)), "no subagent keeps it busy");
        let a = run(vec![
            (0, main(user("go"))),
            (1, sub("a", launched("t"))),
            (2, sub("a", SubagentEvent::Stopped)),
            (3, sub("a", start("x", "Bash"))),
        ]);
        assert!(a.stale(at(400)), "a stopped subagent's tool can't hold it");
    }

    fn old_file(extra: &str) -> Activity {
        let time = r#"{"secs_since_epoch":20,"nanos_since_epoch":0}"#;
        let json = format!(
            r#"{{"state":{{"kind":"idle"}},"since":{time},"prompt":null,"heard":{time},"events":1{extra}}}"#
        );
        serde_json::from_str(&json).expect("json")
    }

    #[test]
    fn sessions_from_the_separate_fields_still_load() {
        let time = r#"{"secs_since_epoch":20,"nanos_since_epoch":0}"#;
        assert_eq!(
            old_file("").session,
            Session::Live {
                join: Join::Untraced
            }
        );
        assert_eq!(
            old_file(r#","launcher":7"#).session,
            Session::Live {
                join: Join::Fresh(pid(7))
            }
        );
        assert_eq!(
            old_file(r#","booting":true,"launcher":7"#).session,
            Session::Booting {
                join: Join::Fresh(pid(7))
            }
        );
        assert_eq!(
            old_file(&format!(
                r#","booting":true,"launcher":7,"launched":{time}"#
            ))
            .session,
            Session::Launched {
                join: Join::Fresh(pid(7)),
                at: at(20)
            }
        );
        assert_eq!(
            old_file(&format!(r#","booting":false,"launched":{time}"#)).session,
            Session::LaunchReported {
                join: Join::Untraced,
                at: at(20)
            }
        );
        assert_eq!(
            old_file(r#","launcher":0"#).session,
            Session::Live {
                join: Join::Untraced
            },
            "zero was never a process"
        );
    }

    #[test]
    fn subagents_from_the_separate_fields_still_load() {
        let time = r#"{"secs_since_epoch":20,"nanos_since_epoch":0}"#;
        let tool = format!(r#"{{"id":"x","name":"Bash","target":null,"since":{time}}}"#);
        let subagent = |extra: &str| {
            let a = old_file(&format!(
                r#","subagents":{{"a":{{"kind":"Explore","description":null,"launch":"background","since":{time}{extra}}}}}"#
            ));
            a.subagents[&AgentId::new("a")].clone()
        };
        let running = subagent(&format!(r#","tools":[{tool}]"#));
        assert_eq!((running.is_running(), running.tools().len()), (true, 1));
        let finished = subagent(&format!(r#","tools":[],"finished":{time}"#));
        assert_eq!(finished.status, SubagentStatus::Finished { at: at(20) });
        let bare = subagent("");
        assert!(bare.is_running() && bare.tools().is_empty());
    }

    #[test]
    fn the_session_survives_a_round_trip() {
        let a = run(vec![
            (0, main(fresh(7))),
            (1, sub("a", launched("t"))),
            (2, sub("a", SubagentEvent::Stopped)),
        ]);
        let json = serde_json::to_string(&a).expect("json");
        assert_eq!(serde_json::from_str::<Activity>(&json).expect("json"), a);
    }

    #[test]
    fn an_older_reader_sees_a_booting_session_as_booting() {
        #[derive(Deserialize)]
        struct Before {
            #[serde(default)]
            booting: bool,
            #[serde(default)]
            launcher: Option<u32>,
            #[serde(default)]
            launched: Option<SystemTime>,
        }
        let read = |a: &Activity| {
            let before: Before =
                serde_json::from_str(&serde_json::to_string(a).expect("json")).expect("json");
            (before.booting, before.launcher, before.launched)
        };
        let launched = run(vec![(20, main(fresh(7)))]);
        assert_eq!(read(&launched), (true, Some(7), Some(at(20))));
        let started = Activity::next(Some(launched), main(MainEvent::Started), at(21));
        assert_eq!(read(&started), (true, Some(7), None));
        let live = Activity::next(Some(started), main(user("go")), at(22));
        assert_eq!(read(&live), (false, Some(7), None));
    }

    #[test]
    fn a_report_other_than_starting_ends_the_boot_but_not_the_launch() {
        let a = run(vec![(20, main(untraced())), (21, main(user("go")))]);
        assert_eq!(
            a.session,
            Session::LaunchReported {
                join: Join::Untraced,
                at: at(20)
            }
        );
        assert!(a.bound_met(Some(at(25))) && !a.session.is_booting());
    }
}
